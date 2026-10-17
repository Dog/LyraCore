use super::*;
use crate::stdb::aoi::ViewerGates;
use crate::world::{Outbound, SessionTx};
use std::collections::{HashMap, HashSet};
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex};

pub(crate) fn identity(byte: u8) -> spacetimedb_sdk::Identity {
    spacetimedb_sdk::Identity::from_byte_array([byte; 32])
}

pub(crate) fn viewer(session: u64, self_guid: u64) -> Arc<Viewer> {
    let (tx, _rx) = SessionTx::with_depth(0);
    viewer_with_tx(session, self_guid, identity(session as u8), tx)
}

pub(crate) fn viewer_with_tx(
    session: u64,
    self_guid: u64,
    bound_identity: spacetimedb_sdk::Identity,
    tx: SessionTx,
) -> Arc<Viewer> {
    viewer_in_zone(session, self_guid, bound_identity, 0, tx)
}

pub(crate) fn viewer_in_zone(
    session: u64,
    self_guid: u64,
    bound_identity: spacetimedb_sdk::Identity,
    zone_id: u32,
    tx: SessionTx,
) -> Arc<Viewer> {
    Arc::new(Viewer {
        active: std::sync::atomic::AtomicBool::new(true),
        session,
        self_guid,
        bound_identity,
        map_id: 0,
        instance_id: 0,
        zone_id: zone_id.into(),
        tx,
        created: Arc::new(Mutex::new(HashSet::new())),
        gates: Arc::new(ViewerGates::default()),
        skill_slots: Arc::new(Mutex::new((HashMap::new(), 0))),
        explored: Mutex::new(ExplorationReplay::default()),
        motion_pending: Arc::new(MotionPending::default()),
        member_stats: Default::default(),
        ignored: Mutex::default(),
        friends: Mutex::default(),
        team: lyracore_shared::faction::TEAM_ALLIANCE,
        group_events: Default::default(),
    })
}

pub(crate) fn queued_job(rx: &Receiver<Outbound>) -> Vec<Outbound> {
    match rx.try_recv().expect("one addressed writer job") {
        Outbound::Job(job) => job(),
        _ => panic!("shared owner dispatch must enqueue packet work as a writer job"),
    }
}

pub(crate) fn teleport(mover_guid: u64) -> TeleportEvent {
    TeleportEvent {
        id: 1,
        recipient_identity: identity(1),
        mover_guid,
        map_id: 36,
        x: 10.0,
        y: 20.0,
        z: 30.0,
        orientation: 1.5,
        created_micros: 0,
        cross_map: true,
    }
}

pub(crate) fn registered_viewer(
    view: &WorldView,
    shard: usize,
    self_guid: u64,
) -> (Arc<Viewer>, Receiver<Outbound>) {
    registered_viewer_at(view, shard, self_guid, CellKey::at(0, 0, 0, 0))
}

pub(crate) fn registered_viewer_at(
    view: &WorldView,
    shard: usize,
    self_guid: u64,
    cell: CellKey,
) -> (Arc<Viewer>, Receiver<Outbound>) {
    let (tx, rx) = SessionTx::with_depth(0);
    let viewer = viewer_with_tx(
        view.next_session_id(),
        self_guid,
        identity(self_guid as u8),
        tx,
    );
    view.add_viewer_on_shard(viewer.clone(), cell, shard);
    (viewer, rx)
}

pub(crate) fn contact_entry(owner_guid: u64, target_guid: u64, is_ignore: bool) -> ContactEntry {
    ContactEntry {
        id: 1,
        owner_guid,
        owner_identity: spacetimedb_sdk::Identity::ZERO,
        target_guid,
        is_ignore,
    }
}
