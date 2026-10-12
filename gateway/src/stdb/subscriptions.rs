//! World Session registration, queue metrics and durable background work.

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

/// RAII guard for one world session's shared-view registration, held by the world connection.
/// Dropping the guard makes the session unreachable from every shard-level dispatcher.
pub struct PlayerSubscriptions {
    /// This session's registration in the gateway-wide shared view. `None` only for the
    /// in-memory test store. Dropping the guard unregisters it, which is what stops the shared
    /// coordinator dispatch from ever enqueueing for a dead session.
    viewer: Option<Arc<Viewer>>,
    view: Option<Arc<WorldView>>,
}

impl PlayerSubscriptions {
    /// A no-op guard (used by the in-memory test store; drops harmlessly).
    #[cfg(test)]
    pub fn empty() -> Self {
        Self {
            viewer: None,
            view: None,
        }
    }

    /// Build a real shared-view registration for world-session ordering tests.
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
            created: Arc::new(Mutex::new(HashSet::from([self_guid]))),
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

    /// Remove this session from owner, identity, shard, and spatial routing immediately.
    ///
    /// World-port uses this before driving a cross-shard transfer because the drive can delete
    /// owner rows on the source shard. `Drop` calls the same method, so failure and re-entry paths
    /// remain idempotent.
    pub(crate) fn unregister_viewer(&mut self) {
        if let (Some(view), Some(viewer)) = (self.view.take(), self.viewer.take()) {
            viewer
                .active
                .store(false, std::sync::atomic::Ordering::Release);
            view.remove_viewer(viewer.session);
        }
    }

    /// What this session's client holds of its group mates' Member Stats. `None` for the in-memory
    /// test store.
    pub(crate) fn member_stats_record(&self) -> Option<&crate::world::MemberStatsRecord> {
        self.viewer.as_ref().map(|viewer| &viewer.member_stats)
    }

    /// Drive the shared AOI index from the player's movement: on a cell crossing, move the
    /// viewer's anchor and relay the CREATE/DESTROY delta the move implies.
    ///
    /// Runs on the session's own thread and does no I/O: the delta is a 10-cell set diff in memory
    /// and the entering rows are read out of coordinator caches that are already resident. Before
    /// Before the shared-connection model, this was an unsubscribe + resubscribe round trip per
    /// cell crossing, per player.
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

/// Diagnosis counters for a failure where the peer-movement relay died permanently on a session —
/// the motion subscription stopped delivering while the module kept accepting movement. Deliberately
/// atomics read ONCE at logout rather than a log per call: a
/// `log::info!` on the relay path made the intermittent failure vanish across six consecutive runs,
/// so any instrument that touches the hot path perturbs the very race being measured.
///
/// `MOTION_CALLS` counts per-viewer [`motion_outbound`] work before its audience Gate.
/// `MOTION_SENT` counts packets built, and `MOTION_DROPPED` counts discarded writer work.
/// These are fan-out counters. Incoming subscription rows are counted separately per Shard.
pub(crate) static MOTION_CALLS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
pub(crate) static MOTION_SENT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

pub(crate) static MOTION_DROPPED: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);

// One argument per counter in the diagnostic line this formats.
#[allow(clippy::too_many_arguments)]
/// The `MOTIONSTAT` 10-second line — pure, so what the operator reads is pinned by a
/// test instead of by eyeballing a `log::info!`. Deltas are the CHANGE over the last window, which is
/// what makes a degradation visible while the cumulative totals keep climbing.
///
/// `delivery` is `sent/calls` over the window: the relay's own guard-rejection rate. `fanout` is
/// `calls per submitted movement` — the observers-per-mover the subscription engine actually
/// delivered to. That second number is the one the 371-client run needed and did not have: at 371
/// co-located clients each submitted movement SHOULD wake ~370 relay callbacks, and the under-
/// delivery showed up as that ratio collapsing, not as a guard rejecting anything.
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

/// Observers per submitted movement over one window — `None` for a window with no movement at all
/// (an idle server must not read as a fan-out of zero). ONE definition, shared by the `MOTIONSTAT`
/// line and the collapse check below, so the number the operator reads and the number the warning
/// fires on can never disagree.
pub(crate) fn fanout_ratio(d_calls: u64, d_submitted: u64) -> Option<f64> {
    (d_submitted > 0).then(|| d_calls as f64 / d_submitted as f64)
}

/// How far below its own trailing baseline the fan-out must fall to count as collapsed. 0.6 = "at
/// least 40 % of the observers a mover was reaching ten seconds ago are no longer being reached".
/// Well outside the wobble of a live crowd (players logging in and out, AOI boxes recentering, a
/// window that lands mid-teleport), so it takes an actual step change to reach.
pub(crate) const FANOUT_COLLAPSE_RATIO: f64 = 0.6;

/// Consecutive collapsed windows before the warning fires — 3 × 10 s, so ~30 s of sustained
/// under-delivery. THE HYSTERESIS IS A DELIBERATE DIVERGENCE FROM THE STUCK-RELAY SIGNAL (the
/// `MOTION_CALLS`/`MOTION_SENT` diagnosis above), which has none
/// and re-fires every window: that signal keys on "movement submitted but the relay callback fired almost
/// never", which is a binary, unambiguous, does-not-self-heal condition worth shouting about
/// instantly. Fan-out is a RATIO of two noisy counters over a 10-second window, and it moves for
/// legitimate reasons every time the crowd changes shape. A signal like that fires spuriously if it
/// is instant, and a warning that has cried wolf once is a warning the operator greps away.
pub(crate) const FANOUT_COLLAPSE_WINDOWS: u32 = 3;

/// After this many consecutive collapsed windows the current (low) fan-out is accepted as the new
/// baseline, which silences the warning until the NEXT collapse. Deliberate: fan-out also drops
/// permanently for the entirely legitimate reason that the crowd thinned, and an alarm that never
/// stops is an alarm nobody reads. So the operator gets windows 3..6 of the collapse — ~30 s of loud
/// warnings — and then the gateway adapts.
pub(crate) const FANOUT_REBASELINE_WINDOWS: u32 = 6;

/// Weight of the newest healthy window in the trailing baseline. An EWMA rather than a ring of
/// recent windows because it needs no allocation and no window bookkeeping, and because the
/// smoothing constant is the only tunable: 0.3 over 10-second windows ≈ a 30-second memory, fast
/// enough to follow a crowd filling up a zone and slow enough that one quiet window does not become
/// the reference point. Note the baseline is fed ONLY by non-collapsed windows (see
/// [`fanout_health_step`]) — an EWMA that absorbed the collapse would decay under the current
/// fan-out within two windows and the run would never reach [`FANOUT_COLLAPSE_WINDOWS`].
pub(crate) const FANOUT_BASELINE_ALPHA: f64 = 0.3;

/// Trailing fan-out baseline + the length of the current run of collapsed windows. Lives in the
/// 10-second `MOTIONSTAT` task; nothing on the relay hot path touches it.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct FanoutHealth {
    /// `None` until the first informative window establishes it.
    pub(crate) baseline: Option<f64>,
    pub(crate) low_windows: u32,
}

/// One 10-second window of the fan-out health check — pure, so the decision is unit-tested directly
/// instead of being eyeballed in a log. Returns the next state, and
/// `Some(consecutive_collapsed_windows)` when this window should warn (`None` = stay quiet).
///
/// **What it catches.** Peer movement being delivered to fewer observers than it was a moment ago,
/// while players are still moving — the 371-client shape, where nothing errored, nothing logged, and
/// peers simply updated less often than they moved. `delivery` (= sent/calls) read ~100 % throughout
/// that run, because the shortfall was `calls` never reaching the rate it should have; `fanout` is
/// the field that showed it, and this turns that field from "a number an operator has to know the
/// expected value of" into a warning.
///
/// **What it does NOT catch — read this before trusting it.**
/// * **A gateway that has been degraded since startup.** The baseline is the gateway's OWN recent
///   behaviour, so if fan-out was already wrong when the first window was measured, that wrong value
///   IS the baseline and nothing ever trips. This check reports a CHANGE, never an absolute truth.
///   Nothing in the gateway currently knows how many observers a mover *should* have reached —
///   the AOI box is per-session, invisible to this task, and `GridBox` has no
///   overlap test, so a true expectation needs a global registry of per-session boxes plus box
///   intersection. That is the honest version of this warning and it is not what this is.
/// * **A slow slide.** A degradation that takes minutes to develop keeps each window within
///   [`FANOUT_COLLAPSE_RATIO`] of the last, and the baseline follows it down. Only a step change of
///   ≥40 % inside ~30 s trips.
/// * **A collapse that starts during a quiet spell.** Windows below
///   [`crate::world::MOVE_ACTIVITY_FLOOR`] submitted movements are treated as uninformative: they do
///   not warn, and they break a run in progress.
/// * **Under-delivery to SOME sessions.** Both counters are process-global, so one wedged session
///   among 300 healthy ones is invisible here.
///
/// A TOTAL loss of the relay trips this as well as the stuck-relay signal (fan-out goes to ~0, which is
/// certainly below any baseline) — two lines about one failure, 30 s apart, which is the right way
/// round: the stuck-relay signal is instant and keeps repeating, this one adds "and it stayed that way".
pub(crate) fn fanout_health_step(
    state: FanoutHealth,
    fanout: Option<f64>,
    submitted_delta: u64,
) -> (FanoutHealth, Option<u32>) {
    // Uninformative window: too little movement to compare, or none at all. Keep the baseline (a
    // quiet window is not evidence about fan-out) but break any run in progress — the same activity
    // floor the stuck-relay signal uses, for the same reason: a handful of movements with little relay
    // traffic is one lonely player, not a broken relay.
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
        // First informative window: nothing to compare against yet.
        return (
            FanoutHealth {
                baseline: Some(f),
                low_windows: 0,
            },
            None,
        );
    };
    if f >= baseline * FANOUT_COLLAPSE_RATIO {
        // Healthy window — fold it into the trailing baseline and forget any partial run.
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
        // Warned for ~30 s and it is still low: accept this as the new normal (see
        // FANOUT_REBASELINE_WINDOWS) — this window still warns, the next one will not.
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

/// **Should this peer-motion packet be dropped instead of queued?** The one decision behind the
/// per-session egress bound (task B2) — pure and unit-tested, called with `tx.depth()`.
///
/// The channel behind [`crate::world::SessionTx`] is unbounded, so a session whose socket has
/// stopped taking bytes grows its backlog for as long as the stall lasts: a memory path under crowd
/// load, and the reason `MOTION_DROPPED` could not mean backpressure before this. Bounding the
/// CHANNEL is not available to us — ~110 of the send sites run inside SpacetimeDB SDK row callbacks
/// on the connection's pump thread, and a blocking send from there on a full channel would deadlock
/// that connection (see `SessionTx`'s own comment). So the queue stays unbounded and the only
/// traffic that can plausibly outrun a socket sheds itself.
///
/// **Why only peer motion and creature legs, out of ~180 send sites.** They are the N²-shaped
/// traffic: one packet per OBSERVER per mover per heartbeat, ~740/s per session at 371 co-located
/// clients, versus a handful per player action for everything else. And they are the only sends that
/// are SELF-HEALING when lost — a movement heartbeat is superseded by the next one 500 ms later, so
/// a shed packet costs a moment of coarser interpolation on a peer that is already being rendered
/// from stale data (the queue is half a second deep). A quest-log page, an item CREATE, a
/// `SMSG_DESTROY_OBJECT` is not superseded by anything: dropping one leaves the client permanently
/// wrong. Those keep queueing exactly as before.
///
/// Shedding is counted in [`MOTION_DROPPED`], so it is visible on the existing `MOTIONSTAT` line as
/// both a rising `dropped` and a falling `delivery` — a shed that fires is not silent.
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

    /// Prepare and register one live viewer. Row callbacks are already armed once per shard in
    /// `world_view::arm_shard`; this method registers no callback of its own.
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
        // Ghost-only visibility constants (vanilla 1.12): a spirit-healer creature carries
        // UNIT_NPC_FLAGS SPIRITHEALER and is visible ONLY to a viewer in the GHOST player-flag state.
        // BUG FIX: was 0x4000 — but 0x4000 is REPAIR in cmangos 1.12 numbering (SPIRITHEALER is 0x20).
        // The wrong constant (a) failed to ghost-gate the real Spirit Healer (npc_flags 0x21) and
        // (b) wrongly ghost-gated the 39 armorers/quartermasters that carry REPAIR (0x4000), hiding
        // them from living players.
        let created = Arc::new(Mutex::new(HashSet::from([self_guid])));
        // The view-merge fast-follow gap: the one piece of viewer-owned state the away leg's
        // peer-visibility gate needs but cannot read off an away connection. Written by the ghost-flag
        // relay (this viewer's OWN row), read by every per-viewer relay body in this file — a
        // shared coordinator callback holds the cache of the shard that owns the ROW, which for a
        // cross-shard peer is not the shard that owns the VIEWER. See `ViewerGates`'s doc comment.
        let viewer_gates = Arc::new(ViewerGates::default());
        // This session's handle on the gateway-wide shared view. Minted HERE, at the top,
        // because relays registered further down capture it — the actual REGISTRATION (which makes
        // the shared dispatch start enqueueing for this session) happens near the end, once
        // everything that could still fail with `?` has succeeded.
        let view = self.world_view();
        let session = view.next_session_id();

        // ======================================================================================
        //  SKILL-UP — game_player_skill (insert/update)
        //  Live PLAYER_SKILL_INFO[slot] VALUES so a newly-learned or trained skill line updates the
        //  open pane without a relog.
        // ======================================================================================
        // LIVE skill pane relay: a game_player_skill INSERT (new line learned) or UPDATE
        // (skill-up/train) pushes the PLAYER_SKILL_INFO[slot] partial so the open pane moves and
        // the client prints its own "Your skill in X has increased to N." line — no relog. The
        // slot map is seeded from the SAME deterministic layout the login CREATE used
        // (codec::skill_slot_layout); a line learned mid-session appends at the next free slot,
        // exactly as the next login's CREATE will lay it out.
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

        // The bound identity WITHOUT touching a per-player connection — synthetic under the
        // flag (what establish_session bound and the module stamps on event rows), the cached
        // connection's real identity otherwise. Identical values to the old `player.identity` read.
        // Stored with the viewer for identity-addressed shared dispatch.
        let self_identity =
            spacetimedb_sdk::Identity::from_byte_array(self.bound_identity(account_id)?);

        {
            // Seed the viewer's replay state before registration. A coordinator reconnect replays
            // every resident row: exploration is state-sync (the fog word must re-send, the
            // discovery feedback must not repeat), so it carries this per-viewer seed, while the
            // event families (teleport, faction announce, item toast) drop replayed rows outright
            // in `world_view`'s live-insert registrations.
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

        // Register with the gateway-wide shared view, then sweep everything already inside
        // the box into this client's world.
        //
        // The sweep is not belt-and-braces here, it is the ONLY thing that populates a fresh
        // client: there is no subscription apply to fire per-row `on_insert` any more — the rows
        // were resident in the coordinator caches long before this session existed. It is
        // idempotent through `created` regardless.
        // Seed the ghost mirror from the character's OWN row before anything can read it. Before
        // the shared-connection model, the spirit-healer gate re-read the live row on every offer,
        // so a player who logged in
        // ALREADY DEAD saw the healer without any ghost transition ever firing; the mirror has to
        // start at the truth or that login regresses.
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
        // The Realm Chat Relay's per-listener filter, and the Account Claim Relay's per-listener
        // audience: this Character's own ignore and friend guids, from its contact rows on this
        // Home Shard. The contact Relay keeps both current from here on. Read from the Shard's
        // owner-keyed `ContactIndex` rather than `contact_lists`, which reads the registered Viewer
        // this call is about to seed.
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

        // Corpse resident sweep: corpse rows ride the base subscription
        // and their callbacks may have fired before this point. Re-offer every resident one through
        // the same instance gate — idempotent for the client (a repeat CREATE for the same corpse
        // guid re-renders it), and the gate is now the session's authoritative `login_instance`
        // rather than a cache lookup that could answer `None`.
        //
        // Read from the COORDINATOR cache: same global rows in both flag states,
        // while the per-player cache stops carrying the corpse query under the flag.
        let resident_corpses: Vec<Corpse> = {
            let guard = self.0.coord();
            guard.conn.db.game_corpse().iter().collect()
        };
        for row in &resident_corpses {
            if instance_relay_gate(row.instance_id, Some(login_instance)) {
                relay_corpse_create(&tx, self_guid, row);
            }
        }
        // Armor/sheet resident sweep (login-low-armor live find, 2026-07-11): the base sub's ITEM
        // and aura replay callbacks fire before the self entity is readable, so `effective_armor`'s
        // base term read 0 and their pushes OVERWROTE the CREATE's correct gear-folded armor. Push
        // the authoritative values once, from the COORDINATOR's cache — which holds the entity, the
        // auras, the item instances AND the item templates, i.e. every term of the fold (the
        // per-player cache lost `game_item_template` to the connection reclaim and has never held
        // the entity since the shared-connection model).
        {
            let guard = self.0.coord();
            let db = &guard.conn.db;
            let eff = super::armor::effective_resistances(db, self_guid);
            let pos = super::armor::aura_armor_positive(db, self_guid);
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
        // Login fog restore: with no per-player subscription there
        // is no initial-sync replay to re-send the fog words, so sweep them explicitly from the
        // coordinator cache — one idempotent full-word PLAYER_EXPLORED_ZONES VALUES per explored
        // 32-bit bucket of THIS character.
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
    /// Wire up the bot-initiated (serendipity) invite relay: one registration per
    /// connected WORLD SHARD — `all_shards()`, not a per-player relay — because there is no player
    /// session to hang this off. A bot's goal tick decides "invite this fellow quester" with no
    /// client behind it, so nothing else in the gateway would ever notice the row.
    ///
    /// Called ONCE, at gateway startup (`main.rs`). Also installs its own reconnect hook, parallel
    /// to `world_view::arm_shard`, so a module republish binds the callback to the fresh `LiveConn`.
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

    /// One shard's half of [`spawn_bot_invite_relay`] — pulled out so both the initial call and the
    /// watchdog's post-reconnect re-arm run the IDENTICAL registration, rather than two copies that
    /// could drift. Re-reads `self.0.coord()` fresh each call, which is the whole point: called again
    /// after a reconnect, it registers on the NEW `LiveConn`, not the dead one the old callback was
    /// bound to.
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
                // The claim targets this same Shard. Run it off the SDK callback thread so a
                // one-pipe Gateway can receive the reducer completion instead of blocking its own
                // connection pump.
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

    /// Start the Roster Revision Relay: Realm-core's `game_group_roster_revision` callbacks mark a
    /// party dirty, and one worker pushes it to every stale World Shard mirror. Called once at
    /// startup. Arms nothing unless Realm-core is a distinct database. A Realm-core reconnect
    /// re-arms the callbacks on the fresh connection; changes missed while it was down are the
    /// reconciliation pass's, which that reconnect also starts.
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

    /// Realm-core's half of [`Self::spawn_roster_revision_relay`], on the current connection. The
    /// callbacks only mark the party dirty; the worker makes every reducer call.
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

    /// Start one bounded Transfer Intent dispatcher per configured World Shard. Each pass reads the
    /// current connection, so a reconnect needs no second thread or connection-scoped callback.
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

    /// Reconcile party and guild membership when a World Shard deletes a Character.
    ///
    /// Fresh Character subscriptions on every Shard distinguish deletion from a Transfer whose
    /// destination update has not reached this Gateway yet.
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

    /// A full pass: every party member and every Character guild state names. Startup and a
    /// reconnect ask for it, since a delete may have gone unseen.
    fn request_deleted_character_reconciliation(&self) {
        self.1.guild_cleanup.lock().unwrap().sweep = true;
        self.start_deleted_character_reconciliation();
    }

    /// A `game_character` delete: the party pass, and guild cleanup for `character_guid` only.
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
                    // Both run on every pass: a party failure must not hold up guild cleanup.
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
    use std::collections::HashSet;
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

    // ── Peer-motion relay: the CALL SITE, not just the codec helper ──────────
    //
    // §8 of the agent playbook: the dominant failure here is a test that pins an extracted helper
    // while the wiring that calls it stays free to vanish. `codec::tests` proves the raw builder
    // emits the same bytes the typed path did; these tests drive `relay_entity_motion` /
    // `relay_creature_leg` themselves — a real `SessionTx`, a real `created` set, a real row —
    // so deleting the send, sending the wrong variant, passing the wrong guid or opcode, or dropping
    // a guard all show up here.

    /// `MOTION_CALLS`/`SENT`/`DROPPED` are process-global atomics and the test binary runs tests in
    /// PARALLEL, so any test that moves them or measures a delta around them must hold this lock.
    /// Without it, the delta a counter test measures silently includes whatever a concurrent relay
    /// test did (which is exactly how the shed test below first turned the motion-counters test red). Poison is
    /// ignored deliberately: a panicking test has already failed and must not cascade into every
    /// other one.
    static MOTION_COUNTERS: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Test shims for the two relays the shared-connection model split in half.
    ///
    /// Before the shared-connection model, each of these pushed straight onto the session channel
    /// from the per-player SDK
    /// callback. Now the SHARED dispatch (`stdb::world_view`) makes the shed decision on the pump
    /// and enqueues a job, and the job calls the `*_outbound` half on the session's writer thread.
    /// These shims are that pair, back to back, so the wire-pinning and counter tests below keep
    /// asserting on the queue exactly as they did — and keep failing if either half regresses.
    fn relay_entity_motion(
        tx: &SessionTx,
        created: &Arc<Mutex<HashSet<u64>>>,
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
        created: &Arc<Mutex<HashSet<u64>>>,
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

    /// One motion row for `guid`, carrying the wire-form `MovementInfo` the module stores.
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

    /// A running-forward movement block plus the bytes the module would have stored for it.
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

    /// What the observer's writer actually receives for one peer heartbeat: an `Outbound::Raw` whose
    /// opcode and body equal what the TYPED path would have serialized. The typed
    /// message is built here independently of the raw builder, so this fails if the relay passes the
    /// wrong guid, the wrong opcode, the row's bytes in the wrong place — or stops sending at all.
    #[test]
    fn the_motion_relay_queues_the_bytes_the_typed_path_would_have_sent() {
        let _counters = motion_counter_lock();
        let (info, bytes) = moving_forward();
        let (tx, rx) = crate::world::SessionTx::with_depth(0);
        let created = Arc::new(Mutex::new(HashSet::from([42u64])));
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
            Outbound::Job(_) => panic!("peer motion must be encoded by the time it is queued"),
        }
        assert!(rx.try_recv().is_err(), "exactly one packet per motion row");
    }

    /// The two guards that were free before the raw path and must stay: a player never receives
    /// their OWN movement echoed back (they would fight the server for authority over their
    /// position), and no `MSG_MOVE_*` goes out for a guid this client never got a CREATE for
    /// (a movement packet for an unknown entity desyncs the client).
    #[test]
    fn the_motion_relay_skips_self_and_uncreated_movers() {
        let _counters = motion_counter_lock();
        let (_, bytes) = moving_forward();
        let opcode = lyracore_shared::opcodes::movement::MSG_MOVE_HEARTBEAT as u16;
        let (tx, rx) = crate::world::SessionTx::with_depth(0);
        let created = Arc::new(Mutex::new(HashSet::from([42u64, 7u64])));

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

    /// A malformed row must produce no packet at all — the raw path memcpys the block, so a short
    /// one would be framed with a `size` the client trusts and would desync its header stream.
    #[test]
    fn the_motion_relay_drops_a_truncated_movement_block() {
        let _counters = motion_counter_lock();
        let (_, bytes) = moving_forward();
        let (tx, rx) = crate::world::SessionTx::with_depth(0);
        let created = Arc::new(Mutex::new(HashSet::from([42u64])));
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

    ///
    /// Deltas rather than absolutes because the counter is a process-global atomic; this is the only
    /// test that increments it, so the deltas are exact.
    #[test]
    fn a_discarded_relay_send_increments_the_dropped_counter_287() {
        let _counters = motion_counter_lock();
        use std::sync::atomic::Ordering::Relaxed;
        let (_, bytes) = moving_forward();
        let created = Arc::new(Mutex::new(HashSet::from([42u64])));
        let opcode = lyracore_shared::opcodes::movement::MSG_MOVE_HEARTBEAT as u16;
        let before = MOTION_DROPPED.load(Relaxed);

        // A live receiver: delivered, so nothing is counted as dropped.
        let (tx, rx) = crate::world::SessionTx::with_depth(0);
        relay_entity_motion(&tx, &created, 1, &motion_row(42, opcode, bytes.clone()));
        assert!(rx.try_recv().is_ok());
        assert_eq!(
            MOTION_DROPPED.load(Relaxed),
            before,
            "a delivered packet is not a drop"
        );

        // The writer half is gone (the session ended) — the send is discarded and MUST be counted.
        let (dead_tx, dead_rx) = crate::world::SessionTx::with_depth(0);
        drop(dead_rx);
        relay_entity_motion(&dead_tx, &created, 1, &motion_row(42, opcode, bytes));
        assert_eq!(
            MOTION_DROPPED.load(Relaxed),
            before + 1,
            "a peer-motion packet discarded by a dead writer channel must increment MOTION_DROPPED"
        );

        // The sibling relay discards the same way (creature legs share the writer channel).
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

    /// The operator-facing half of the delivery instrument: the 10-second line must carry the
    /// delivery ratio, the
    /// per-movement fan-out, and the dropped count — the three numbers the 371-client run had to
    /// reconstruct by hand afterwards. Pinned as text because that text IS the instrument.
    #[test]
    fn the_motionstat_line_reports_delivery_dropped_and_fanout_287() {
        // 1000 callbacks, 900 packets queued, 7 discarded, from 10 submitted movements.
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

        // An idle window must not divide by zero or print a fake 0 %.
        let idle = motionstat_line(5000, 4500, 21, 0, 0, 0, 0, 400);
        assert!(
            idle.contains("delivery=--") && idle.contains("fanout=--/move"),
            "{idle}"
        );
    }

    // ── Fan-out collapse signal ─────────────────────────────────────────────────────────────────

    /// The shared ratio: observers per submitted movement, and `None` — never a fake 0.0 — for a
    /// window with no movement in it. The `MOTIONSTAT` line and the warning both read this, which is
    /// the point: a number the operator sees and a number the gateway warns on cannot disagree.
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

    /// A quiet window is not evidence. Below `MOVE_ACTIVITY_FLOOR` submitted movements the check must
    /// stay silent AND keep the baseline it learned while the server was busy — one lone player
    /// wandering with nobody nearby must not become the reference point for a full zone.
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

        // No movement at all: same treatment, no divide-by-zero fake.
        let (idle, warn) = fanout_health_step(busy, None, 0);
        assert_eq!((idle.baseline, warn), (Some(300.0), None));
    }

    /// The first informative window can only ESTABLISH the baseline — there is nothing to compare it
    /// against. This is also the honest limit of the whole signal: a gateway that is already
    /// under-delivering when it starts up bakes that into the baseline and never trips.
    #[test]
    fn the_first_window_establishes_the_baseline_and_cannot_warn_b1() {
        let (next, warn) = fanout_health_step(FanoutHealth::default(), Some(40.0), 1_000);
        assert_eq!(
            (next.baseline, next.low_windows, warn),
            (Some(40.0), 0, None)
        );
    }

    /// **The hysteresis, which is the deliberate divergence from the stuck-relay signal.** A 40 %+ fan-out
    /// collapse must persist for `FANOUT_COLLAPSE_WINDOWS` consecutive windows before it says
    /// anything: the ratio wobbles, and a warning that cries wolf gets grepped away. Windows 1 and 2
    /// are silent; window 3 warns and reports the run length.
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

    /// Half a collapse is not a collapse: one bad window followed by a healthy one resets the run, so
    /// an alternating wobble never fires however long it goes on.
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

    /// Just above the ratio is NOT a collapse (the boundary), and a healthy window moves the trailing
    /// baseline by exactly one EWMA step — the mechanism that lets the signal follow a zone filling up
    /// instead of firing on it.
    #[test]
    fn the_collapse_boundary_and_the_ewma_step_b1() {
        let st = FanoutHealth {
            baseline: Some(100.0),
            low_windows: 2,
        };
        // Exactly at the ratio: healthy, so the run resets rather than firing on window 3.
        let (next, warn) = fanout_health_step(st, Some(100.0 * FANOUT_COLLAPSE_RATIO), 1_000);
        assert_eq!(
            warn, None,
            "fan-out exactly at the ratio is not yet a collapse"
        );
        assert_eq!(next.low_windows, 0);
        // baseline 100 + 0.3 * (60 - 100) = 88
        assert!(
            (next.baseline.unwrap() - 88.0).abs() < 1e-9,
            "one EWMA step at alpha {FANOUT_BASELINE_ALPHA}, got {:?}",
            next.baseline
        );
        // A hair below it, on the third window, does fire.
        let (_, warn) = fanout_health_step(st, Some(100.0 * FANOUT_COLLAPSE_RATIO - 0.001), 1_000);
        assert_eq!(
            warn,
            Some(3),
            "a hair below the ratio on window 3 must fire"
        );
    }

    /// After `FANOUT_REBASELINE_WINDOWS` the low level is accepted as the new normal, so the warning
    /// stops. Deliberate and documented: fan-out also drops permanently because the crowd thinned,
    /// and an alarm that never stops is one nobody reads. It fires for windows 3..6 — ~30s of loud
    /// warnings — and then goes quiet with the new baseline in place.
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

    // ── Egress shedding (task B2) ──────────────────────────────────────────────────────────────────

    /// The shed decision and its boundary. A session AT the threshold still gets its packet — only a
    /// queue that has grown PAST it sheds — so the constant reads as "this many queued packets are
    /// acceptable", which is how the comment on it is written.
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

    /// The shed at its CALL SITE (playbook §8: pinning the pure decision while the wiring stays free
    /// to vanish is this repo's most repeated test defect). A real `SessionTx` whose queue is one item
    /// past the threshold must produce NO packet from either high-volume relay, and each skip must be
    /// counted in `MOTION_DROPPED` — so a shed shows up on `MOTIONSTAT` as falling `delivery` instead
    /// of being silent.
    #[test]
    fn a_backed_up_session_sheds_peer_motion_and_creature_legs_b2() {
        let _counters = motion_counter_lock();
        use std::sync::atomic::Ordering::Relaxed;
        let (_, bytes) = moving_forward();
        let created = Arc::new(Mutex::new(HashSet::from([42u64])));
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

        // A session AT the threshold still receives its packet (this half is what stops the test from
        // passing for the trivial reason that the relays send nothing at all). One `SessionTx` each,
        // because a delivered packet raises the depth by one — the second send on a shared sender
        // would already be over the line, which is the mechanism working, not a failure.
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

        // One item past it: each relay skips, and each counts its skip. Driven SEPARATELY so a
        // failure names which of the two call sites lost its shed.
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

    /// The depth counter's own arithmetic: every enqueue counts, and a send that FAILS (the writer
    /// half is gone) must roll its increment back. Without the rollback a session that ends while
    /// relays are still firing would leave a permanently high depth — on a live gateway the
    /// `SessionTx` clones outlive the writer briefly on every logout.
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
