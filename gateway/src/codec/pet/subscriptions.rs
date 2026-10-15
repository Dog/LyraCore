use crate::codec::{self};
use crate::stdb::bindings::*;
use crate::stdb::views::hunter_pet_protocol_view;
use crate::stdb::world_index::ShardId;
use crate::stdb::world_view::*;
use crate::world::Outbound;
use wow_world_messages::vanilla::opcodes::ServerOpcodeMessage;

pub(crate) fn hunter_pet_outbound(row: &HunterPetProtocol) -> Vec<Outbound> {
    vec![Outbound::One(ServerOpcodeMessage::SMSG_UPDATE_OBJECT(
        Box::new(codec::build_hunter_pet_values(&hunter_pet_protocol_view(
            row.clone(),
        ))),
    ))]
}

pub(crate) fn hunter_pet_appeared(view: &WorldView, shard: ShardId, row: &HunterPetProtocol) {
    let Some(viewer) = view.viewer_of_owner_on_shard(shard, OwnerGuid(row.owner_guid)) else {
        return;
    };
    let row = row.clone();
    enqueue(viewer.clone(), move |_| {
        crate::codec::pet::subscriptions::hunter_pet_outbound(&row)
    });
}

crate::stdb::relays::declare_relays! {
    game_hunter_pet_protocol [false] {
        insert(Snapshot) => |context, _event, row| hunter_pet_appeared(&context.view, context.shard, row);
        update(Snapshot) => |context, _event, old, row| {
            if old != row {
                hunter_pet_appeared(&context.view, context.shard, row)
            }
        };
    }
}
