//! Shared viewer routing, AOI indexes and Relay queue lifetime.
//!
//! Protocol Families declare their table callbacks beside their codecs. Callbacks update indexes
//! and enqueue owned rows; the World Session writer builds packets. Arming registers callbacks
//! before reconciling the cache on the same Shard pump.

use crate::codec::entity::subscriptions::{entity_key, offer_create_job};
use crate::codec::gameobject::subscriptions::gameobject_appeared;
use crate::codec::social::subscriptions::{
    replace_contact_sets, resident_instance_removal_outbound,
};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};

use anyhow::{anyhow, Result};
use lyracore_shared::spatial::{BOX_HALF_SPAN, GRID_CELL_SIZE};
use spacetimedb_sdk::Table;

use super::aoi::{ViewerGates, AOI_RECENTERS};
use super::bindings::*;
use super::connection::Coordinator;
use super::world_index::{CellKey, EntityLayer, SessionId, ShardId, WorldIndex};
use crate::world::{Outbound, SessionTx};

/// Everything the dispatch needs to know about one live world session. Registered at world entry
/// (`subscribe_player_events`) and dropped by `PlayerSubscriptions`'s guard.
///
/// Every field here is session relay state used by the shared callbacks' writer jobs.
pub(crate) struct Viewer {
    /// Cleared when this World Session leaves. Queued Relays must not reach its replacement.
    pub(crate) active: std::sync::atomic::AtomicBool,
    pub(crate) session: SessionId,
    pub(crate) self_guid: u64,
    /// The account identity stamped on identity-addressed event rows.
    pub(crate) bound_identity: spacetimedb_sdk::Identity,
    /// The map this session is on. Constant for the session's lifetime — every path that changes
    /// a character's map (cross-map teleport, portal entry) tears the subscriptions
    /// down and calls `subscribe_player_events` again, which builds a fresh viewer.
    pub(crate) map_id: u32,
    /// The partition this session is playing in — supplied by the caller (login row / transfer
    /// plan), never re-read from a cache. The lesson: the authoritative value is known at
    /// construction, and a second possibly-stale copy can only disagree with it.
    pub(crate) instance_id: u64,
    /// The zone this session is standing in — the weather relay's whole audience predicate.
    ///
    /// Unlike `map_id` this changes inside one session: walking, a same-map teleport and a taxi
    /// landing all cross zone boundaries without tearing the viewer down, so it is interior-mutable
    /// and written from the self entity's own `zone_id` deltas. `0` is the unresolved zone and
    /// matches no weather row. Seeded at `add_viewer` time with the zone world entry already sent
    /// weather for, so the first stored value is never a phantom crossing.
    pub(crate) zone_id: AtomicU32,
    pub(crate) tx: SessionTx,
    /// WorldEntity GUIDs already shown to this viewer.
    pub(crate) created: Arc<Mutex<HashSet<u64>>>,
    pub(crate) gates: Arc<ViewerGates>,
    /// PLAYER_SKILL_INFO slot map `(skill_line → slot, next_free)` — seeded at login from the same
    /// deterministic layout the CREATE used.
    pub(crate) skill_slots: Arc<Mutex<(HashMap<u32, u8>, u8)>>,
    /// Areas known before this viewer entered the world. Reconnect replays use the same set, so
    /// restoring fog never repeats the discovery feedback.
    pub(crate) explored: Mutex<ExplorationReplay>,
    /// This viewer's motion-coalescing buffer. Shared-dispatch-only state (no per-player
    /// twin), so it is constructed inline and pinned by no tripwire.
    pub(crate) motion_pending: Arc<MotionPending>,
    /// What the Member Stats Relay last sent about each group mate. A new viewer starts empty, so
    /// world entry and a group join both get every field on the next tick.
    pub(crate) member_stats: crate::world::MemberStatsRecord,
    /// The Characters on this viewer's own ignore list. Seeded at world entry from its contact
    /// rows on the Home Shard, kept current by that Shard's contact Relay, and rebuilt when the
    /// Shard reconciles after a resubscribe. The Realm Chat Relay reads it for ignorable lines only.
    pub(crate) ignored: Mutex<HashSet<u64>>,
    /// The Characters on this viewer's own friend list. Seeded and kept current exactly like
    /// `ignored`, from the same contact rows and callbacks. The Account Claim Relay reads it to
    /// decide who hears a login/logout notice; `CMSG_FRIEND_LIST` reads it for the friend guids.
    pub(crate) friends: Mutex<HashSet<u64>>,
    /// This viewer's own team (`lyracore_shared::faction::TEAM_ALLIANCE`/`TEAM_HORDE`), read once
    /// at world entry from its race. A friend online/offline notice goes only to a same-team
    /// viewer, the faction separation the friends pane has in vanilla.
    pub(crate) team: u32,
    /// The group events already relayed to this viewer, so one transaction's rows go out once
    /// and in order.
    pub(crate) group_events: GroupEventCursor,
}

impl Viewer {
    /// Move this viewer's weather routing into `zone_id`, answering whether that was a real
    /// crossing — the ONE place [`Viewer::zone_id`] is written after construction.
    ///
    /// The unresolved zone (`0`) is not a crossing and never overwrites a known zone: it means the
    /// Module's terrain could not answer, not that the player left every zone, and treating it as a
    /// destination would send fine weather over a real storm.
    pub(crate) fn enter_zone(&self, zone_id: u32) -> bool {
        zone_id != 0 && self.zone_id.swap(zone_id, Ordering::Relaxed) != zone_id
    }

    /// Whether this viewer ignores `guid`.
    pub(crate) fn ignores(&self, guid: u64) -> bool {
        self.ignored
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .contains(&guid)
    }

    pub(crate) fn set_ignored(&self, guid: u64, ignored: bool) {
        let mut set = self.ignored.lock().unwrap_or_else(|p| p.into_inner());
        if ignored {
            set.insert(guid);
        } else {
            set.remove(&guid);
        }
    }

    /// Whether this viewer lists `guid` as a friend.
    pub(crate) fn is_friend(&self, guid: u64) -> bool {
        self.friends
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .contains(&guid)
    }

    pub(crate) fn set_friend(&self, guid: u64, present: bool) {
        let mut set = self.friends.lock().unwrap_or_else(|p| p.into_inner());
        if present {
            set.insert(guid);
        } else {
            set.remove(&guid);
        }
    }

    /// A snapshot of this viewer's ignore guids, for `CMSG_FRIEND_LIST`'s `SMSG_IGNORE_LIST`.
    pub(crate) fn ignored_guids(&self) -> Vec<u64> {
        self.ignored
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .iter()
            .copied()
            .collect()
    }

    /// A snapshot of this viewer's friend guids, for `CMSG_FRIEND_LIST`'s `SMSG_FRIEND_LIST`.
    pub(crate) fn friend_guids(&self) -> Vec<u64> {
        self.friends
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .iter()
            .copied()
            .collect()
    }
}

/// Per-viewer exploration replay state. `area_bit` is stable across a transfer's re-minted row
/// ids, so it identifies discoveries rather than the table's surrogate key.
#[derive(Default)]
pub(crate) struct ExplorationReplay {
    announced: HashSet<i32>,
}

impl ExplorationReplay {
    pub(crate) fn seed(&mut self, area_bits: impl IntoIterator<Item = i32>) {
        self.announced.extend(area_bits);
    }

    pub(crate) fn admit_once(&mut self, area_bit: i32) -> bool {
        self.announced.insert(area_bit)
    }
}

/// Dense-crowd coalescing: the pump UPSERTS the latest motion row per mover here and queues
/// at most ONE flush job per viewer; a superseded heartbeat dies in the map instead of living as a
/// queued job. A viewer needs only the LATEST position of each mover per flush — with N movers in
/// a cell the per-viewer queue cost is now O(distinct movers), not O(heartbeats), which is the
/// difference between a packed cell starving logins and not (the measured dense-500 numbers).
#[derive(Default)]
pub(crate) struct MotionPending {
    pub(crate) entity: Mutex<HashMap<u64, Arc<EntityMotion>>>,
    pub(crate) creature: Mutex<HashMap<u64, Arc<CreatureSpline>>>,
    /// True while a flush job sits in the viewer's queue. Cleared by the flush BEFORE it drains,
    /// so an upsert racing the drain re-queues a (possibly empty, harmless) follow-up flush
    /// rather than being stranded.
    pub(crate) flush_queued: std::sync::atomic::AtomicBool,
}

struct RegisteredViewer {
    viewer: Arc<Viewer>,
    shard: ShardId,
}

/// A viewer address carried by owner-GUID-scoped rows.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct OwnerGuid(pub(crate) u64);

/// A viewer address carried by identity-scoped rows.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct BoundIdentity(pub(crate) spacetimedb_sdk::Identity);

#[derive(Default)]
struct ViewerRegistry {
    by_session: HashMap<SessionId, RegisteredViewer>,
    session_of_owner: HashMap<u64, SessionId>,
    session_of_identity: HashMap<spacetimedb_sdk::Identity, SessionId>,
    sessions_by_shard: HashMap<ShardId, HashSet<SessionId>>,
}

impl ViewerRegistry {
    fn register(&mut self, viewer: Arc<Viewer>, shard: ShardId) {
        let session = viewer.session;
        if let Some(previous) = self.by_session.remove(&session) {
            previous.viewer.active.store(false, Ordering::Release);
            self.remove_indexes(session, &previous);
        }
        self.session_of_owner.insert(viewer.self_guid, session);
        self.session_of_identity
            .insert(viewer.bound_identity, session);
        self.sessions_by_shard
            .entry(shard)
            .or_default()
            .insert(session);
        self.by_session
            .insert(session, RegisteredViewer { viewer, shard });
    }

    pub(crate) fn remove(&mut self, session: SessionId) {
        let Some(registered) = self.by_session.remove(&session) else {
            return;
        };
        registered.viewer.active.store(false, Ordering::Release);
        self.remove_indexes(session, &registered);
    }

    fn remove_indexes(&mut self, session: SessionId, registered: &RegisteredViewer) {
        let owner = registered.viewer.self_guid;
        if self.session_of_owner.get(&owner) == Some(&session) {
            self.session_of_owner.remove(&owner);
        }
        let identity = registered.viewer.bound_identity;
        if self.session_of_identity.get(&identity) == Some(&session) {
            self.session_of_identity.remove(&identity);
        }
        if let Some(sessions) = self.sessions_by_shard.get_mut(&registered.shard) {
            sessions.remove(&session);
            if sessions.is_empty() {
                self.sessions_by_shard.remove(&registered.shard);
            }
        }
    }
}

/// Auras indexed by shard and target. Transfer copies on different shards remain independent.
#[derive(Default)]
pub(crate) struct AuraIndex {
    by_target: Mutex<HashMap<(ShardId, u64), Vec<Aura>>>,
}

impl AuraIndex {
    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<(ShardId, u64), Vec<Aura>>> {
        self.by_target.lock().unwrap_or_else(|p| p.into_inner())
    }

    pub(crate) fn upsert(&self, shard: ShardId, row: &Aura) {
        let mut by_target = self.lock();
        let rows = by_target.entry((shard, row.target_guid)).or_default();
        match rows.iter_mut().find(|a| a.id == row.id) {
            Some(slot) => *slot = row.clone(),
            None => rows.push(row.clone()),
        }
    }

    pub(crate) fn remove(&self, shard: ShardId, row: &Aura) {
        let mut by_target = self.lock();
        let key = (shard, row.target_guid);
        if let Some(rows) = by_target.get_mut(&key) {
            rows.retain(|a| a.id != row.id);
            if rows.is_empty() {
                by_target.remove(&key);
            }
        }
    }

    /// Replace the Shard snapshot and return previous auras grouped by every old or current target.
    /// A newly present target has an empty previous set. Each target needs one snapshot Relay.
    pub(crate) fn replace_shard(
        &self,
        shard: ShardId,
        rows: impl IntoIterator<Item = Aura>,
    ) -> HashMap<u64, Vec<Aura>> {
        let mut by_target = self.lock();
        let targets: Vec<_> = by_target
            .keys()
            .filter(|(owner, _)| *owner == shard)
            .copied()
            .collect();
        let mut old: HashMap<_, _> = targets
            .into_iter()
            .map(|target| (target.1, by_target.remove(&target).unwrap_or_default()))
            .collect();
        for row in rows {
            old.entry(row.target_guid).or_default();
            by_target
                .entry((shard, row.target_guid))
                .or_default()
                .push(row);
        }
        old
    }

    pub(crate) fn on_target(&self, shard: ShardId, target_guid: u64) -> Vec<Aura> {
        self.lock()
            .get(&(shard, target_guid))
            .cloned()
            .unwrap_or_default()
    }

    pub(crate) fn stealth_count(&self, shard: ShardId, target_guid: u64) -> usize {
        self.lock()
            .get(&(shard, target_guid))
            .map(|rows| {
                rows.iter()
                    .filter(|a| a.eff_kind == crate::codec::combat::subscriptions::A_STEALTH)
                    .count()
            })
            .unwrap_or(0)
    }

    pub(crate) fn is_stealthed(&self, shard: ShardId, target_guid: u64) -> bool {
        self.stealth_count(shard, target_guid) > 0
    }

    pub(crate) fn stats(&self) -> (usize, usize) {
        let by_target = self.lock();
        (by_target.values().map(Vec::len).sum(), by_target.len())
    }
}

/// The gateway-wide shared view: one layered spatial index, the aura index, the live viewer
/// registry, and the shard table the index's [`ShardId`]s point into.
pub(crate) struct WorldView {
    /// One authoritative viewer anchor with independent world-entity and game-object layers.
    pub(crate) spatial: WorldIndex,
    pub(crate) auras: AuraIndex,
    viewers: RwLock<ViewerRegistry>,
    /// [`ShardId`] → that shard's coordinator handle. Written once at startup, read on every
    /// dispatch; the order fixes the ids, so it must never be reordered after arming.
    shards: RwLock<Vec<Coordinator>>,
    next_session: AtomicU64,
    /// Characters whose client was last sent a running Instance Removal countdown. It outlives the
    /// viewer: a world-port drops the source viewer before the source Shard deletes the row, so
    /// the next world entry must hide the countdown itself.
    countdowns_shown: Mutex<HashSet<u64>>,
    /// Characters this Gateway has itself watched the `game_character.online` false→true edge
    /// for (`character_online_changed`), not yet paired with a later Account Claim close. The
    /// friend-notice dedup state that lets a claim close (`account_claim_changed`) answer
    /// FRIEND_OFFLINE only for a Character THIS Gateway confirmed came online — never for a claim
    /// that opened and closed without `player_login` ever running, which happens whenever
    /// `enter_world` refuses after `claim_session` already succeeded (`CMSG_PLAYER_LOGIN`'s
    /// handler runs them in that order). A Gateway that starts, or reconnects a Shard, after a
    /// Character already logged in has no mark for it and answers nothing for that Character's
    /// NEXT close — a known gap, not a correctness hazard: the friends pane itself still reads
    /// live Realm Presence, so it is never wrong, only occasionally a push notification behind.
    online_characters: Mutex<HashSet<u64>>,
}

impl WorldView {
    pub(crate) fn new(cell_scoped: bool) -> Self {
        Self {
            spatial: WorldIndex::new(cell_scoped),
            auras: AuraIndex::default(),
            viewers: RwLock::new(ViewerRegistry::default()),
            shards: RwLock::new(Vec::new()),
            next_session: AtomicU64::new(1),
            countdowns_shown: Mutex::new(HashSet::new()),
            online_characters: Mutex::new(HashSet::new()),
        }
    }

    /// Record whether `character_guid`'s client now shows an Instance Removal countdown.
    pub(crate) fn note_countdown(&self, character_guid: u64, shown: bool) {
        let mut countdowns = self.countdowns_shown.lock().unwrap();
        if shown {
            countdowns.insert(character_guid);
        } else {
            countdowns.remove(&character_guid);
        }
    }

    pub(crate) fn countdown_shown(&self, character_guid: u64) -> bool {
        self.countdowns_shown
            .lock()
            .unwrap()
            .contains(&character_guid)
    }

    /// Record that `guid` reached the `game_character.online` transition — the one signal a
    /// later Account Claim close checks before it answers FRIEND_OFFLINE.
    pub(crate) fn mark_character_online(&self, guid: u64) {
        self.online_characters
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(guid);
    }

    /// Consume `guid`'s online mark, if this Gateway ever recorded one. `true` only for a
    /// Character this Gateway itself watched come online.
    pub(crate) fn take_character_online(&self, guid: u64) -> bool {
        self.online_characters
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(&guid)
    }

    pub(crate) fn viewer(&self, session: SessionId) -> Option<Arc<Viewer>> {
        self.viewers
            .read()
            .unwrap()
            .by_session
            .get(&session)
            .map(|registered| registered.viewer.clone())
    }

    pub(crate) fn session_of_owner(&self, self_guid: u64) -> Option<SessionId> {
        self.viewers
            .read()
            .unwrap()
            .session_of_owner
            .get(&self_guid)
            .copied()
    }

    /// The owner's viewer regardless of shard — for a caller that only knows "my own session",
    /// never a peer's (a shard-scoped row Relay resolves through
    /// [`Self::viewer_of_owner_on_shard`] instead, so a transfer's source shard cannot address the
    /// viewer after it re-registers on the destination shard). `CMSG_FRIEND_LIST`'s own contact
    /// read is the production caller: it only ever asks for the CONNECTED session's own guid.
    pub(crate) fn viewer_of_owner(&self, owner: OwnerGuid) -> Option<Arc<Viewer>> {
        let registry = self.viewers.read().unwrap();
        let session = registry.session_of_owner.get(&owner.0)?;
        registry
            .by_session
            .get(session)
            .map(|registered| registered.viewer.clone())
    }

    /// The owner's viewer, only when it is registered on `shard`. Shard-row owner relays
    /// (teleport/quest/exploration/reputation/item) resolve through this so a transfer's source
    /// shard cannot address the viewer after it re-registered on the destination shard.
    pub(crate) fn viewer_of_owner_on_shard(
        &self,
        shard: ShardId,
        owner: OwnerGuid,
    ) -> Option<Arc<Viewer>> {
        let registry = self.viewers.read().unwrap();
        let session = registry.session_of_owner.get(&owner.0)?;
        let registered = registry.by_session.get(session)?;
        (registered.shard == shard).then(|| registered.viewer.clone())
    }

    pub(crate) fn viewer_of_identity_on_shard(
        &self,
        shard: ShardId,
        identity: BoundIdentity,
    ) -> Option<Arc<Viewer>> {
        let registry = self.viewers.read().unwrap();
        let session = registry.session_of_identity.get(&identity.0)?;
        let registered = registry.by_session.get(session)?;
        (registered.shard == shard).then(|| registered.viewer.clone())
    }

    #[cfg(test)]
    pub(crate) fn viewer_of_identity(&self, identity: BoundIdentity) -> Option<Arc<Viewer>> {
        let registry = self.viewers.read().unwrap();
        let session = registry.session_of_identity.get(&identity.0)?;
        registry
            .by_session
            .get(session)
            .map(|registered| registered.viewer.clone())
    }

    pub(crate) fn viewers_on_shard(&self, shard: ShardId) -> Vec<Arc<Viewer>> {
        let registry = self.viewers.read().unwrap();
        registry
            .sessions_by_shard
            .get(&shard)
            .into_iter()
            .flatten()
            .filter_map(|session| {
                registry
                    .by_session
                    .get(session)
                    .map(|registered| registered.viewer.clone())
            })
            .collect()
    }

    /// Every registered viewer on every shard, for a Relay that visits each World Session.
    pub(crate) fn all_viewers(&self) -> Vec<Arc<Viewer>> {
        let registry = self.viewers.read().unwrap();
        registry
            .by_session
            .values()
            .map(|registered| registered.viewer.clone())
            .collect()
    }

    pub(crate) fn shard(&self, id: ShardId) -> Option<Coordinator> {
        self.shards.read().unwrap().get(id).cloned()
    }

    /// The Shard `session`'s viewer is registered on.
    pub(crate) fn shard_of_viewer(&self, session: SessionId) -> Option<ShardId> {
        let registry = self.viewers.read().unwrap();
        registry
            .by_session
            .get(&session)
            .map(|registered| registered.shard)
    }

    /// Mint a session id. Monotonic and never reused, so a relogin can never inherit a previous
    /// session's interest set.
    pub(crate) fn next_session_id(&self) -> SessionId {
        self.next_session.fetch_add(1, Ordering::Relaxed)
    }

    /// Register a live session on the shard owned by `coord`. The coordinator must be one of the
    /// stable handles installed by [`Self::set_shards`].
    pub(crate) fn add_viewer(
        &self,
        coord: &Coordinator,
        viewer: Arc<Viewer>,
        anchor: CellKey,
    ) -> Result<()> {
        let shards = self.shards.read().unwrap();
        let shard = shards
.iter()
.position(|registered| registered.shard_name() == coord.shard_name())
.ok_or_else(|| {
                anyhow!(
                    "cannot register viewer {}: coordinator shard `{}` is absent from the world-view shard table ({:?})",
                    viewer.session,
                    coord.shard_name(),
                    shards.iter().map(|known| known.shard_name()).collect::<Vec<_>>()
                )
            })?;
        drop(shards);
        self.add_viewer_on_shard(viewer, anchor, shard);
        Ok(())
    }

    pub(crate) fn add_viewer_on_shard(&self, viewer: Arc<Viewer>, anchor: CellKey, shard: ShardId) {
        let mut registry = self.viewers.write().unwrap();
        self.spatial.add_viewer(viewer.session, anchor);
        registry.register(viewer, shard);
    }

    pub(crate) fn remove_viewer(&self, session: SessionId) {
        let mut registry = self.viewers.write().unwrap();
        registry.remove(session);
        self.spatial.remove_viewer(session);
    }

    /// Geometric recipients plus the row owner's session. The owner lookup lives in the viewer
    /// registry because it is session identity, not spatial state.
    pub(crate) fn world_entity_recipients(
        &self,
        shard: ShardId,
        guid: u64,
        key: CellKey,
    ) -> Vec<SessionId> {
        let mut recipients = self.spatial.viewers_of(EntityLayer::WorldEntity, key);
        if let Some(owner) = self.session_of_owner(guid) {
            if !recipients.contains(&owner) {
                recipients.push(owner);
            }
        }
        let registry = self.viewers.read().unwrap();
        recipients.retain(|session| {
            registry
                .by_session
                .get(session)
                .is_some_and(|v| v.shard == shard)
        });
        recipients
    }

    /// The viewers a cell-anchored event reaches: every session anchored within `half_span` cells
    /// of `key`, plus the sessions of the characters the row names. The owner leg keeps a
    /// self-addressed packet (the attacker's ATTACKSTOP, the caster's cast result, a player's own
    /// aura timer) reaching its owner while the owner's anchor and the row's cell disagree by a
    /// heartbeat. When the source Shard has no anchor, only its owners receive the event.
    /// Every recipient must still belong to that Shard after a transfer.
    ///
    /// The per-viewer gate the job runs is the final filter, unchanged; this only stops the pump
    /// enqueueing a job for every viewer on the shard so it can reject the row later.
    pub(crate) fn cell_audience(
        &self,
        shard: ShardId,
        key: Option<CellKey>,
        half_span: i32,
        owners: &[u64],
    ) -> Vec<Arc<Viewer>> {
        let mut sessions = key
            .map(|key| self.spatial.viewers_within(key, half_span))
            .unwrap_or_default();
        let registry = self.viewers.read().unwrap();
        for owner in owners {
            if let Some(session) = registry.session_of_owner.get(owner) {
                if !sessions.contains(session) {
                    sessions.push(*session);
                }
            }
        }
        sessions
            .into_iter()
            .filter_map(|session| registry.by_session.get(&session))
            .filter(|registered| registered.shard == shard)
            .map(|registered| registered.viewer.clone())
            .collect()
    }

    // ===========================================================================================
    //  Arming — one registration per shard, re-run by the coordinator watchdog after a reconnect.
    // ===========================================================================================

    /// Give this view its shard table. Called once, from `Coordinator::connect`, before arming.
    pub(crate) fn set_shards(&self, shards: Vec<Coordinator>) {
        *self.shards.write().unwrap() = shards;
    }
}

/// Register Relays and reconcile the snapshot between messages on this Shard's pump.
/// Connection replacement joins the old pump before this connection becomes readable.
pub(crate) fn arm_shard(view: Arc<WorldView>, coord: Coordinator, shard: ShardId) -> Result<()> {
    let owner = coord.clone();
    coord.0.on_pump(move |conn| {
        crate::stdb::relays::register_shard(view.clone(), owner, shard, &conn.db);
        seed_shard_from_cache(&view, shard, &conn.db);
    })
}

/// Register the cross-shard private relays on the REALM-CORE connection: group
/// events, Realm Chat Lines (whispers included), Channel Notices and Mail Arrivals written
/// realm-side for recipients whose home shard is elsewhere. Same recipient-keyed dispatchers as
/// `arm_shard`'s private tier, armed ONCE per realm-core connection instead of once per session;
/// the last per-session registrations are gone.
///
/// Only called when realm-core is a DISTINCT database (`Coordinator::connect` gates it): on a
/// single-database gateway the world shard's own `arm_shard` registration already watches these
/// tables, and a second registration would deliver every private packet twice.
///
/// `coord` is a WORLD handle (the default shard) — `group_event_appeared` needs one for the
/// QUEST_SHARE detail JOIN; realm-core's own cache holds no quest catalogue.
///
/// Re-reads `realm.0.coord()` on every call, so it doubles as the watchdog's post-reconnect
/// re-arm hook, exactly like `arm_shard`.
pub(crate) fn arm_realm_private(view: Arc<WorldView>, realm: Coordinator, coord: Coordinator) {
    let guard = realm.0.coord();
    crate::stdb::relays::register_realm(view, realm.clone(), coord, &guard.conn.db);
}

/// Reads one Character's `(guild_id, rank_id)` from the Realm-core cache. A closure so relay
/// tests can supply membership without a connection.
pub(crate) type GuildMembershipRead = Arc<dyn Fn(u64) -> Option<(u32, u32)> + Send + Sync>;

/// Reads the Guild data one settings-changing Guild Event needs from the Realm-core cache:
/// `TABARD_CHANGED` needs the query response, `ROSTER_TO_ACTOR` needs the roster, `ROSTER_REFRESH`
/// needs both. `guild_event_appeared` calls this once per event and shares the result with every
/// recipient's job. A closure so relay tests can supply a snapshot without a live connection.
pub(crate) type GuildRosterSnapshotRead =
    Arc<dyn Fn(u32, u8) -> crate::world::GuildEventSnapshot + Send + Sync>;

/// Reads the open Petition of one Guild Charter from the Realm-core cache. A closure so relay
/// tests can supply a Petition without a connection.
pub(crate) type PetitionRead = Arc<dyn Fn(u64) -> Option<crate::codec::PetitionView> + Send + Sync>;

#[cfg(test)]
fn is_initial_apply(event: &spacetimedb_sdk::Event<Reducer>) -> bool {
    matches!(event, spacetimedb_sdk::Event::SubscribeApplied)
}

/// Every viewer a shard-broadcast family may reach: the sessions whose characters `shard`'s
/// database owns — the same audience the per-player subscriptions produced (each player connection
/// was to its own shard's database, and the base queries were unscoped `SELECT *`s). Filtered by
/// direct shard membership; family-specific gates (instance, range) run per viewer at the call site.
pub(crate) fn viewers_on_shard(view: &WorldView, shard: ShardId) -> Vec<Arc<Viewer>> {
    view.viewers_on_shard(shard)
}

// ===============================================================================================
//  Cell-anchored event dispatch. The Module stamps the actor's cell on the event row (or the
//  actor's own row is in the index), so the audience is the actor's box rather than the shard;
//  each job's per-viewer gate stays the final filter.
// ===============================================================================================

/// Cells a yard range can reach from an anchor cell: the range in cells, rounded up, plus one
/// because the listener's anchor is its client position while the gate reads its last durable
/// row, and the two can sit one cell apart.
pub(crate) fn cells_within(range_yd: f32) -> i32 {
    (range_yd / GRID_CELL_SIZE).ceil() as i32 + 1
}

/// The database a `game_group_event` row came from. Each database numbers its rows itself.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum GroupEventSource {
    Shard(ShardId),
    RealmCore,
}

/// The newest `game_group_event` id relayed to one viewer from each source database.
#[derive(Default)]
pub(crate) struct GroupEventCursor(Mutex<HashMap<GroupEventSource, u64>>);

impl GroupEventCursor {
    /// The rows to relay for the insert callback of `row`, oldest first.
    ///
    /// The SDK fires the inserts of one transaction in hash order, and the client must hear them
    /// in insert order: a filled Meeting Stone party's `COMPLETE` comes before `SETQUEUE(0, NONE)`.
    /// So the first callback of a transaction takes every row of that transaction for this
    /// recipient from `cached`, which reads the table that already holds the whole transaction, and
    /// the later callbacks find theirs taken without calling it. Rows of one transaction share
    /// `created_at`.
    pub(crate) fn take(
        &self,
        source: GroupEventSource,
        row: &GroupEvent,
        cached: impl FnOnce() -> Vec<GroupEvent>,
    ) -> Vec<GroupEvent> {
        let mut cursors = self.0.lock().unwrap_or_else(|p| p.into_inner());
        let cursor = cursors.entry(source).or_default();
        if row.id <= *cursor {
            return Vec::new();
        }
        let mut rows: Vec<GroupEvent> = cached()
            .into_iter()
            .filter(|cached| {
                cached.recipient_guid == row.recipient_guid
                    && cached.created_at == row.created_at
                    && cached.id > *cursor
            })
            .collect();
        if !rows.iter().any(|cached| cached.id == row.id) {
            rows.push(row.clone());
        }
        rows.sort_unstable_by_key(|cached| cached.id);
        *cursor = rows.last().map_or(row.id, |last| last.id);
        rows
    }
}

/// The `game_group_event` table as one source database last cached it, grouped by recipient guid.
/// Every recipient of a transaction takes its rows from the same read.
#[derive(Default)]
pub(crate) struct GroupEventSnapshot(HashMap<u64, Vec<GroupEvent>>);

impl GroupEventSnapshot {
    /// [`GroupEventCursor::take`] over the recipient's rows of the snapshot.
    ///
    /// `read_table` runs only when the recipient's group lacks `row`, which is the first callback
    /// of each transaction. The cache already holds the whole transaction by then, so the later
    /// callbacks reuse the read. A row the table lacks reads it again.
    pub(crate) fn take(
        &mut self,
        cursor: &GroupEventCursor,
        source: GroupEventSource,
        row: &GroupEvent,
        read_table: impl FnOnce() -> Vec<GroupEvent>,
    ) -> Vec<GroupEvent> {
        cursor.take(source, row, || {
            let held = self
                .0
                .get(&row.recipient_guid)
                .is_some_and(|rows| rows.iter().any(|held| held.id == row.id));
            if !held {
                self.0.clear();
                for cached in read_table() {
                    self.0
                        .entry(cached.recipient_guid)
                        .or_default()
                        .push(cached);
                }
            }
            self.0.get(&row.recipient_guid).cloned().unwrap_or_default()
        })
    }
}

/// Every World Session on this Gateway whose Character is a member of `guild_id`. The one
/// audience read for broadcast Guild Events.
pub(crate) fn online_audience(
    view: &WorldView,
    membership: &GuildMembershipRead,
    guild_id: u32,
) -> Vec<Arc<Viewer>> {
    view.all_viewers()
        .into_iter()
        .filter(|viewer| membership(viewer.self_guid).is_some_and(|(id, _)| id == guild_id))
        .collect()
}

// The relay-order tests below scan this file's own source, so the production items that follow
// them must stay where they are.
#[allow(clippy::items_after_test_module)]
#[cfg(test)]
mod cell_audience_tests;

#[cfg(test)]
mod relay_bench;

#[cfg(test)]
mod item_durable_tests;

#[cfg(all(test, unix))]
mod benilla_movement_tests {
    #[test]
    fn benilla_clients_observe_each_others_movement_within_aoi() {
        crate::world::tests::benilla_tests::two_clients_observe_movement(|view, row| {
            crate::codec::movement::subscriptions::motion(view, 0, row, &Default::default());
        });
    }
}

#[allow(clippy::items_after_test_module)]
#[cfg(test)]
mod routing_tests {
    use super::*;
    use crate::codec::addon::subscriptions::addon_message_appeared;
    use crate::codec::leveling::subscriptions::{levelup_appeared, xp_appeared};
    use crate::codec::movement::subscriptions::teleport_appeared;
    use crate::codec::social::subscriptions::private_recipient_audience;
    use crate::stdb::bindings::{AddonMessage, LevelupEvent, TeleportEvent, XpEvent};
    use crate::stdb::world_index::{CellKey, EntityLayer};
    use crate::stdb::world_view::test_support::{
        identity, queued_job, teleport, viewer, viewer_with_tx,
    };
    use crate::world::{Outbound, SessionTx};
    use std::collections::HashSet;
    use std::sync::{Arc, Mutex};
    use wow_world_messages::vanilla::opcodes::ServerOpcodeMessage;

    /// The private tier's whole privacy guarantee (group/resurrect): the addressee and
    /// NOBODY else. The zero cases are the leak vectors — an unset recipient must never match a
    /// real viewer and a real recipient must never match a half-initialized viewer.
    #[test]
    fn private_rows_reach_their_addressee_and_nobody_else() {
        assert!(private_recipient_audience(42, 42));
        assert!(
            !private_recipient_audience(42, 43),
            "a third party must never receive a private row"
        );
        assert!(
            !private_recipient_audience(0, 42),
            "recipient 0 (unaddressed) matches no viewer"
        );
        assert!(
            !private_recipient_audience(42, 0),
            "an uninitialized viewer guid matches no row"
        );
        assert!(
            !private_recipient_audience(0, 0),
            "the all-zero case is a leak, not a match — both sides unset must still deny"
        );
    }

    #[test]
    fn one_recenter_reports_both_layers_and_keeps_equal_guids_independent() {
        let view = WorldView::new(true);
        let viewer = viewer(1, 9001);
        view.add_viewer_on_shard(viewer.clone(), CellKey::at(0, 0, 0, 0), 0);

        // Moving the anchor one cell right adds x=3 to the 5x5 neighbourhood. The same GUID in
        // both row tables must appear once in each independent layer delta.
        let entering = CellKey::at(0, 0, 3, 0);
        view.spatial
            .upsert_entity(EntityLayer::WorldEntity, 77, entering, 0, 0);
        view.spatial
            .upsert_entity(EntityLayer::GameObject, 77, entering, 1, 0);
        let delta = view
            .spatial
            .move_viewer_delta(viewer.session, CellKey::at(0, 0, 1, 0))
            .expect("a cell crossing recenters");
        assert_eq!(delta.world_entities.entered, vec![(77, 0)]);
        assert_eq!(delta.game_objects.entered, vec![(77, 1)]);

        // Removing one table's row cannot erase the equal-valued GUID in the other table.
        assert_eq!(
            view.spatial.remove_entity(EntityLayer::WorldEntity, 77, 0),
            Some((entering, 0))
        );
        assert!(view
            .spatial
            .visible_entities(EntityLayer::WorldEntity, viewer.session)
            .is_empty());
        assert_eq!(
            view.spatial
                .visible_entities(EntityLayer::GameObject, viewer.session),
            vec![(77, 1)]
        );
    }

    #[test]
    fn removing_an_old_session_preserves_the_new_relogin_owner_mapping() {
        let view = WorldView::new(true);
        let old = viewer(1, 9001);
        let (new_tx, _new_rx) = SessionTx::with_depth(0);
        let new = viewer_with_tx(2, 9001, old.bound_identity, new_tx);
        let anchor = CellKey::at(0, 0, 0, 0);
        view.add_viewer_on_shard(old.clone(), anchor, 0);
        view.add_viewer_on_shard(new.clone(), anchor, 0);

        view.remove_viewer(old.session);
        assert_eq!(view.session_of_owner(9001), Some(new.session));
        assert!(view.viewer(new.session).is_some());
        view.remove_viewer(old.session); // idempotent
        assert_eq!(view.session_of_owner(9001), Some(new.session));
        assert_eq!(
            view.viewer_of_identity(BoundIdentity(new.bound_identity))
                .map(|viewer| viewer.session),
            Some(new.session)
        );
    }

    #[test]
    fn replacing_a_session_cleans_every_old_registry_index() {
        let view = WorldView::new(true);
        let old = viewer(7, 9001);
        let replacement = Arc::new(Viewer {
            active: std::sync::atomic::AtomicBool::new(true),
            session: old.session,
            self_guid: 9002,
            bound_identity: identity(99),
            map_id: 1,
            instance_id: 2,
            zone_id: 0.into(),
            tx: old.tx.clone(),
            created: old.created.clone(),
            gates: old.gates.clone(),
            skill_slots: old.skill_slots.clone(),
            explored: Mutex::new(ExplorationReplay::default()),
            motion_pending: old.motion_pending.clone(),
            member_stats: Default::default(),
            ignored: Mutex::default(),
            friends: Mutex::default(),
            team: lyracore_shared::faction::TEAM_ALLIANCE,
            group_events: Default::default(),
        });
        view.add_viewer_on_shard(old.clone(), CellKey::at(0, 0, 0, 0), 3);
        view.add_viewer_on_shard(replacement.clone(), CellKey::at(1, 2, 0, 0), 4);

        assert!(view.viewer_of_owner(OwnerGuid(old.self_guid)).is_none());
        assert!(view
            .viewer_of_identity(BoundIdentity(old.bound_identity))
            .is_none());
        assert!(view.viewers_on_shard(3).is_empty());
        assert_eq!(
            view.viewer_of_owner(OwnerGuid(replacement.self_guid))
                .map(|viewer| viewer.session),
            Some(replacement.session)
        );
        assert_eq!(
            view.viewer_of_identity(BoundIdentity(replacement.bound_identity))
                .map(|viewer| viewer.session),
            Some(replacement.session)
        );
        assert_eq!(view.viewers_on_shard(4).len(), 1);

        view.remove_viewer(replacement.session);
        assert!(view
            .viewer_of_owner(OwnerGuid(replacement.self_guid))
            .is_none());
        assert!(view
            .viewer_of_identity(BoundIdentity(replacement.bound_identity))
            .is_none());
        assert!(view.viewers_on_shard(4).is_empty());
    }

    #[test]
    fn identity_events_route_once_in_fifo_order_and_addon_uses_the_viewer_guid() {
        let view = WorldView::new(true);
        let (owner_tx, owner_rx) = SessionTx::with_depth(0);
        let (other_tx, other_rx) = SessionTx::with_depth(0);
        let owner = viewer_with_tx(1, 0x1122_3344_5566_7788, identity(7), owner_tx);
        let other = viewer_with_tx(2, 9002, identity(8), other_tx);
        let anchor = CellKey::at(0, 0, 0, 0);
        view.add_viewer_on_shard(owner.clone(), anchor, 0);
        view.add_viewer_on_shard(other, anchor, 0);

        for shard in [1, 0] {
            xp_appeared(
                &view,
                shard,
                &XpEvent {
                    id: 1,
                    recipient_identity: owner.bound_identity,
                    killed_guid: 77,
                    total_exp: 123,
                    created_at: spacetimedb_sdk::Timestamp::UNIX_EPOCH,
                    is_kill: true,
                    rested_bonus: 0,
                },
            );
            levelup_appeared(
                &view,
                shard,
                &LevelupEvent {
                    id: 2,
                    recipient_identity: owner.bound_identity,
                    new_level: 10,
                    health_gained: 20,
                    created_at: spacetimedb_sdk::Timestamp::UNIX_EPOCH,
                    mana_gained: 15,
                    strength_gained: 1,
                    agility_gained: 2,
                    stamina_gained: 3,
                    intellect_gained: 4,
                    spirit_gained: 5,
                },
            );
            addon_message_appeared(
                &view,
                shard,
                &AddonMessage {
                    id: 3,
                    recipient_identity: owner.bound_identity,
                    cmd: "progress".to_string(),
                    payload: "ready".to_string(),
                    created_at: spacetimedb_sdk::Timestamp::UNIX_EPOCH,
                },
            );

            if shard == 1 {
                assert!(
                    owner_rx.try_recv().is_err(),
                    "source Shard events cannot reach a destination owner"
                );
                assert!(other_rx.try_recv().is_err());
                continue;
            }
            assert!(
                other_rx.try_recv().is_err(),
                "an unrelated identity receives nothing"
            );
            assert!(matches!(
                queued_job(&owner_rx).as_slice(),
                [Outbound::Raw { opcode: 0x01d0, .. }]
            ));
            assert!(matches!(
                queued_job(&owner_rx).as_slice(),
                [Outbound::One(ServerOpcodeMessage::SMSG_LEVELUP_INFO(_))]
            ));
            match queued_job(&owner_rx).as_slice() {
                [Outbound::Raw { body, .. }] => assert_eq!(
                    &body[5..13],
                    &owner.self_guid.to_le_bytes(),
                    "the addon sender is the addressed viewer"
                ),
                _ => panic!("addon event must produce one raw SMSG_MESSAGECHAT"),
            }
            assert!(
                owner_rx.try_recv().is_err(),
                "each identity row queues exactly once"
            );
        }
    }

    /// The reconnect contract, behaviorally: a re-arm builds a fresh callback generation over the
    /// same shared view (each generation is a `view.clone()` capture, exactly what `wire_insert`
    /// takes), and the viewer registered before the reconnect still receives owner relays through
    /// the new generation — while the fresh connection's initial-apply replays stay suppressed.
    #[test]
    fn a_rearmed_owner_relay_still_reaches_the_logged_in_viewer() {
        let view = std::sync::Arc::new(WorldView::new(true));
        let (owner_tx, owner_rx) = SessionTx::with_depth(0);
        let owner = viewer_with_tx(1, 9001, identity(1), owner_tx);
        view.add_viewer_on_shard(owner.clone(), CellKey::at(0, 0, 0, 0), 0);

        let arm = |view: &std::sync::Arc<WorldView>| {
            let view = view.clone();
            move |row: &TeleportEvent| teleport_appeared(&view, 0, row)
        };
        let first_generation = arm(&view);
        first_generation(&teleport(owner.self_guid));
        assert_eq!(queued_job(&owner_rx).len(), 1);

        // The watchdog's re-arm: a new generation on the replacement connection; the old one
        // dies with its `LiveConn`. The viewer never re-registers.
        let rearmed = arm(&view);
        drop(first_generation);
        rearmed(&teleport(owner.self_guid));
        assert_eq!(
            queued_job(&owner_rx).len(),
            1,
            "a re-armed owner relay must still address the logged-in viewer"
        );
        assert!(
            owner_rx.try_recv().is_err(),
            "each live row queues exactly once"
        );

        // The replay flavor the re-armed generation races: initial-apply rows are history, not
        // live deltas, and the event-family registrations drop them before dispatch.
        assert!(is_initial_apply(&spacetimedb_sdk::Event::SubscribeApplied));
        assert!(!is_initial_apply(&spacetimedb_sdk::Event::Transaction));
    }

    #[test]
    fn shard_membership_is_direct_and_does_not_wait_for_the_self_entity_row() {
        let view = WorldView::new(true);
        let first = viewer(1, 9001);
        let second = viewer(2, 9002);
        view.add_viewer_on_shard(first.clone(), CellKey::at(0, 0, 0, 0), 0);
        view.add_viewer_on_shard(second.clone(), CellKey::at(1, 0, 0, 0), 1);

        assert_eq!(view.spatial.shard_of(EntityLayer::WorldEntity, 9001), None);
        assert_eq!(
            view.viewers_on_shard(0)
                .into_iter()
                .map(|viewer| viewer.session)
                .collect::<HashSet<_>>(),
            HashSet::from([first.session])
        );
        assert_eq!(
            view.viewers_on_shard(1)
                .into_iter()
                .map(|viewer| viewer.session)
                .collect::<HashSet<_>>(),
            HashSet::from([second.session])
        );
    }

    #[test]
    fn a_single_shard_selects_every_registered_viewer() {
        let view = WorldView::new(true);
        let first = viewer(1, 9001);
        let second = viewer(2, 9002);
        let anchor = CellKey::at(0, 0, 0, 0);
        view.add_viewer_on_shard(first.clone(), anchor, 0);
        view.add_viewer_on_shard(second.clone(), anchor, 0);

        assert_eq!(
            view.viewers_on_shard(0)
                .into_iter()
                .map(|viewer| viewer.session)
                .collect::<HashSet<_>>(),
            HashSet::from([first.session, second.session])
        );
    }

    #[test]
    fn removal_immediately_cleans_shard_owner_and_spatial_membership() {
        let view = WorldView::new(true);
        let viewer = viewer(7, 4242);
        let anchor = CellKey::at(0, 0, 0, 0);
        view.add_viewer_on_shard(viewer.clone(), anchor, 3);

        view.remove_viewer(viewer.session);

        assert!(view.viewers_on_shard(3).is_empty());
        assert_eq!(view.session_of_owner(viewer.self_guid), None);
        assert_eq!(view.spatial.viewer_cell(viewer.session), None);
        view.remove_viewer(viewer.session);
    }

    #[test]
    fn a_player_always_receives_their_own_row_before_the_anchor_catches_up() {
        let view = WorldView::new(true);
        let viewer = viewer(7, 4242);
        let anchor = CellKey::at(0, 0, 0, 0);
        view.add_viewer_on_shard(viewer.clone(), anchor, 0);
        let far = CellKey::at(0, 0, 99, 99);

        assert!(view
            .spatial
            .viewers_of(EntityLayer::WorldEntity, far)
            .is_empty());
        assert_eq!(
            view.world_entity_recipients(0, viewer.self_guid, far),
            vec![viewer.session]
        );
        assert_eq!(
            view.world_entity_recipients(0, viewer.self_guid, anchor),
            vec![viewer.session]
        );
        view.remove_viewer(viewer.session);
        assert!(view
            .world_entity_recipients(0, viewer.self_guid, far)
            .is_empty());
    }

    #[test]
    fn world_entry_sweep_defers_resident_reads_to_one_writer_job() {
        let view = Arc::new(WorldView::new(true));
        let (tx, rx) = SessionTx::with_depth(0);
        let viewer = viewer_with_tx(1, 9001, identity(1), tx);
        view.add_viewer_on_shard(viewer.clone(), CellKey::at(0, 0, 0, 0), 0);
        let shards = view.shards.write().unwrap();
        let (done, completed) = std::sync::mpsc::channel();
        let reader = {
            let view = view.clone();
            std::thread::spawn(move || {
                sweep_into_view(&view, &viewer);
                done.send(()).unwrap();
            })
        };

        let returned = completed.recv_timeout(std::time::Duration::from_secs(5));
        drop(shards);
        reader.join().unwrap();
        assert!(
            returned.is_ok(),
            "the reader must not wait for the resident Shard table"
        );
        assert!(queued_job(&rx).is_empty(), "there are no resident rows");
        assert!(rx.try_recv().is_err(), "one sweep must be one writer job");
    }
    /// **The other half of the fix:** the online mark is one-shot and starts unset, so a claim
    /// that closes for a Character this Gateway never watched come online — a login that fails
    /// after `claim_session` succeeds, before `enter_world`/`player_login` ever runs — has nothing
    /// to consume. No Fake reaches a Coordinator-backed `account_claim_changed` call to prove the
    /// end-to-end "no notice at all" path for a failed login directly.
    #[test]
    fn take_character_online_answers_false_for_a_login_it_never_observed() {
        let view = WorldView::new(true);
        assert!(
            !view.take_character_online(10),
            "nothing marked this guid online, so there is nothing to consume"
        );
    }
    #[test]
    fn quest_owner_routing_follows_a_transfer_to_its_destination_viewer() {
        let view = WorldView::new(true);
        let source = viewer(1, 9001);
        let destination = viewer(2, 9001);
        let anchor = CellKey::at(0, 0, 0, 0);
        view.add_viewer_on_shard(source.clone(), anchor, 0);
        view.remove_viewer(source.session);
        view.add_viewer_on_shard(destination.clone(), anchor, 1);

        assert_eq!(
            view.viewer_of_owner(OwnerGuid(9001))
                .map(|viewer| viewer.session),
            Some(destination.session),
            "a later quest row must route only to the destination viewer"
        );
        assert!(
            view.viewer_of_owner_on_shard(0, OwnerGuid(9001)).is_none(),
            "a source-shard cascade delta applied after the transfer must find no viewer"
        );
        assert_eq!(
            view.viewer_of_owner_on_shard(1, OwnerGuid(9001))
                .map(|viewer| viewer.session),
            Some(destination.session)
        );
    }
}

#[cfg(test)]
pub(crate) mod test_support;

#[cfg(test)]
mod reconciliation_tests {
    use super::test_support::{contact_entry as ignore_row, registered_viewer as listener};
    use super::*;
    use crate::codec::social::subscriptions::contact_changed;

    const SPEAKER: u64 = 10;

    /// A resubscribe replays inserts but not the deletes it missed. Reconciling the Shard must
    /// rebuild each of its viewers' ignore sets from the rows that are there now.
    #[test]
    fn reconciling_a_shard_rebuilds_its_viewers_ignore_sets_from_the_contact_rows() {
        let view = Arc::new(WorldView::new(true));
        let (stale, _stale_rx) = listener(&view, 0, 20);
        let (kept, _kept_rx) = listener(&view, 0, 30);
        let (elsewhere, _elsewhere_rx) = listener(&view, 1, 40);
        for (viewer, shard) in [(&stale, 0), (&kept, 0), (&elsewhere, 1)] {
            contact_changed(
                &view,
                shard,
                &ignore_row(viewer.self_guid, SPEAKER, true),
                true,
            );
        }

        super::reconcile_shard(
            &view,
            0,
            vec![],
            vec![],
            vec![],
            vec![
                ignore_row(30, SPEAKER, true),
                ignore_row(30, 77, true),
                ignore_row(20, 77, false),
            ],
        );

        assert!(
            !stale.ignores(SPEAKER),
            "an ignore deleted while disconnected must not survive the resubscribe"
        );
        assert!(kept.ignores(SPEAKER) && kept.ignores(77));
        assert!(!stale.ignores(77), "a friend row is not an ignore");
        assert!(
            elsewhere.ignores(SPEAKER),
            "another Shard's viewers keep the set their own Shard maintains"
        );
    }

    /// A resubscribe rebuilds the friend set the same way it rebuilds the ignore set.
    #[test]
    fn reconciling_a_shard_rebuilds_its_viewers_friend_sets_from_the_contact_rows() {
        let view = Arc::new(WorldView::new(true));
        let (kept, _kept_rx) = listener(&view, 0, 30);
        contact_changed(&view, 0, &ignore_row(30, SPEAKER, false), true);

        super::reconcile_shard(
            &view,
            0,
            vec![],
            vec![],
            vec![],
            vec![ignore_row(30, SPEAKER, false), ignore_row(30, 77, true)],
        );

        assert!(kept.is_friend(SPEAKER));
        assert!(!kept.is_friend(77), "an ignore row is not a friend");
    }
}

#[cfg(test)]
mod group_event_order_tests {
    use super::{GroupEvent, GroupEventCursor, GroupEventSnapshot, GroupEventSource};

    const RECIPIENT: u64 = 7;

    fn event(id: u64, recipient_guid: u64, created_micros: i64) -> GroupEvent {
        GroupEvent {
            id,
            recipient_identity: spacetimedb_sdk::Identity::ZERO,
            kind: 0,
            other_guid: 0,
            other_name: String::new(),
            created_at: spacetimedb_sdk::Timestamp::from_micros_since_unix_epoch(created_micros),
            payload: String::new(),
            recipient_guid,
        }
    }

    fn ids(rows: &[GroupEvent]) -> Vec<u64> {
        rows.iter().map(|row| row.id).collect()
    }

    /// One transaction wrote rows 10 to 12 for the recipient and row 13 for someone else. Row 5 is
    /// an older transaction's. The SDK fires the inserts in hash order; the recipient still gets
    /// 10, 11, 12 once, in that order.
    #[test]
    fn the_first_callback_of_a_transaction_relays_its_rows_in_insert_order() {
        let cached = [
            event(5, RECIPIENT, 100),
            event(12, RECIPIENT, 200),
            event(10, RECIPIENT, 200),
            event(13, 8, 200),
            event(11, RECIPIENT, 200),
        ];
        let cursor = GroupEventCursor::default();
        let source = GroupEventSource::RealmCore;

        let first = cursor.take(source, &cached[1], || cached.to_vec());
        let later: Vec<GroupEvent> = [&cached[2], &cached[4]]
            .into_iter()
            .flat_map(|row| {
                cursor.take(source, row, || -> Vec<GroupEvent> {
                    panic!("a row already taken must not read the table again")
                })
            })
            .collect();

        assert_eq!(ids(&first), [10, 11, 12]);
        assert!(later.is_empty());
    }

    /// The next transaction starts after the cursor, even with the same timestamp, and another
    /// database's ids are counted apart.
    #[test]
    fn each_source_database_keeps_its_own_cursor() {
        let cursor = GroupEventCursor::default();
        let realm = [event(10, RECIPIENT, 200), event(11, RECIPIENT, 200)];
        cursor.take(GroupEventSource::RealmCore, &realm[0], || realm.to_vec());

        let next = [
            event(10, RECIPIENT, 200),
            event(11, RECIPIENT, 200),
            event(14, RECIPIENT, 200),
        ];
        let shard = [event(3, RECIPIENT, 200)];

        assert_eq!(
            ids(&cursor.take(GroupEventSource::RealmCore, &next[2], || next.to_vec())),
            [14]
        );
        assert_eq!(
            ids(&cursor.take(GroupEventSource::Shard(0), &shard[0], || shard.to_vec())),
            [3]
        );
    }

    /// A Raid roster list writes two rows for each of 40 members in one transaction. The SDK
    /// fires an insert callback per row; the table is read once, and every member relays both of
    /// its rows once, oldest first.
    #[test]
    fn one_transaction_reads_the_table_once_for_every_recipient() {
        let members = 40_u64;
        let transaction: Vec<GroupEvent> = (0..members)
            .flat_map(|member| {
                let guid = 100 + member;
                [
                    event(1000 + member * 2, guid, 200),
                    event(1001 + member * 2, guid, 200),
                ]
            })
            .collect();
        let mut table = vec![event(5, 100, 100)];
        table.extend(transaction.iter().cloned());

        let mut snapshot = GroupEventSnapshot::default();
        let cursors: Vec<GroupEventCursor> =
            (0..members).map(|_| GroupEventCursor::default()).collect();
        let reads = std::cell::Cell::new(0);
        let mut relayed: Vec<Vec<Vec<u64>>> = vec![Vec::new(); members as usize];
        for row in transaction.iter().rev() {
            let member = (row.recipient_guid - 100) as usize;
            let rows = snapshot.take(&cursors[member], GroupEventSource::RealmCore, row, || {
                reads.set(reads.get() + 1);
                table.clone()
            });
            relayed[member].push(ids(&rows));
        }

        assert_eq!(reads.get(), 1);
        for (member, calls) in relayed.iter().enumerate() {
            let first = 1000 + member as u64 * 2;
            assert_eq!(calls, &[vec![first, first + 1], vec![]]);
        }
    }

    /// A row the snapshot lacks starts the next transaction, even with the same timestamp.
    #[test]
    fn the_next_transaction_reads_the_table_again() {
        let table = [
            event(10, RECIPIENT, 200),
            event(11, RECIPIENT, 200),
            event(12, RECIPIENT, 200),
        ];
        let mut snapshot = GroupEventSnapshot::default();
        let cursor = GroupEventCursor::default();
        let reads = std::cell::Cell::new(0);
        let mut take = |row: &GroupEvent, visible: usize| {
            ids(
                &snapshot.take(&cursor, GroupEventSource::RealmCore, row, || {
                    reads.set(reads.get() + 1);
                    table[..visible].to_vec()
                }),
            )
        };

        assert_eq!(take(&table[0], 2), [10, 11]);
        assert!(take(&table[1], 2).is_empty());
        assert_eq!(take(&table[2], 3), [12]);
        assert_eq!(reads.get(), 2);
    }
}

// ===============================================================================================
//  The pump→writer hand-off
// ===============================================================================================

/// Queue work for one World Session. The socket writer checks its lifetime before running it.
/// Jobs already running finish before the destination entry batch on the same FIFO writer;
/// jobs selected by an old callback but enqueued after that batch are discarded here.
pub(crate) fn enqueue(
    viewer: Arc<Viewer>,
    job: impl FnOnce(Arc<Viewer>) -> Vec<Outbound> + Send + 'static,
) {
    let tx = viewer.tx.clone();
    let _ = tx.send(Outbound::Job(Box::new(move || {
        if !viewer.active.load(Ordering::Acquire) {
            return Vec::new();
        }
        job(viewer)
    })));
}

// ===============================================================================================
//  Viewer recentre — the session-thread half
// ===============================================================================================

/// Move a session's anchor and relay the visibility delta the move implies: a CREATE for everything
/// that entered the box, a DESTROY for everything that left it.
pub(crate) fn recenter(view: &Arc<WorldView>, viewer: &Arc<Viewer>, map_id: u32, x: f32, y: f32) {
    let key = CellKey::of_position(map_id, viewer.instance_id, x, y);
    let Some(delta) = view.spatial.move_viewer_delta(viewer.session, key) else {
        return; // no crossing: the by-far-most-common per-heartbeat call, and it allocates nothing
    };
    AOI_RECENTERS.fetch_add(1, Ordering::Relaxed);

    // Everything below is enqueued as a JOB rather than sent directly, for ORDERING: the shard
    // pumps are concurrently enqueueing CREATE/VALUES jobs for the same session, and putting both
    // producers on the one queue is what makes "the DESTROY for a peer I walked away from lands
    // after the CREATE that showed it to me" true rather than a race between two threads.
    {
        for (guid, shard) in delta.world_entities.left {
            let owner_guid = view
                .shard(shard)
                .and_then(|c| {
                    let g = c.0.coord();
                    let row = g.conn.db.game_world_entity().guid().find(&guid);
                    row.map(|r| r.owner_guid)
                })
                .unwrap_or(0);
            let viewer = viewer.clone();
            enqueue(viewer.clone(), move |viewer| {
                crate::codec::entity::subscriptions::relay_peer_destroy(&viewer, guid, owner_guid)
            });
        }
        for (guid, shard) in delta.world_entities.entered {
            let Some(coord) = view.shard(shard) else {
                continue;
            };
            let Some(row) = coord
                .0
                .coord()
                .conn
                .db
                .game_world_entity()
                .guid()
                .find(&guid)
            else {
                continue;
            };
            let (view, viewer, row) = (view.clone(), viewer.clone(), Arc::new(row));
            enqueue(viewer.clone(), move |viewer| {
                crate::codec::entity::subscriptions::offer_peer_create_for(
                    &coord, &view, shard, &viewer, &row,
                )
            });
        }
    }
    {
        for (guid, _) in delta.game_objects.left {
            let viewer = viewer.clone();
            enqueue(viewer.clone(), move |viewer| {
                crate::codec::gameobject::subscriptions::relay_gameobject_destroy(&viewer, guid)
            });
        }
        for (guid, shard) in delta.game_objects.entered {
            let Some(coord) = view.shard(shard) else {
                continue;
            };
            let Some(row) = coord.0.coord().conn.db.game_gameobject().guid().find(&guid) else {
                continue;
            };
            let (viewer, row) = (viewer.clone(), Arc::new(row));
            enqueue(viewer.clone(), move |viewer| {
                crate::codec::gameobject::subscriptions::relay_gameobject_create(
                    &coord, &viewer, &row,
                )
            });
        }
    }
}

/// Offer resident rows in one writer job. Reading the AOI and caches inside the job keeps a
/// queued DESTROY from being followed by a CREATE built from an earlier resident snapshot.
/// The writer sends every CREATE and its follow-up VALUES before running another Relay.
pub(crate) fn sweep_into_view(view: &Arc<WorldView>, viewer: &Arc<Viewer>) {
    let view = view.clone();
    enqueue(viewer.clone(), move |viewer| {
        let mut out = Vec::new();
        if let Some(shard) = view
            .spatial
            .shard_of(EntityLayer::WorldEntity, viewer.self_guid)
        {
            if let Some(coord) = view.shard(shard) {
                out.extend(crate::codec::combat::subscriptions::aura_snapshot_outbound(
                    &coord,
                    &view,
                    shard,
                    &viewer,
                    viewer.self_guid,
                    &[],
                ));
            }
        }
        for (guid, shard) in view
            .spatial
            .visible_entities(EntityLayer::WorldEntity, viewer.session)
        {
            let Some(coord) = view.shard(shard) else {
                continue;
            };
            let Some(row) = coord
                .0
                .coord()
                .conn
                .db
                .game_world_entity()
                .guid()
                .find(&guid)
            else {
                continue;
            };
            out.extend(crate::codec::entity::subscriptions::offer_peer_create_for(
                &coord, &view, shard, &viewer, &row,
            ));
        }
        for (guid, shard) in view
            .spatial
            .visible_entities(EntityLayer::GameObject, viewer.session)
        {
            let Some(coord) = view.shard(shard) else {
                continue;
            };
            let Some(row) = coord.0.coord().conn.db.game_gameobject().guid().find(&guid) else {
                continue;
            };
            out.extend(
                crate::codec::gameobject::subscriptions::relay_gameobject_create(
                    &coord, &viewer, &row,
                ),
            );
        }

        // The login sequence queued self CREATE before registration, so the resident flight
        // follows it on this writer too.
        for coord in view.shards.read().unwrap().iter() {
            out.extend(
                crate::codec::taxi::subscriptions::resident_taxi_spline_outbound(
                    coord,
                    &viewer,
                    viewer.self_guid,
                ),
            );
        }
        // Only the Home Shard can hold this Character's countdown. Another Shard's row would be a
        // Transfer source's delete still on its way.
        if let Some(coord) = view
            .shard_of_viewer(viewer.session)
            .and_then(|shard| view.shard(shard))
        {
            out.extend(resident_instance_removal_outbound(
                &view,
                &coord,
                viewer.self_guid,
            ));
        }
        out
    });
}

/// Replace this Shard's rows between SDK messages. Its old pump has stopped, and other
/// Shards retain their current rows.
fn seed_shard_from_cache(view: &Arc<WorldView>, shard: ShardId, db: &RemoteTables) {
    reconcile_shard(
        view,
        shard,
        db.game_world_entity().iter().collect(),
        db.game_gameobject().iter().collect(),
        db.game_aura().iter().collect(),
        db.game_character_contact().iter().collect(),
    );
}

fn reconcile_shard(
    view: &Arc<WorldView>,
    shard: ShardId,
    entities: Vec<WorldEntity>,
    objects: Vec<GameObject>,
    auras: Vec<Aura>,
    contacts: Vec<ContactEntry>,
) {
    replace_contact_sets(view, shard, &contacts);
    let aura_targets = view.auras.replace_shard(shard, auras);
    let removed_entities = view.spatial.replace_shard(
        EntityLayer::WorldEntity,
        shard,
        entities
            .iter()
            .map(|row| (row.guid, entity_key(row), row.owner_guid)),
    );
    let removed_objects = view.spatial.replace_shard(
        EntityLayer::GameObject,
        shard,
        objects.iter().map(|row| {
            (
                row.guid,
                CellKey::at(row.map_id, row.instance_id, row.grid_x, row.grid_y),
                0,
            )
        }),
    );
    for (layer, removed) in [
        (EntityLayer::WorldEntity, removed_entities),
        (EntityLayer::GameObject, removed_objects),
    ] {
        for (guid, key, owner_guid) in removed {
            for viewer in view.cell_audience(shard, Some(key), BOX_HALF_SPAN, &[]) {
                if view.spatial.can_see(layer, viewer.session, guid) {
                    continue;
                }
                enqueue(viewer.clone(), move |viewer| match layer {
                    EntityLayer::WorldEntity => {
                        crate::codec::entity::subscriptions::relay_peer_destroy(
                            &viewer, guid, owner_guid,
                        )
                    }
                    EntityLayer::GameObject => {
                        crate::codec::gameobject::subscriptions::relay_gameobject_destroy(
                            &viewer, guid,
                        )
                    }
                });
            }
        }
    }
    if let Some(coord) = view.shard(shard) {
        for (target_guid, previous) in aura_targets {
            let previous = Arc::new(previous);
            let key =
                view.spatial
                    .entity_cell_on_shard(EntityLayer::WorldEntity, target_guid, shard);
            for viewer in view.cell_audience(shard, key, BOX_HALF_SPAN, &[target_guid]) {
                let (coord, view, previous) = (coord.clone(), view.clone(), previous.clone());
                enqueue(viewer.clone(), move |viewer| {
                    crate::codec::combat::subscriptions::aura_snapshot_outbound(
                        &coord,
                        &view,
                        shard,
                        &viewer,
                        target_guid,
                        &previous,
                    )
                });
            }
        }
    }
    // Existing WorldEntity rows retain their CREATE dedup state.
    for row in entities {
        if view.spatial.shard_of(EntityLayer::WorldEntity, row.guid) != Some(shard) {
            continue;
        }
        let row = Arc::new(row);
        for session in view.world_entity_recipients(shard, row.guid, entity_key(&row)) {
            if view
                .viewer(session)
                .is_some_and(|v| v.created.lock().unwrap().contains(&row.guid))
            {
                continue;
            }
            offer_create_job(view, shard, session, &row);
        }
    }
    // Refresh resident GameObjects too: state and rotation may have changed while disconnected.
    // Their CREATE packets do not use the WorldEntity dedup set.
    for row in objects {
        if view.spatial.shard_of(EntityLayer::GameObject, row.guid) == Some(shard) {
            gameobject_appeared(view, shard, &row);
        }
    }
    let (aura_rows, aura_units) = view.auras.stats();
    log::info!(
        "shared AOI shard {shard} reconciled; {aura_rows} auras on {aura_units} shard-local units"
    );
}
