use crate::codec::{self};
use crate::stdb::bindings::*;
use crate::stdb::views::corpse_view;
use crate::stdb::world_index::ShardId;
use crate::stdb::world_view::*;
use crate::world::{Outbound, SessionTx};
use std::sync::Arc;
use std::time::Duration;
use wow_world_messages::vanilla::opcodes::ServerOpcodeMessage;

pub(crate) fn relay_corpse_create(tx: &SessionTx, self_guid: u64, row: &Corpse) {
    for o in corpse_create_outbound(self_guid, row) {
        let _ = tx.send(o);
    }
}

pub(crate) fn corpse_create_outbound(self_guid: u64, row: &Corpse) -> Vec<Outbound> {
    let m = codec::build_corpse_create_object(&corpse_view(row.clone()));
    let mut out = vec![Outbound::One(ServerOpcodeMessage::SMSG_UPDATE_OBJECT(
        Box::new(m),
    ))];
    if row.owner_guid == self_guid {
        out.push(Outbound::One(
            ServerOpcodeMessage::SMSG_CORPSE_RECLAIM_DELAY(
                wow_world_messages::vanilla::SMSG_CORPSE_RECLAIM_DELAY {
                    delay: Duration::from_micros(row.reclaim_delay_micros as u64),
                },
            ),
        ));
    }
    out
}

pub(crate) fn relay_corpse_update(row: &Corpse) -> Vec<Outbound> {
    let m = codec::build_corpse_create_object(&corpse_view(row.clone()));
    vec![Outbound::One(ServerOpcodeMessage::SMSG_UPDATE_OBJECT(
        Box::new(m),
    ))]
}

pub(crate) fn resurrect_request_outbound(row: &ResurrectRequest) -> Vec<Outbound> {
    let m = codec::build_resurrect_request(row.caster_guid, row.caster_name.clone());
    vec![Outbound::One(ServerOpcodeMessage::SMSG_RESURRECT_REQUEST(
        Box::new(m),
    ))]
}

pub(crate) fn self_res_option_outbound(character_guid: u64, spell_id: u32) -> Vec<Outbound> {
    let m = codec::build_self_res_spell_values(character_guid, spell_id);
    vec![Outbound::One(ServerOpcodeMessage::SMSG_UPDATE_OBJECT(
        Box::new(m),
    ))]
}

pub(crate) fn corpse_appeared(view: &WorldView, shard: ShardId, row: &Corpse) {
    let row = Arc::new(row.clone());
    for viewer in viewers_on_shard(view, shard) {
        if !crate::codec::entity::subscriptions::instance_relay_gate(
            row.instance_id,
            Some(viewer.instance_id),
        ) {
            continue;
        }
        let row = row.clone();
        let self_guid = viewer.self_guid;
        enqueue(viewer.clone(), move |_| {
            crate::codec::corpse::subscriptions::corpse_create_outbound(self_guid, &row)
        });
    }
}

pub(crate) fn corpse_changed(view: &WorldView, shard: ShardId, row: &Corpse) {
    let row = Arc::new(row.clone());
    for viewer in viewers_on_shard(view, shard) {
        if !crate::codec::entity::subscriptions::instance_relay_gate(
            row.instance_id,
            Some(viewer.instance_id),
        ) {
            continue;
        }
        let row = row.clone();
        enqueue(viewer.clone(), move |_| {
            crate::codec::corpse::subscriptions::relay_corpse_update(&row)
        });
    }
}

pub(crate) fn corpse_vanished(view: &WorldView, shard: ShardId, row: &Corpse) {
    let guid = row.guid;
    for viewer in viewers_on_shard(view, shard) {
        enqueue(viewer.clone(), move |_| {
            crate::codec::combat::subscriptions::relay_destroy_object(guid)
        });
    }
}

pub(crate) fn resurrect_offered(view: &WorldView, shard: ShardId, row: &ResurrectRequest) {
    let Some(viewer) = view.viewer_of_owner_on_shard(shard, OwnerGuid(row.target_guid)) else {
        return;
    };
    if !crate::codec::social::subscriptions::private_recipient_audience(
        row.target_guid,
        viewer.self_guid,
    ) {
        return;
    }
    let row = row.clone();
    enqueue(viewer.clone(), move |_| {
        crate::codec::corpse::subscriptions::resurrect_request_outbound(&row)
    });
}

pub(crate) fn self_res_option_changed(
    view: &WorldView,
    shard: ShardId,
    character_guid: u64,
    spell_id: u32,
) {
    let Some(viewer) = view.viewer_of_owner_on_shard(shard, OwnerGuid(character_guid)) else {
        return;
    };
    if !crate::codec::social::subscriptions::private_recipient_audience(
        character_guid,
        viewer.self_guid,
    ) {
        return;
    }
    enqueue(viewer.clone(), move |_| {
        crate::codec::corpse::subscriptions::self_res_option_outbound(character_guid, spell_id)
    });
}

crate::stdb::relays::declare_relays! {
    game_corpse [false] {
        insert(Snapshot) => |context, _event, row| corpse_appeared(&context.view, context.shard, row);
        update(Snapshot) => |context, _event, _old, row| corpse_changed(&context.view, context.shard, row);
        delete(Snapshot) => |context, _event, row| corpse_vanished(&context.view, context.shard, row);
    }
    game_resurrect_request [false] {
        insert(Live) => |context, _event, row| resurrect_offered(&context.view, context.shard, row);
    }
    game_self_resurrect_option [false] {
        insert(Snapshot) => |context, _event, row| self_res_option_changed(&context.view, context.shard, row.character_guid, row.spell_id);
        update(Snapshot) => |context, _event, _old, row| self_res_option_changed(&context.view, context.shard, row.character_guid, row.spell_id);
        delete(Snapshot) => |context, _event, row| self_res_option_changed(&context.view, context.shard, row.character_guid, 0);
    }
}

#[cfg(test)]
mod audience_tests;
