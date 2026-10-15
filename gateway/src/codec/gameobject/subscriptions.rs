use crate::codec::entity::subscriptions::instance_relay_gate;
use crate::codec::{self};
use crate::stdb::bindings::*;
use crate::stdb::views::go_view;
use crate::stdb::world_index::{CellKey, EntityLayer, SessionId, ShardId};
use crate::stdb::world_view::*;
use crate::stdb::Coordinator;
use crate::world::Outbound;
use lyracore_shared::spatial::BOX_HALF_SPAN;
use std::collections::HashSet;
use std::sync::Arc;
use wow_world_messages::vanilla::opcodes::ServerOpcodeMessage;

pub(crate) fn relay_gameobject_create(
    coord: &Coordinator,
    viewer: &Viewer,
    row: &GameObject,
) -> Vec<Outbound> {
    if !instance_relay_gate(row.instance_id, Some(viewer.instance_id)) {
        return Vec::new();
    }
    let guard = coord.0.coord();
    let Some(tmpl) = guard
        .conn
        .db
        .game_gameobject_template()
        .entry()
        .find(&row.template_entry)
    else {
        return Vec::new();
    };
    let view = go_view(row.clone(), &tmpl);
    // CREATE carries only rot0; send all four quaternion slots in the following VALUES frame.
    let (rot_opcode, rot_body) = codec::build_gameobject_rotation_values(&view);
    vec![
        Outbound::One(ServerOpcodeMessage::SMSG_UPDATE_OBJECT(Box::new(
            codec::build_gameobject_create_object(&view),
        ))),
        Outbound::Raw {
            opcode: rot_opcode,
            body: rot_body,
        },
    ]
}

pub(crate) fn relay_gameobject_destroy(_viewer: &Viewer, guid: u64) -> Vec<Outbound> {
    vec![Outbound::One(ServerOpcodeMessage::SMSG_DESTROY_OBJECT(
        codec::build_destroy_object(guid),
    ))]
}

pub(crate) fn gameobject_appeared(view: &WorldView, shard: ShardId, row: &GameObject) {
    let key = CellKey::at(row.map_id, row.instance_id, row.grid_x, row.grid_y);
    let previous = view
        .spatial
        .upsert_entity(EntityLayer::GameObject, row.guid, key, shard, 0);
    let recipients = view.cell_audience(shard, Some(key), BOX_HALF_SPAN, &[]);
    if let Some(old_key) = previous {
        if old_key != key {
            let still: HashSet<SessionId> = recipients.iter().map(|v| v.session).collect();
            for viewer in view.cell_audience(shard, Some(old_key), BOX_HALF_SPAN, &[]) {
                if !still.contains(&viewer.session) {
                    let guid = row.guid;
                    enqueue(viewer.clone(), move |viewer| {
                        crate::codec::gameobject::subscriptions::relay_gameobject_destroy(
                            &viewer, guid,
                        )
                    });
                }
            }
        }
    }
    let Some(coord) = view.shard(shard) else {
        return;
    };
    let row = Arc::new(row.clone());
    for viewer in recipients {
        let (coord, row) = (coord.clone(), row.clone());
        enqueue(viewer.clone(), move |viewer| {
            crate::codec::gameobject::subscriptions::relay_gameobject_create(&coord, &viewer, &row)
        });
    }
}

pub(crate) fn gameobject_vanished(view: &WorldView, shard: ShardId, guid: u64) {
    let Some((key, _)) = view
        .spatial
        .remove_entity(EntityLayer::GameObject, guid, shard)
    else {
        return;
    };
    for viewer in view.cell_audience(shard, Some(key), BOX_HALF_SPAN, &[]) {
        // DESTROY for an unknown guid is a client no-op; gating it can leave stale objects.
        enqueue(viewer.clone(), move |viewer| {
            crate::codec::gameobject::subscriptions::relay_gameobject_destroy(&viewer, guid)
        });
    }
}

crate::stdb::relays::declare_relays! {
    game_gameobject [false] {
        insert(Snapshot) => |context, _event, row| gameobject_appeared(&context.view, context.shard, row);
        update(Snapshot) => |context, _event, _old, row| gameobject_appeared(&context.view, context.shard, row);
        delete(Snapshot) => |context, _event, row| gameobject_vanished(&context.view, context.shard, row.guid);
    }
}
