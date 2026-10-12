//! Table Relays for this Protocol Family.

use crate::codec::{self};
use crate::stdb::bindings::*;
use crate::stdb::subscriptions::{MOTION_CALLS, MOTION_SENT};
use crate::stdb::world_index::{CellKey, ShardId};
use crate::stdb::world_view::*;
use crate::world::Outbound;
use anyhow::Result;
use lyracore_shared::spatial::BOX_HALF_SPAN;
use std::collections::HashSet;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};
use wow_world_messages::vanilla::opcodes::ServerOpcodeMessage;
use wow_world_messages::vanilla::Vector3d;

/// Build the wire relay for one `game_teleport_event` row. Pure: takes the
/// already-derived `still_here` boolean (whether the live entity survived the transaction — the
/// module/gateway-mirrored same-map/cross-map signal, see the `on_teleport` callback's doc comment)
/// instead of a live `ReducerContext`/connection, so it's testable without a DB (module crate
/// convention: extract pure functions and test those).
///
/// `still_here` → same-map: `MSG_MOVE_TELEPORT_ACK` (byte-identical to the previous behavior — the ONLY
/// caller-visible difference from before is that this is now reached via a branch rather than
/// unconditionally). NOT `still_here` → cross-map: `SMSG_TRANSFER_PENDING`+`SMSG_NEW_WORLD` as one batch
/// (`codec::build_cross_map_teleport`). `Err` only for a `map_id` the client's `Map` enum doesn't know
/// (propagated so the caller can log it — never silently drops the relay).
pub(crate) fn build_teleport_relay(
    still_here: bool,
    mover_guid: u64,
    map_id: u32,
    x: f32,
    y: f32,
    z: f32,
    o: f32,
) -> Result<Outbound> {
    if still_here {
        let m = codec::build_teleport_ack(mover_guid, 0, x, y, z, o);
        Ok(Outbound::One(ServerOpcodeMessage::MSG_MOVE_TELEPORT_ACK(
            Box::new(m),
        )))
    } else {
        let [pending, new_world] = codec::build_cross_map_teleport(map_id, x, y, z, o)?;
        Ok(Outbound::Batch(vec![pending, new_world]))
    }
}

/// Build the complete teleport result on the addressed session's writer queue.
pub(crate) fn teleport_event_outbound(row: &TeleportEvent) -> Vec<Outbound> {
    match build_teleport_relay(
        !row.cross_map,
        row.mover_guid,
        row.map_id,
        row.x,
        row.y,
        row.z,
        row.orientation,
    ) {
        Ok(out) => vec![out],
        Err(error) => {
            log::warn!("teleport relay: {error} (guid {})", row.mover_guid);
            Vec::new()
        }
    }
}

pub(crate) fn motion_outbound(
    created: &Mutex<HashSet<u64>>,
    self_guid: u64,
    row: &EntityMotion,
) -> Vec<Outbound> {
    MOTION_CALLS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    if row.guid == self_guid || !created.lock().unwrap().contains(&row.guid) {
        return Vec::new();
    }
    match codec::build_movement_relay_raw(row.opcode as u32, row.guid, &row.movement_info) {
        Some((opcode, body)) => {
            MOTION_SENT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            vec![Outbound::Raw { opcode, body }]
        }
        None if lyracore_shared::opcodes::movement::is_slice_move(row.opcode as u32) => {
            log::warn!(
                "motion relay: malformed movement_info ({} bytes) for mover {}",
                row.movement_info.len(),
                row.guid
            );
            Vec::new()
        }
        None => Vec::new(),
    }
}

pub(crate) fn creature_leg_outbound(
    created: &Mutex<HashSet<u64>>,
    row: &CreatureSpline,
) -> Vec<Outbound> {
    if !created.lock().unwrap().contains(&row.guid) {
        return Vec::new();
    }
    let start = Vector3d {
        x: row.sx,
        y: row.sy,
        z: row.sz,
    };
    // a facing-only leg (the mover doesn't move, `emit_facing_spline`'s carrier) gets the
    // `FacingAngle` variant — the only 1.12 `SMSG_MONSTER_MOVE` shape that turns a STATIONARY
    // creature without a spline the client would otherwise have nothing to interpolate from.
    if row.facing {
        return vec![Outbound::One(ServerOpcodeMessage::SMSG_MONSTER_MOVE(
            Box::new(codec::build_monster_move_facing(
                row.guid,
                start,
                row.facing_angle,
                row.spline_id,
            )),
        ))];
    }
    if let Some(path) = &row.path {
        let points = &path.points;
        let points: Vec<_> = points.iter().map(|p| (p.x, p.y, p.z)).collect();
        if let Some((opcode, body)) = codec::build_ground_path_raw(
            row.guid,
            start,
            &points,
            row.dur_ms,
            row.spline_id,
            row.run,
        ) {
            return vec![Outbound::Raw { opcode, body }];
        }
        log::warn!(
            "refused malformed creature path for guid {} spline {}",
            row.guid,
            row.spline_id
        );
        if [start.x, start.y, start.z].iter().all(|v| v.is_finite()) {
            return vec![Outbound::One(ServerOpcodeMessage::SMSG_MONSTER_MOVE(
                Box::new(codec::build_monster_move(
                    row.guid,
                    start,
                    start,
                    0,
                    row.spline_id,
                    false,
                )),
            ))];
        }
        return Vec::new();
    }
    let dest = Vector3d {
        x: row.dx,
        y: row.dy,
        z: row.dz,
    };
    vec![Outbound::One(ServerOpcodeMessage::SMSG_MONSTER_MOVE(
        Box::new(codec::build_monster_move(
            row.guid,
            start,
            dest,
            row.dur_ms,
            row.spline_id,
            row.run,
        )),
    ))]
}

pub(crate) fn append_resident_creature_after_create(
    created_outbound: &mut Vec<Outbound>,
    created: &Mutex<HashSet<u64>>,
    viewer_instance_id: u64,
    row: Option<&CreatureSpline>,
    now_micros: u64,
) {
    let Some(row) = row else {
        return;
    };
    if row.instance_id != viewer_instance_id {
        return;
    }
    if row.facing {
        created_outbound.extend(creature_leg_outbound(created, row));
        return;
    }

    let duration_micros = u64::from(row.dur_ms) * 1_000;
    let elapsed_micros = now_micros.saturating_sub(row.start_micros);
    if duration_micros == 0 || elapsed_micros >= duration_micros {
        return;
    }

    let elapsed_fraction = elapsed_micros as f64 / duration_micros as f64;
    let current = |start: f32, destination: f32| {
        (f64::from(start) + (f64::from(destination) - f64::from(start)) * elapsed_fraction) as f32
    };
    let mut remaining = row.clone();
    remaining.sx = current(row.sx, row.dx);
    remaining.sy = current(row.sy, row.dy);
    remaining.sz = current(row.sz, row.dz);
    if let Some(path) = &row.path {
        let points = &path.points;
        let points: Vec<_> = points.iter().map(|p| (p.x, p.y, p.z)).collect();
        let (at, next) = lyracore_shared::movement_path::sample(
            (row.sx, row.sy, row.sz),
            &points,
            elapsed_fraction as f32,
        );
        (remaining.sx, remaining.sy, remaining.sz) = at;
        if let Some(path) = &mut remaining.path {
            path.points = path.points[next..].to_vec();
        }
    }
    remaining.dur_ms = (duration_micros - elapsed_micros).div_ceil(1_000) as u32;
    created_outbound.extend(creature_leg_outbound(created, &remaining));
}

pub(crate) fn unix_now_micros() -> u64 {
    let micros = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_micros();
    micros.min(u128::from(u64::MAX)) as u64
}

pub(crate) fn motion(
    view: &WorldView,
    shard: ShardId,
    row: &EntityMotion,
    delivery: &crate::stdb::movement_batch::MotionDelivery,
) {
    delivery.received();
    let key = CellKey::at(row.map_id, row.instance_id, row.grid_x, row.grid_y);
    let row = Arc::new(row.clone());
    // The MOVER is deliberately not a recipient (`world_entity_recipients`' self leg is skipped
    // here): a
    // player must never receive their own movement echoed back, or they fight the server for
    // authority over their position. `relay_entity_motion` re-checks it anyway.
    for viewer in view.cell_audience(shard, Some(key), BOX_HALF_SPAN, &[]) {
        if viewer.self_guid == row.guid {
            continue;
        }
        // SUPERSEDE, don't append — the viewer needs only this mover's latest position, so
        // the upsert replaces any not-yet-flushed heartbeat from the same mover and the queue
        // carries at most one flush job regardless of crowd size.
        viewer
            .motion_pending
            .entity
            .lock()
            .unwrap()
            .insert(row.guid, row.clone());
        maybe_queue_motion_flush(view, &viewer);
    }
}

pub(crate) fn creature_leg(view: &WorldView, shard: ShardId, row: &CreatureSpline) {
    let key = CellKey::at(row.map_id, row.instance_id, row.grid_x, row.grid_y);
    let row = Arc::new(row.clone());
    for viewer in view.cell_audience(shard, Some(key), BOX_HALF_SPAN, &[]) {
        // Same supersession as player motion — latest leg per creature wins.
        viewer
            .motion_pending
            .creature
            .lock()
            .unwrap()
            .insert(row.guid, row.clone());
        maybe_queue_motion_flush(view, &viewer);
    }
}

/// Queue this viewer's ONE motion-flush job, unless one is already queued. The egress shed
/// decision keeps its pump-side, one-atomic-load shape (task B2's contract): a shed drops the
/// FLUSH — the pending map keeps superseding meanwhile, the flag resets so a later heartbeat
/// retries, and nothing is stranded.
pub(crate) fn maybe_queue_motion_flush(_view: &WorldView, viewer: &Arc<Viewer>) {
    use std::sync::atomic::Ordering as AtOrd;
    if viewer.motion_pending.flush_queued.swap(true, AtOrd::AcqRel) {
        return; // a queued flush will carry the upsert that just landed
    }
    if crate::stdb::subscriptions::shed_motion_at_depth(viewer.tx.depth()) {
        crate::stdb::subscriptions::MOTION_DROPPED.fetch_add(1, Ordering::Relaxed);
        viewer
            .motion_pending
            .flush_queued
            .store(false, AtOrd::Release);
        return;
    }
    let viewer = viewer.clone();
    enqueue(viewer.clone(), move |viewer| {
        // Clear BEFORE draining: an upsert racing the drain re-queues a follow-up flush (possibly
        // empty — harmless) instead of being stranded behind a still-set flag.
        viewer
            .motion_pending
            .flush_queued
            .store(false, AtOrd::Release);
        let moves: Vec<Arc<EntityMotion>> = {
            let mut m = viewer.motion_pending.entity.lock().unwrap();
            m.drain().map(|(_, r)| r).collect()
        };
        let legs: Vec<Arc<CreatureSpline>> = {
            let mut m = viewer.motion_pending.creature.lock().unwrap();
            m.drain().map(|(_, r)| r).collect()
        };
        let mut out = Vec::new();
        for row in moves {
            out.extend(crate::codec::movement::subscriptions::motion_outbound(
                &viewer.created,
                viewer.self_guid,
                &row,
            ));
        }
        for row in legs {
            out.extend(
                crate::codec::movement::subscriptions::creature_leg_outbound(&viewer.created, &row),
            );
        }
        out
    });
}

/// A teleport row names one character owner on this shard. Packet construction stays on that
/// viewer's writer.
pub(crate) fn teleport_appeared(view: &WorldView, shard: ShardId, row: &TeleportEvent) {
    let Some(viewer) = view.viewer_of_owner_on_shard(shard, OwnerGuid(row.mover_guid)) else {
        return;
    };
    let row = row.clone();
    enqueue(viewer.clone(), move |_| {
        crate::codec::movement::subscriptions::teleport_event_outbound(&row)
    });
}

crate::stdb::relays::declare_relays! {
    game_teleport_event [false] {
        insert(Live) => |context, _event, row| teleport_appeared(&context.view, context.shard, row);
    }
    game_entity_motion [false] {
        insert(Snapshot) => |context, _event, row| motion(&context.view, context.shard, row, &context.coord.0.motion_batch.delivery);
        update(Snapshot) => |context, _event, _old, row| motion(&context.view, context.shard, row, &context.coord.0.motion_batch.delivery);
    }
    game_creature_spline [false] {
        insert(Snapshot) => |context, _event, row| creature_leg(&context.view, context.shard, row);
        update(Snapshot) => |context, _event, _old, row| creature_leg(&context.view, context.shard, row);
    }
}

#[cfg(test)]
mod packet_tests;

#[cfg(test)]
mod audience_tests;
