//! Table Relays for this Protocol Family.

use crate::codec::{self};
use crate::stdb::bindings::*;
use crate::stdb::world_index::{CellKey, ShardId};
use crate::stdb::world_view::*;
use crate::stdb::Coordinator;
use crate::world::Outbound;
use lyracore_shared::spatial::BOX_HALF_SPAN;
use std::collections::HashSet;
use std::sync::Mutex;
use wow_world_messages::vanilla::Vector3d;

/// One module-owned passenger route to self and observers. Ordinary player motion deliberately
/// excludes self; taxi presentation must include it because the client is not driving the mover.
pub(crate) fn taxi_spline_outbound(
    created: &Mutex<HashSet<u64>>,
    self_guid: u64,
    row: &TaxiPassengerSpline,
) -> Vec<Outbound> {
    if row.character_guid != self_guid && !created.lock().unwrap().contains(&row.character_guid) {
        return Vec::new();
    }
    let points: Vec<Vector3d> = row
        .points
        .chunks_exact(3)
        .map(|point| Vector3d {
            x: point[0],
            y: point[1],
            z: point[2],
        })
        .collect();
    if points.is_empty() || points.len() * 3 != row.points.len() {
        log::warn!(
            "taxi spline: malformed route ({} coordinates) for passenger {}",
            row.points.len(),
            row.character_guid
        );
        return Vec::new();
    }
    let start = Vector3d {
        x: row.start_x,
        y: row.start_y,
        z: row.start_z,
    };
    codec::build_taxi_move_raw(
        row.character_guid,
        start,
        points,
        row.duration_ms,
        row.spline_id,
    )
    .map_or_else(Vec::new, |(opcode, body)| {
        vec![Outbound::Raw { opcode, body }]
    })
}

pub(crate) fn append_resident_taxi_after_create(
    created_outbound: &mut Vec<Outbound>,
    created: &Mutex<HashSet<u64>>,
    self_guid: u64,
    row: &TaxiPassengerSpline,
) {
    created_outbound.extend(taxi_spline_outbound(created, self_guid, row));
}

/// Replay a resident passenger spline after the owner's self CREATE/login batch. The same helper
/// is used by reconnect recovery, keeping the CREATE-before-MONSTER_MOVE ordering explicit.
pub(crate) fn resident_taxi_spline_outbound(
    coord: &Coordinator,
    viewer: &Viewer,
    character_guid: u64,
) -> Vec<Outbound> {
    let guard = coord.0.coord();
    let Some(row) = guard
        .conn
        .db
        .game_taxi_passenger_spline()
        .character_guid()
        .find(&character_guid)
    else {
        return Vec::new();
    };
    taxi_spline_outbound(&viewer.created, viewer.self_guid, &row)
}

/// A passenger spline is always sent to its owner. Nearby sessions receive it only after the
/// outbound visibility check confirms that the passenger already exists in their client world.
pub(crate) fn taxi_spline(view: &WorldView, shard: ShardId, row: &TaxiPassengerSpline) {
    let key = CellKey::at(row.map_id, row.instance_id, row.grid_x, row.grid_y);
    let row = row.clone();
    for viewer in view.cell_audience(shard, Some(key), BOX_HALF_SPAN, &[row.character_guid]) {
        let row = row.clone();
        enqueue(viewer.clone(), move |viewer| {
            crate::codec::taxi::subscriptions::taxi_spline_outbound(
                &viewer.created,
                viewer.self_guid,
                &row,
            )
        });
    }
}

crate::stdb::relays::declare_relays! {
    game_taxi_passenger_spline [false] {
        insert(Snapshot) => |context, _event, row| taxi_spline(&context.view, context.shard, row);
        update(Snapshot) => |context, _event, _old, row| taxi_spline(&context.view, context.shard, row);
    }
}
