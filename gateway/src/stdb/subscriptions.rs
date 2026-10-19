use crate::codec;
use crate::codec::corpse::subscriptions::relay_corpse_create;
use crate::codec::entity::subscriptions::instance_relay_gate;
use crate::world::{Outbound, SessionTx};
use anyhow::{anyhow, Result};
use spacetimedb_sdk::Table;
use std::collections::HashSet;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use wow_world_messages::vanilla::opcodes::ServerOpcodeMessage;

use super::aoi::ViewerGates;
use super::bindings::*;
use super::connection::Coordinator;
use super::world_index::CellKey;
use super::world_view::{self, Viewer, WorldView};

#[cfg(test)]
pub(crate) static DURABLE_TOPOLOGY_ENV_LOCK: Mutex<()> = Mutex::new(());

pub struct PlayerSubscriptions {
    viewer: Option<Arc<Viewer>>,
    view: Option<Arc<WorldView>>,
}

impl PlayerSubscriptions {
    #[cfg(test)]
    pub fn empty() -> Self {
        Self {
            viewer: None,
            view: None,
        }
    }

    #[cfg(test)]
    pub(crate) fn registered_for_test(
        view: Arc<WorldView>,
        self_guid: u64,
        arrival: &codec::EntityView,
        tx: SessionTx,
    ) -> Self {
        let (instance_id, map_id, x, y) =
            (arrival.instance_id, arrival.map_id, arrival.x, arrival.y);
        let mut identity = [0; 32];
        identity[..8].copy_from_slice(&self_guid.to_le_bytes());
        let viewer = Arc::new(Viewer {
            active: std::sync::atomic::AtomicBool::new(true),
            session: view.next_session_id(),
            self_guid,
            bound_identity: spacetimedb_sdk::Identity::from_byte_array(identity),
            instance_id,
            map_id,
            zone_id: arrival.zone_id.into(),
            tx,
            created: Arc::new(Mutex::new([self_guid].into_iter().collect())),
            gates: Arc::new(ViewerGates::default()),
            skill_slots: Arc::new(Mutex::new((std::collections::HashMap::new(), 0))),
            explored: Mutex::new(world_view::ExplorationReplay::default()),
            motion_pending: Arc::new(world_view::MotionPending::default()),
            member_stats: Default::default(),
            ignored: Mutex::default(),
            friends: Mutex::default(),
            team: lyracore_shared::faction::TEAM_ALLIANCE,
            group_events: Default::default(),
        });
        view.add_viewer_on_shard(
            viewer.clone(),
            CellKey::of_position(map_id, instance_id, x, y),
            0,
        );
        Self {
            viewer: Some(viewer),
            view: Some(view),
        }
    }

    /// Unregister before Transfer deletes source rows whose Relays could reach the destination.
    pub(crate) fn unregister_viewer(&mut self) {
        if let (Some(view), Some(viewer)) = (self.view.take(), self.viewer.take()) {
            viewer
                .active
                .store(false, std::sync::atomic::Ordering::Release);
            view.remove_viewer(viewer.session);
        }
    }

    pub(crate) fn member_stats_record(&self) -> Option<&crate::world::MemberStatsRecord> {
        self.viewer.as_ref().map(|viewer| &viewer.member_stats)
    }

    pub(crate) fn teleport_pending(&self) -> bool {
        self.viewer
            .as_ref()
            .is_some_and(|viewer| viewer.gates.teleport_pending.load(Ordering::Acquire))
    }

    pub(crate) fn teleport_acknowledged(&self) {
        if let Some(viewer) = &self.viewer {
            viewer
                .gates
                .teleport_pending
                .store(false, Ordering::Release);
        }
    }

    pub fn aoi_update(&mut self, x: f32, y: f32) {
        let (Some(viewer), Some(view)) = (self.viewer.as_ref(), self.view.as_ref()) else {
            return;
        };
        world_view::recenter(view, viewer, viewer.map_id, x, y);
    }
}

impl Drop for PlayerSubscriptions {
    fn drop(&mut self) {
        self.unregister_viewer();
    }
}

pub(crate) static MOTION_CALLS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
pub(crate) static MOTION_SENT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

pub(crate) static MOTION_DROPPED: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);

#[allow(clippy::too_many_arguments)]
pub(crate) fn motionstat_line(
    calls: u64,
    sent: u64,
    dropped: u64,
    d_calls: u64,
    d_sent: u64,
    d_dropped: u64,
    d_submitted: u64,
    submitted: u64,
) -> String {
    let delivery = if d_calls > 0 {
        format!("{:.1}%", 100.0 * d_sent as f64 / d_calls as f64)
    } else {
        "--".to_string()
    };
    let fanout = match fanout_ratio(d_calls, d_submitted) {
        Some(f) => format!("{f:.1}"),
        None => "--".to_string(),
    };
    format!(
        "MOTIONSTAT calls={calls} sent={sent} dropped={dropped} \
         (+{d_calls} +{d_sent} +{d_dropped} in 10s) | delivery={delivery} fanout={fanout}/move \
         | move submitted={submitted}"
    )
}

pub(crate) fn fanout_ratio(d_calls: u64, d_submitted: u64) -> Option<f64> {
    (d_submitted > 0).then(|| d_calls as f64 / d_submitted as f64)
}

pub(crate) const FANOUT_COLLAPSE_RATIO: f64 = 0.6;

pub(crate) const FANOUT_COLLAPSE_WINDOWS: u32 = 3;

pub(crate) const FANOUT_REBASELINE_WINDOWS: u32 = 6;

pub(crate) const FANOUT_BASELINE_ALPHA: f64 = 0.3;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct FanoutHealth {
    pub(crate) baseline: Option<f64>,
    pub(crate) low_windows: u32,
}

pub(crate) fn fanout_health_step(
    state: FanoutHealth,
    fanout: Option<f64>,
    submitted_delta: u64,
) -> (FanoutHealth, Option<u32>) {
    let Some(f) = fanout.filter(|_| submitted_delta > crate::world::MOVE_ACTIVITY_FLOOR) else {
        return (
            FanoutHealth {
                low_windows: 0,
                ..state
            },
            None,
        );
    };
    let Some(baseline) = state.baseline else {
        return (
            FanoutHealth {
                baseline: Some(f),
                low_windows: 0,
            },
            None,
        );
    };
    if f >= baseline * FANOUT_COLLAPSE_RATIO {
        let next = baseline + FANOUT_BASELINE_ALPHA * (f - baseline);
        return (
            FanoutHealth {
                baseline: Some(next),
                low_windows: 0,
            },
            None,
        );
    }
    let low_windows = state.low_windows + 1;
    let warn = (low_windows >= FANOUT_COLLAPSE_WINDOWS).then_some(low_windows);
    if low_windows >= FANOUT_REBASELINE_WINDOWS {
        return (
            FanoutHealth {
                baseline: Some(f),
                low_windows: 0,
            },
            warn,
        );
    }
    (
        FanoutHealth {
            baseline: Some(baseline),
            low_windows,
        },
        warn,
    )
}

pub(crate) fn shed_motion_at_depth(depth: usize) -> bool {
    depth > crate::world::EGRESS_SHED_DEPTH
}

impl Coordinator {
    pub fn spawn_party_command_relay(&self) {
        for shard in self.all_shards() {
            let polling_shard = shard.clone();
            if let Err(error) = std::thread::Builder::new()
                .name("party-command-dispatch".into())
                .spawn(move || loop {
                    polling_shard.dispatch_party_command_intents();
                    std::thread::sleep(Duration::from_millis(100));
                })
            {
                log::error!("could not start party command dispatcher: {error}");
            }
        }
    }

    fn dispatch_party_command_intents(&self) {
        const DISPATCH_LANE_LIMIT: usize = lyracore_shared::group::COMMAND_DISPATCH_LANES as usize;
        let pending = {
            let live = self.0.coord();
            let lanes: Vec<_> = live
                .conn
                .db
                .game_party_command_dispatch_lane()
                .iter()
                .take(DISPATCH_LANE_LIMIT + 1)
                .map(|lane| lane.head_intent_id)
                .collect();
            if lanes.len() > DISPATCH_LANE_LIMIT {
                if !self
                    .0
                    .party_command_lane_overflow
                    .swap(true, Ordering::AcqRel)
                {
                    log::error!(
                        "party command dispatcher found more than {DISPATCH_LANE_LIMIT} lanes; \
                         command dispatch on this Shard is stopped until the Module and Gateway \
                         lane counts agree"
                    );
                }
                return;
            }
            if self
                .0
                .party_command_lane_overflow
                .swap(false, Ordering::AcqRel)
            {
                log::info!("party command dispatcher lane count recovered on this Shard");
            }
            lanes
                .into_iter()
                .filter_map(|head_intent_id| {
                    live.conn
                        .db
                        .game_party_command_intent()
                        .id()
                        .find(&head_intent_id)
                        .map(|row| party_command_intent(&row))
                })
                .collect::<Vec<_>>()
        };
        for intent in pending {
            attempt_party_command(self, intent);
        }
    }

    pub fn subscribe_player_events(
        &self,
        account_id: u64,
        self_guid: u64,
        arrival: &codec::EntityView,
        tx: SessionTx,
    ) -> Result<PlayerSubscriptions> {
        self.watch_session(tx.clone());
        let (login_instance, login_map, login_zone, login_x, login_y) = (
            arrival.instance_id,
            arrival.map_id,
            arrival.zone_id,
            arrival.x,
            arrival.y,
        );
        let created = Arc::new(Mutex::new(rustc_hash::FxHashSet::from_iter([self_guid])));
        // Read ghost state from the mirror; re-entering the cache from a callback would deadlock.
        let viewer_gates = Arc::new(ViewerGates::default());
        let view = self.world_view();
        let session = view.next_session_id();

        let skill_slots: std::sync::Arc<
            std::sync::Mutex<(std::collections::HashMap<u32, u8>, u8)>,
        > = {
            let class_b = {
                let guard = self.0.coord();
                guard
                    .conn
                    .db
                    .game_world_entity()
                    .guid()
                    .find(&self_guid)
                    .map(|e| ((e.unit_bytes_0 >> 8) & 0xFF) as u8)
                    .unwrap_or(0)
            };
            let learned = self.player_skills(self_guid).unwrap_or_default();
            let layout = codec::skill_slot_layout(class_b, &learned);
            let map: std::collections::HashMap<u32, u8> = layout
                .iter()
                .enumerate()
                .map(|(i, &(sk, _, _))| (sk.as_int() as u32, i as u8))
                .collect();
            let next_free = layout.len() as u8;
            std::sync::Arc::new(std::sync::Mutex::new((map, next_free)))
        };

        let mut explored = world_view::ExplorationReplay::default();

        let self_identity =
            spacetimedb_sdk::Identity::from_byte_array(self.bound_identity(account_id)?);

        {
            // Seed before registration so replay restores fog without discovery feedback.
            let guard = self.0.coord();
            explored.seed(
                guard
                    .conn
                    .db
                    .game_character_explored()
                    .iter()
                    .filter(|r| r.character_guid == self_guid)
                    .map(|r| r.area_bit),
            );
        }

        {
            const GHOST_PLAYER_FLAG: u32 = lyracore_shared::constants::player_flags::GHOST;
            let is_ghost = self
                .0
                .coord()
                .conn
                .db
                .game_world_entity()
                .guid()
                .find(&self_guid)
                .map(|e| e.player_flags & GHOST_PLAYER_FLAG != 0)
                .unwrap_or(false);
            viewer_gates
                .is_ghost
                .store(is_ghost, std::sync::atomic::Ordering::Relaxed);
        }
        let (ignored, friends): (HashSet<u64>, HashSet<u64>) = {
            let guard = self.0.coord();
            let contacts = guard.contacts.read().unwrap_or_else(|p| p.into_inner());
            (
                contacts.ignored_by(self_guid).into_iter().collect(),
                contacts.friends_of(self_guid).into_iter().collect(),
            )
        };
        let team = lyracore_shared::faction::team_for_race((arrival.unit_bytes_0 & 0xFF) as u8);
        let viewer = Arc::new(Viewer {
            active: std::sync::atomic::AtomicBool::new(true),
            session,
            self_guid,
            bound_identity: self_identity,
            instance_id: login_instance,
            map_id: login_map,
            zone_id: login_zone.into(),
            tx: tx.clone(),
            created: created.clone(),
            gates: viewer_gates.clone(),
            skill_slots: skill_slots.clone(),
            explored: Mutex::new(explored),
            motion_pending: Arc::new(world_view::MotionPending::default()),
            member_stats: Default::default(),
            ignored: Mutex::new(ignored),
            friends: Mutex::new(friends),
            team,
            group_events: Default::default(),
        });
        view.add_viewer(
            self,
            viewer.clone(),
            CellKey::of_position(login_map, login_instance, login_x, login_y),
        )?;
        world_view::sweep_into_view(&view, &viewer);

        let resident_corpses: Vec<Corpse> = {
            let guard = self.0.coord();
            guard.conn.db.game_corpse().iter().collect()
        };
        for row in &resident_corpses {
            if instance_relay_gate(row.instance_id, Some(login_instance)) {
                relay_corpse_create(&tx, self_guid, row);
            }
        }
        {
            let guard = self.0.coord();
            let db = &guard.conn.db;
            let eff = super::armor::effective_resistances(&guard, self_guid);
            let pos = super::armor::aura_armor_positive(&guard, self_guid);
            let sheet = super::armor::sheet_stats(db, self_guid);
            drop(guard);
            let _ = tx.send(Outbound::One(ServerOpcodeMessage::SMSG_UPDATE_OBJECT(
                Box::new(codec::build_armor_values(self_guid, eff, pos)),
            )));
            if let Some(st) = sheet {
                let _ = tx.send(Outbound::One(ServerOpcodeMessage::SMSG_UPDATE_OBJECT(
                    Box::new(codec::build_sheet_stats_values(self_guid, &st)),
                )));
            }
        }
        {
            let guard = self.0.coord();
            let db = &guard.conn.db;
            let entity = db.game_world_entity().guid().find(&self_guid);
            let auras: Vec<Aura> = db
                .game_aura()
                .iter()
                .filter(|aura| aura.target_guid == self_guid)
                .collect();
            drop(guard);
            for out in entity.iter().flat_map(|entity| {
                crate::codec::movement::subscriptions::world_entry_movement_outbound(entity, &auras)
            }) {
                let _ = tx.send(out);
            }
        }
        {
            let guard = self.0.coord();
            let mut words: std::collections::HashMap<u16, u32> = std::collections::HashMap::new();
            for r in guard.conn.db.game_character_explored().iter() {
                if r.character_guid != self_guid || r.area_bit < 0 {
                    continue;
                }
                *words.entry((r.area_bit / 32) as u16).or_default() |= 1u32 << (r.area_bit % 32);
            }
            drop(guard);
            for (word_idx, word) in words {
                let (opcode, body) = codec::build_explored_zones_values(self_guid, word_idx, word);
                let _ = tx.send(Outbound::Raw { opcode, body });
            }
        }

        Ok(PlayerSubscriptions {
            viewer: Some(viewer),
            view: Some(view),
        })
    }
}

fn party_command_intent(row: &PartyCommandIntent) -> crate::world::party::PartyCommandIntent {
    crate::world::party::PartyCommandIntent {
        id: row.id,
        source_identity: row.source_identity,
        issuer_guid: row.issuer_guid,
        issuer_sequence: row.issuer_sequence,
        kind: row.command.kind,
        bot_guid: row.command.bot_guid,
        authority_member_guid: row.command.authority_member_guid,
        exact_target_guid: row.command.exact_target_guid,
        expires_micros: row.expires_micros,
    }
}

fn attempt_party_command(store: &Coordinator, intent: crate::world::party::PartyCommandIntent) {
    let claim_token = next_party_command_claim_token();
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_micros() as i64;
    if now >= intent.expires_micros {
        if let Err(error) =
            crate::world::party::finish_expired_party_command_intent(store, &intent, claim_token)
        {
            log::debug!("party command intent {} expiry retry: {error}", intent.id);
            let _ = store.defer_party_command_intent(intent.id, claim_token);
        }
        return;
    }
    match crate::world::party::run_party_command_intent(store, &intent, claim_token) {
        Ok(crate::world::party::CompanionCommandOutcome::WaitingForCapacity) => {
            if let Err(error) = store.defer_party_command_intent(intent.id, claim_token) {
                log::debug!("party command intent {} defer: {error}", intent.id);
            }
        }
        Ok(_) => {}
        Err(error) => {
            log::debug!("party command intent {} retry: {error}", intent.id);
            let _ = store.defer_party_command_intent(intent.id, claim_token);
        }
    }
}

fn next_party_command_claim_token() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let time = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos() as u64;
    (time ^ NEXT.fetch_add(1, Ordering::Relaxed)).max(1)
}

impl Coordinator {
    pub fn spawn_bot_invite_relay(&self) {
        for shard in self.all_shards() {
            shard.arm_bot_invite_relay();
            let hook_shard = shard.clone();
            shard
                .0
                .on_reconnect
                .lock()
                .unwrap()
                .push(std::sync::Arc::new(move || {
                    hook_shard.arm_bot_invite_relay();
                }));
        }
    }

    fn arm_bot_invite_relay(&self) {
        let store = self.clone();
        self.0
            .coord()
            .conn
            .db
            .game_bot_invite_intent()
            .on_insert(move |_ctx, row| {
                let store = store.clone();
                let (intent_id, op, inviter_guid, target_guid) =
                    (row.id, row.op, row.inviter_guid, row.target_guid);
                // Run off-pump so a one-pipe Gateway can receive its own reducer completion.
                let spawned = std::thread::Builder::new()
                    .name("bot-invite-intent".into())
                    .spawn(move || {
                        match crate::world::party::run_bot_invite_intent(
                            &store,
                            intent_id,
                            op,
                            inviter_guid,
                            target_guid,
                        ) {
                            Ok(crate::world::party::PartyOutcome::Ran) => {}
                            outcome => log::debug!(
                                "group intent {intent_id} op {op} ({inviter_guid} -> \
                                 {target_guid}) did not execute: {outcome:?}"
                            ),
                        }
                    });
                if let Err(error) = spawned {
                    log::error!(
                        "could not start consumer for group intent {intent_id} op {op} \
                         ({inviter_guid} -> {target_guid}): {error}"
                    );
                }
            });
    }

    pub fn spawn_roster_revision_relay(&self) {
        let realm = match self.realm_core() {
            Ok(realm) => realm,
            Err(error) => {
                log::error!(
                    "party: the Roster Revision Relay is not running; Realm-core is unavailable at \
                     startup ({error:#}). World Shard mirrors follow only party ops and the \
                     reconnect pass until the Gateway restarts"
                );
                return;
            }
        };
        if self
            .all_shards()
            .iter()
            .any(|shard| shard.shard_name() == realm.shard_name())
        {
            return;
        }
        let relay = match crate::world::party_mirror::RosterRevisionRelay::spawn(self.clone()) {
            Ok(relay) => relay,
            Err(error) => {
                log::error!("party: could not start the Roster Revision Relay: {error}");
                return;
            }
        };
        realm.arm_roster_revision_relay(&relay);
        let hook_realm = realm.clone();
        realm
            .0
            .on_reconnect
            .lock()
            .unwrap()
            .push(std::sync::Arc::new(move || {
                hook_realm.arm_roster_revision_relay(&relay);
            }));
    }

    fn arm_roster_revision_relay(
        &self,
        relay: &std::sync::Arc<crate::world::party_mirror::RosterRevisionRelay>,
    ) {
        use spacetimedb_sdk::TableWithPrimaryKey;
        let live = self.0.coord();
        let inserted = relay.clone();
        live.conn
            .db
            .game_group_roster_revision()
            .on_insert(move |_ctx, row| inserted.mark_dirty(row.group_id));
        let updated = relay.clone();
        live.conn
            .db
            .game_group_roster_revision()
            .on_update(move |_ctx, _old, row| updated.mark_dirty(row.group_id));
    }

    pub fn spawn_bot_transfer_relay(&self) {
        for shard in self.all_shards() {
            if let Err(error) = std::thread::Builder::new()
                .name("bot-transfer-dispatch".into())
                .spawn(move || {
                    let mut after_id = 0;
                    loop {
                        after_id = shard.dispatch_bot_transfer_intents(after_id);
                        std::thread::sleep(Duration::from_millis(100));
                    }
                })
            {
                log::error!("could not start bot Transfer dispatcher: {error}");
            }
        }
    }

    fn dispatch_bot_transfer_intents(&self, after_id: u64) -> u64 {
        use lyracore_shared::transfer::BOT_TRANSFER_PENDING_LIMIT;
        const BOT_TRANSFER_WORK_LIMIT: usize = 4;
        let mut pending = {
            let live = self.0.coord();
            live.conn
                .db
                .game_bot_transfer_intent()
                .iter()
                .take(BOT_TRANSFER_PENDING_LIMIT + 1)
                .collect::<Vec<_>>()
        };
        if pending.len() > BOT_TRANSFER_PENDING_LIMIT {
            if !self
                .0
                .bot_transfer_pending_overflow
                .swap(true, Ordering::AcqRel)
            {
                log::error!(
                    "bot Transfer dispatcher found more than {BOT_TRANSFER_PENDING_LIMIT} pending \
                     rows; this populated state predates the bounded writer Gate"
                );
            }
            return after_id;
        }
        if self
            .0
            .bot_transfer_pending_overflow
            .swap(false, Ordering::AcqRel)
        {
            log::info!("bot Transfer dispatcher pending count recovered on this Shard");
        }
        pending.sort_by_key(|intent| intent.id);
        rotate_transfer_work(&mut pending, after_id, |intent| intent.id);
        let mut next_after = after_id;
        for row in pending.into_iter().take(BOT_TRANSFER_WORK_LIMIT) {
            next_after = row.id;
            let intent = crate::world::transfer::BotTransferIntent {
                id: row.id,
                bot_guid: row.bot_guid,
                destination_map: row.destination_map,
                destination_instance: row.destination_instance,
                reason: row.reason,
                created_micros: row.created_at.to_micros_since_unix_epoch(),
                controller_generation: row.controller_generation,
                arrival_ready: row.arrival_ready,
                source_module_identity: row.source_module_identity,
                source_map: row.source_map,
                source_instance: row.source_instance,
                source_locator_revision: row.source_locator_revision,
            };
            attempt_bot_transfer(self, &intent);
        }
        next_after
    }

    pub fn spawn_character_gone_relay(&self) {
        for shard in self.all_shards() {
            shard.arm_character_gone_relay();
            let hook_shard = shard.clone();
            shard
                .0
                .on_reconnect
                .lock()
                .unwrap()
                .push(std::sync::Arc::new(move || {
                    hook_shard.arm_character_gone_relay();
                    hook_shard.request_deleted_character_reconciliation();
                }));
        }
        if self.is_sharded() {
            if let Ok(realm) = self.realm_core() {
                let reconciliation_store = self.clone();
                realm
                    .0
                    .on_reconnect
                    .lock()
                    .unwrap()
                    .push(std::sync::Arc::new(move || {
                        reconciliation_store.request_deleted_character_reconciliation();
                    }));
            }
        }
        self.request_deleted_character_reconciliation();
    }

    fn request_deleted_character_reconciliation(&self) {
        self.1.guild_cleanup.lock().unwrap().sweep = true;
        self.start_deleted_character_reconciliation();
    }

    fn request_deleted_character_cleanup(&self, character_guid: u64) {
        self.1
            .guild_cleanup
            .lock()
            .unwrap()
            .deleted
            .insert(character_guid);
        self.start_deleted_character_reconciliation();
    }

    fn start_deleted_character_reconciliation(&self) {
        self.1
            .deleted_character_reconciliation_requested
            .store(true, Ordering::Release);
        if self
            .1
            .deleted_character_reconciliation_running
            .swap(true, Ordering::AcqRel)
        {
            return;
        }
        let store = self.clone();
        let spawned = std::thread::Builder::new()
            .name("reconcile-deleted-characters".into())
            .spawn(move || {
                let mut retry_delay = Duration::from_millis(100);
                loop {
                    store
                        .1
                        .deleted_character_reconciliation_requested
                        .store(false, Ordering::Release);
                    let guild_work = std::mem::take(&mut *store.1.guild_cleanup.lock().unwrap());
                    let parties = crate::world::party::reconcile_deleted_character_parties(&store);
                    let guilds =
                        crate::world::reconcile_deleted_guild_characters(&store, &guild_work);
                    if guilds.is_err() {
                        store.1.guild_cleanup.lock().unwrap().merge(guild_work);
                    }
                    if let Err(error) = parties.and(guilds) {
                        log::warn!(
                            "deleted Character reconciliation deferred ({error:#}); retrying in \
                             {retry_delay:?}"
                        );
                        store
                            .1
                            .deleted_character_reconciliation_requested
                            .store(true, Ordering::Release);
                        std::thread::sleep(retry_delay);
                        retry_delay = (retry_delay * 2).min(Duration::from_secs(5));
                        continue;
                    } else {
                        retry_delay = Duration::from_millis(100);
                    }

                    if store
                        .1
                        .deleted_character_reconciliation_requested
                        .load(Ordering::Acquire)
                    {
                        continue;
                    }
                    store
                        .1
                        .deleted_character_reconciliation_running
                        .store(false, Ordering::Release);
                    if !store
                        .1
                        .deleted_character_reconciliation_requested
                        .load(Ordering::Acquire)
                        || store
                            .1
                            .deleted_character_reconciliation_running
                            .swap(true, Ordering::AcqRel)
                    {
                        break;
                    }
                }
            });
        if let Err(error) = spawned {
            self.1
                .deleted_character_reconciliation_running
                .store(false, Ordering::Release);
            log::error!("party: could not start deleted Character reconciliation: {error}");
        }
    }

    fn arm_character_gone_relay(&self) {
        let live = self.0.coord();
        let inserted_revision = live.character_revision();
        live.conn.db.game_character().on_insert(move |_ctx, _row| {
            inserted_revision.fetch_add(1, Ordering::Release);
        });

        let deleted_revision = live.character_revision();
        let store = self.clone();
        live.conn.db.game_character().on_delete(move |_ctx, row| {
            deleted_revision.fetch_add(1, Ordering::Release);
            store.request_deleted_character_cleanup(row.guid);
        });
    }
}

fn rotate_transfer_work<T>(rows: &mut [T], after_id: u64, id: impl Fn(&T) -> u64) {
    let split = rows.partition_point(|row| id(row) <= after_id);
    rows.rotate_left(split);
}

fn attempt_bot_transfer(store: &Coordinator, intent: &crate::world::transfer::BotTransferIntent) {
    let claim_token = match next_bot_transfer_claim_token() {
        Ok(token) => token,
        Err(error) => {
            log::warn!(
                "bot Transfer Intent {} could not mint a claim: {error:#}",
                intent.id
            );
            return;
        }
    };
    if let Err(error) = store.claim_bot_transfer_intent(
        intent.id,
        intent.bot_guid,
        intent.controller_generation,
        claim_token,
    ) {
        log::debug!("bot Transfer Intent {} claim: {error}", intent.id);
        return;
    }
    if intent.source_module_identity == spacetimedb_sdk::Identity::ZERO {
        log::debug!(
            "bot Transfer Intent {} normalized its source Module identity; a later bounded pass \
             will claim the populated row",
            intent.id
        );
        return;
    }
    if let Err(error) = crate::world::transfer::run_bot_transfer_intent(store, intent, claim_token)
    {
        log::warn!(
            "bot Transfer Intent {} (Character {} -> map {} instance {}) did not settle: \
             {error:#}",
            intent.id,
            intent.bot_guid,
            intent.destination_map,
            intent.destination_instance
        );
        return;
    }
    if let Err(error) = store.complete_bot_transfer_intent(
        intent.id,
        intent.bot_guid,
        intent.controller_generation,
        claim_token,
    ) {
        log::warn!(
            "bot Transfer Intent {} settled but its exact completion did not commit: {error:#}",
            intent.id
        );
    }
}

fn next_bot_transfer_claim_token() -> Result<u64> {
    loop {
        let mut bytes = [0u8; 8];
        getrandom::fill(&mut bytes)
            .map_err(|error| anyhow!("OS randomness unavailable: {error}"))?;
        let token = u64::from_le_bytes(bytes);
        if token != 0 {
            return Ok(token);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec;
    use crate::codec::movement::subscriptions::{creature_leg_outbound, motion_outbound};
    use crate::stdb::bindings::{CreatureSpline, EntityMotion};
    use crate::world::{Outbound, SessionTx};

    use std::sync::{Arc, Mutex};
    use wow_world_messages::vanilla::Vector3d;

    #[test]
    fn transfer_work_continues_after_the_prior_pass_then_wraps() {
        let mut ids = [1, 2, 3, 4, 5, 6];
        rotate_transfer_work(&mut ids, 4, |id| *id);
        assert_eq!(ids, [5, 6, 1, 2, 3, 4]);

        let mut wrapped = [1, 2, 3, 4, 5, 6];
        rotate_transfer_work(&mut wrapped, 6, |id| *id);
        assert_eq!(wrapped, [1, 2, 3, 4, 5, 6]);
    }

    // Tests that read or change process-global motion counters must hold this lock.
    static MOTION_COUNTERS: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn relay_entity_motion(
        tx: &SessionTx,
        created: &Arc<Mutex<rustc_hash::FxHashSet<u64>>>,
        self_guid: u64,
        row: &EntityMotion,
    ) {
        if shed_motion_at_depth(tx.depth()) {
            MOTION_DROPPED.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            return;
        }
        for o in motion_outbound(created, self_guid, row) {
            if tx.send(o).is_err() {
                MOTION_DROPPED.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            }
        }
    }

    fn relay_creature_leg(
        tx: &SessionTx,
        created: &Arc<Mutex<rustc_hash::FxHashSet<u64>>>,
        row: &CreatureSpline,
    ) {
        if shed_motion_at_depth(tx.depth()) {
            MOTION_DROPPED.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            return;
        }
        for o in creature_leg_outbound(created, row) {
            if tx.send(o).is_err() {
                MOTION_DROPPED.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            }
        }
    }

    fn motion_counter_lock() -> std::sync::MutexGuard<'static, ()> {
        MOTION_COUNTERS.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn motion_row(guid: u64, opcode: u16, movement_info: Vec<u8>) -> EntityMotion {
        EntityMotion {
            guid,
            map_id: 0,
            instance_id: 0,
            grid_x: 0,
            grid_y: 0,
            cell: lyracore_shared::spatial::grid_cell_id(0, 0),
            opcode,
            movement_info,
            seq: 0,
        }
    }

    fn moving_forward() -> (wow_world_messages::vanilla::MovementInfo, Vec<u8>) {
        let info = wow_world_messages::vanilla::MovementInfo {
            flags: wow_world_messages::vanilla::MovementInfo_MovementFlags::new_forward(),
            timestamp: 0x0BAD_F00D,
            position: Vector3d {
                x: -8949.95,
                y: -132.493,
                z: 83.5312,
            },
            orientation: 2.5,
            fall_time: 0.0,
        };
        let bytes = codec::movement_info_to_bytes(&info).unwrap();
        (info, bytes)
    }

    #[test]
    fn the_motion_relay_queues_the_bytes_the_typed_path_would_have_sent() {
        let _counters = motion_counter_lock();
        let (info, bytes) = moving_forward();
        let (tx, rx) = crate::world::SessionTx::with_depth(0);
        let created = Arc::new(Mutex::new(rustc_hash::FxHashSet::from_iter([42u64])));
        let opcode = lyracore_shared::opcodes::movement::MSG_MOVE_START_FORWARD;

        relay_entity_motion(&tx, &created, 1, &motion_row(42, opcode as u16, bytes));

        let mut framed = Vec::new();
        codec::build_movement_relay(opcode, 42, info)
            .unwrap()
            .write_unencrypted_server(&mut framed)
            .unwrap();
        match rx
            .try_recv()
            .expect("the observer's writer must have been handed a packet")
        {
            Outbound::Raw { opcode, body } => {
                assert_eq!(
                    opcode,
                    u16::from_le_bytes([framed[2], framed[3]]),
                    "relayed opcode"
                );
                assert_eq!(
                    body,
                    &framed[4..],
                    "relayed body must match the typed serialization"
                );
            }
            Outbound::One(m) => panic!("peer motion must ride Outbound::Raw, got a typed {m}"),
            Outbound::Batch(v) => panic!(
                "peer motion must ride Outbound::Raw, got a {}-batch",
                v.len()
            ),
            Outbound::Job(_) | Outbound::Motion(_) => {
                panic!("peer motion must be encoded by the time it is queued")
            }
            Outbound::Shared { .. } => panic!("direct motion helper returns a raw packet"),
        }
        assert!(rx.try_recv().is_err(), "exactly one packet per motion row");
    }

    #[test]
    fn the_motion_relay_skips_self_and_uncreated_movers() {
        let _counters = motion_counter_lock();
        let (_, bytes) = moving_forward();
        let opcode = lyracore_shared::opcodes::movement::MSG_MOVE_HEARTBEAT as u16;
        let (tx, rx) = crate::world::SessionTx::with_depth(0);
        let created = Arc::new(Mutex::new(rustc_hash::FxHashSet::from_iter([42u64, 7u64])));

        relay_entity_motion(&tx, &created, 7, &motion_row(7, opcode, bytes.clone()));
        assert!(
            rx.try_recv().is_err(),
            "a player's own movement must never be echoed back"
        );

        relay_entity_motion(&tx, &created, 7, &motion_row(999, opcode, bytes));
        assert!(
            rx.try_recv().is_err(),
            "no MSG_MOVE for a guid this client never got a CREATE for"
        );
    }

    #[test]
    fn the_motion_relay_drops_a_truncated_movement_block() {
        let _counters = motion_counter_lock();
        let (_, bytes) = moving_forward();
        let (tx, rx) = crate::world::SessionTx::with_depth(0);
        let created = Arc::new(Mutex::new(rustc_hash::FxHashSet::from_iter([42u64])));
        let opcode = lyracore_shared::opcodes::movement::MSG_MOVE_HEARTBEAT as u16;

        relay_entity_motion(
            &tx,
            &created,
            1,
            &motion_row(42, opcode, bytes[..20].to_vec()),
        );
        assert!(
            rx.try_recv().is_err(),
            "a truncated movement block must not be framed"
        );
    }

    #[test]
    fn a_discarded_relay_send_increments_the_dropped_counter_287() {
        let _counters = motion_counter_lock();
        use std::sync::atomic::Ordering::Relaxed;
        let (_, bytes) = moving_forward();
        let created = Arc::new(Mutex::new(rustc_hash::FxHashSet::from_iter([42u64])));
        let opcode = lyracore_shared::opcodes::movement::MSG_MOVE_HEARTBEAT as u16;
        let before = MOTION_DROPPED.load(Relaxed);

        let (tx, rx) = crate::world::SessionTx::with_depth(0);
        relay_entity_motion(&tx, &created, 1, &motion_row(42, opcode, bytes.clone()));
        assert!(rx.try_recv().is_ok());
        assert_eq!(
            MOTION_DROPPED.load(Relaxed),
            before,
            "a delivered packet is not a drop"
        );

        let (dead_tx, dead_rx) = crate::world::SessionTx::with_depth(0);
        drop(dead_rx);
        relay_entity_motion(&dead_tx, &created, 1, &motion_row(42, opcode, bytes));
        assert_eq!(
            MOTION_DROPPED.load(Relaxed),
            before + 1,
            "a peer-motion packet discarded by a dead writer channel must increment MOTION_DROPPED"
        );

        relay_creature_leg(
            &dead_tx,
            &created,
            &CreatureSpline {
                guid: 42,
                start_micros: 0,
                dur_ms: 500,
                sx: 1.0,
                sy: 2.0,
                sz: 3.0,
                dx: 4.0,
                dy: 5.0,
                dz: 6.0,
                map_id: 0,
                instance_id: 0,
                grid_x: 0,
                grid_y: 0,
                spline_id: 1,
                run: true,
                cell: lyracore_shared::spatial::grid_cell_id(0, 0),
                facing: false,
                facing_angle: 0.0,
                path: None,
            },
        );
        assert_eq!(
            MOTION_DROPPED.load(Relaxed),
            before + 2,
            "a creature leg discarded by a dead writer channel must increment MOTION_DROPPED too"
        );
    }

    #[test]
    fn the_motionstat_line_reports_delivery_dropped_and_fanout_287() {
        let line = motionstat_line(5000, 4500, 21, 1000, 900, 7, 10, 400);
        assert!(
            line.contains("calls=5000"),
            "cumulative calls missing: {line}"
        );
        assert!(
            line.contains("sent=4500"),
            "cumulative sent missing: {line}"
        );
        assert!(
            line.contains("dropped=21"),
            "cumulative dropped count missing: {line}"
        );
        assert!(
            line.contains("+7 in 10s"),
            "the window's dropped delta missing: {line}"
        );
        assert!(
            line.contains("delivery=90.0%"),
            "delivery ratio missing/wrong: {line}"
        );
        assert!(
            line.contains("fanout=100.0/move"),
            "per-movement fan-out missing/wrong: {line}"
        );
        assert!(
            line.contains("submitted=400"),
            "submitted count missing: {line}"
        );
        assert!(
            !line.contains("completed="),
            "retired completed count remains: {line}"
        );
        assert!(
            !line.contains("outstanding="),
            "retired outstanding count remains: {line}"
        );

        let idle = motionstat_line(5000, 4500, 21, 0, 0, 0, 0, 400);
        assert!(
            idle.contains("delivery=--") && idle.contains("fanout=--/move"),
            "{idle}"
        );
    }

    #[test]
    fn fanout_ratio_is_observers_per_move_and_none_when_idle() {
        assert_eq!(fanout_ratio(3710, 10), Some(371.0));
        assert_eq!(
            fanout_ratio(0, 10),
            Some(0.0),
            "delivering to nobody is a real 0, not idle"
        );
        assert_eq!(
            fanout_ratio(500, 0),
            None,
            "no movement submitted = nothing to divide by"
        );
    }

    #[test]
    fn a_quiet_window_neither_warns_nor_rebaselines_b1() {
        let busy = FanoutHealth {
            baseline: Some(300.0),
            low_windows: 2,
        };
        let (next, warn) = fanout_health_step(busy, Some(1.0), crate::world::MOVE_ACTIVITY_FLOOR);
        assert_eq!(warn, None, "a below-floor window must never warn");
        assert_eq!(
            next.baseline,
            Some(300.0),
            "the busy-server baseline must survive a quiet window"
        );
        assert_eq!(
            next.low_windows, 0,
            "and it breaks any collapse run in progress"
        );

        let (idle, warn) = fanout_health_step(busy, None, 0);
        assert_eq!((idle.baseline, warn), (Some(300.0), None));
    }

    #[test]
    fn the_first_window_establishes_the_baseline_and_cannot_warn_b1() {
        let (next, warn) = fanout_health_step(FanoutHealth::default(), Some(40.0), 1_000);
        assert_eq!(
            (next.baseline, next.low_windows, warn),
            (Some(40.0), 0, None)
        );
    }

    #[test]
    fn a_fanout_collapse_warns_only_after_three_consecutive_windows_b1() {
        let mut st = FanoutHealth {
            baseline: Some(300.0),
            low_windows: 0,
        };
        for window in 1..=2 {
            let (next, warn) = fanout_health_step(st, Some(150.0), 1_000);
            assert_eq!(
                warn, None,
                "window {window} of a collapse must stay quiet (hysteresis)"
            );
            assert_eq!(
                next.low_windows, window,
                "the run must be counted while it is quiet"
            );
            st = next;
        }
        let (next, warn) = fanout_health_step(st, Some(150.0), 1_000);
        assert_eq!(
            warn,
            Some(3),
            "the third consecutive collapsed window must warn"
        );
        assert_eq!(
            next.baseline,
            Some(300.0),
            "a collapsed window must NOT be folded into the baseline — a baseline that absorbed the \
             collapse would decay under the current fan-out and the run could never reach 3"
        );
    }

    #[test]
    fn a_healthy_window_resets_the_collapse_run_b1() {
        let mut st = FanoutHealth {
            baseline: Some(300.0),
            low_windows: 0,
        };
        for _ in 0..10 {
            let (low, warn) = fanout_health_step(st, Some(100.0), 1_000);
            assert_eq!(warn, None, "a single collapsed window must never warn");
            let (healthy, warn) = fanout_health_step(low, Some(300.0), 1_000);
            assert_eq!(warn, None);
            assert_eq!(
                healthy.low_windows, 0,
                "a healthy window must clear the run"
            );
            st = healthy;
        }
    }

    #[test]
    fn the_collapse_boundary_and_the_ewma_step_b1() {
        let st = FanoutHealth {
            baseline: Some(100.0),
            low_windows: 2,
        };
        let (next, warn) = fanout_health_step(st, Some(100.0 * FANOUT_COLLAPSE_RATIO), 1_000);
        assert_eq!(
            warn, None,
            "fan-out exactly at the ratio is not yet a collapse"
        );
        assert_eq!(next.low_windows, 0);
        assert!(
            (next.baseline.unwrap() - 88.0).abs() < 1e-9,
            "one EWMA step at alpha {FANOUT_BASELINE_ALPHA}, got {:?}",
            next.baseline
        );
        let (_, warn) = fanout_health_step(st, Some(100.0 * FANOUT_COLLAPSE_RATIO - 0.001), 1_000);
        assert_eq!(
            warn,
            Some(3),
            "a hair below the ratio on window 3 must fire"
        );
    }

    #[test]
    fn a_sustained_collapse_rebaselines_and_stops_warning_b1() {
        let mut st = FanoutHealth {
            baseline: Some(300.0),
            low_windows: 0,
        };
        let mut fired = Vec::new();
        for _ in 1..=8 {
            let (next, warn) = fanout_health_step(st, Some(100.0), 1_000);
            if let Some(n) = warn {
                fired.push(n);
            }
            st = next;
        }
        assert_eq!(
            fired,
            vec![3, 4, 5, 6],
            "the warning must fire on windows 3..6 of a sustained collapse and then adapt"
        );
        assert_eq!(
            st.baseline,
            Some(100.0),
            "the low level becomes the new baseline"
        );
    }

    #[test]
    fn the_egress_shed_boundary_is_strictly_above_the_threshold_b2() {
        assert!(!shed_motion_at_depth(0), "an empty queue must never shed");
        assert!(
            !shed_motion_at_depth(crate::world::EGRESS_SHED_DEPTH),
            "exactly at the threshold is still allowed through"
        );
        assert!(
            shed_motion_at_depth(crate::world::EGRESS_SHED_DEPTH + 1),
            "one past the threshold sheds"
        );
    }

    #[test]
    fn a_backed_up_session_sheds_peer_motion_and_creature_legs_b2() {
        let _counters = motion_counter_lock();
        use std::sync::atomic::Ordering::Relaxed;
        let (_, bytes) = moving_forward();
        let created = Arc::new(Mutex::new(rustc_hash::FxHashSet::from_iter([42u64])));
        let opcode = lyracore_shared::opcodes::movement::MSG_MOVE_HEARTBEAT as u16;
        let leg = CreatureSpline {
            guid: 42,
            start_micros: 0,
            dur_ms: 500,
            sx: 1.0,
            sy: 2.0,
            sz: 3.0,
            dx: 4.0,
            dy: 5.0,
            dz: 6.0,
            map_id: 0,
            instance_id: 0,
            grid_x: 0,
            grid_y: 0,
            spline_id: 1,
            run: true,
            cell: lyracore_shared::spatial::grid_cell_id(0, 0),
            facing: false,
            facing_angle: 0.0,
            path: None,
        };

        let (tx, rx) = crate::world::SessionTx::with_depth(crate::world::EGRESS_SHED_DEPTH);
        relay_entity_motion(&tx, &created, 1, &motion_row(42, opcode, bytes.clone()));
        assert!(
            rx.try_recv().is_ok(),
            "at-threshold peer motion must still flow"
        );
        let (tx, rx) = crate::world::SessionTx::with_depth(crate::world::EGRESS_SHED_DEPTH);
        relay_creature_leg(&tx, &created, &leg);
        assert!(
            rx.try_recv().is_ok(),
            "at-threshold creature legs must still flow"
        );

        let (sent_before, dropped_before) =
            (MOTION_SENT.load(Relaxed), MOTION_DROPPED.load(Relaxed));
        let (tx, rx) = crate::world::SessionTx::with_depth(crate::world::EGRESS_SHED_DEPTH + 1);
        relay_entity_motion(&tx, &created, 1, &motion_row(42, opcode, bytes));
        assert!(
            rx.try_recv().is_err(),
            "PEER MOTION was queued onto a session more than EGRESS_SHED_DEPTH packets behind"
        );
        assert_eq!(
            MOTION_DROPPED.load(Relaxed) - dropped_before,
            1,
            "the shed peer-motion packet must be counted in MOTION_DROPPED"
        );
        let (tx, rx) = crate::world::SessionTx::with_depth(crate::world::EGRESS_SHED_DEPTH + 1);
        relay_creature_leg(&tx, &created, &leg);
        assert!(
            rx.try_recv().is_err(),
            "a CREATURE LEG was queued onto a session more than EGRESS_SHED_DEPTH packets behind"
        );
        assert_eq!(
            MOTION_DROPPED.load(Relaxed) - dropped_before,
            2,
            "the shed creature leg must be counted in MOTION_DROPPED too"
        );
        assert_eq!(
            MOTION_SENT.load(Relaxed),
            sent_before,
            "a shed packet is not a sent one — otherwise `delivery` would still read 100%"
        );
    }

    #[test]
    fn the_egress_depth_counts_queued_items_and_rolls_back_a_failed_send_b2() {
        let (tx, rx) = crate::world::SessionTx::with_depth(0);
        assert_eq!(tx.depth(), 0);
        for expected in 1..=3 {
            tx.send(Outbound::Raw {
                opcode: 1,
                body: vec![],
            })
            .unwrap();
            assert_eq!(tx.depth(), expected, "each queued item must count");
        }
        drop(rx);
        assert!(tx
            .send(Outbound::Raw {
                opcode: 1,
                body: vec![]
            })
            .is_err());
        assert_eq!(
            tx.depth(),
            3,
            "a send that queued nothing must not leave the depth raised"
        );
    }
}

#[cfg(test)]
#[path = "subscriptions_character_gone_durable_tests.rs"]
mod character_gone_durable_tests;

#[cfg(test)]
#[path = "subscriptions_roster_relay_durable_tests.rs"]
mod roster_relay_durable_tests;

#[cfg(test)]
#[path = "subscriptions_meeting_stone_durable_tests.rs"]
mod meeting_stone_durable_tests;

#[cfg(test)]
#[path = "subscriptions_party_command_durable_tests.rs"]
mod party_command_durable_tests;
