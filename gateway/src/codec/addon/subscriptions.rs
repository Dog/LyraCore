//! Table Relays for this Protocol Family.

use crate::codec::{self};
use crate::stdb::bindings::*;
use crate::stdb::world_index::ShardId;
use crate::stdb::world_view::*;
use crate::world::Outbound;

/// Build one addon-language whisper using the addressed viewer as its sender.
pub(crate) fn addon_message_outbound(self_guid: u64, row: &AddonMessage) -> Vec<Outbound> {
    let text = codec::addon::build_bridge_envelope(&row.cmd, &row.payload);
    let (opcode, body) = codec::addon::build_addon_smsg_raw(self_guid, &text);
    vec![Outbound::Raw { opcode, body }]
}

/// Addon messages are identity-addressed, with the addressed viewer as the wire sender.
pub(crate) fn addon_message_appeared(view: &WorldView, shard: ShardId, row: &AddonMessage) {
    let Some(viewer) =
        view.viewer_of_identity_on_shard(shard, BoundIdentity(row.recipient_identity))
    else {
        return;
    };
    let row = row.clone();
    enqueue(viewer.clone(), move |viewer| {
        crate::codec::addon::subscriptions::addon_message_outbound(viewer.self_guid, &row)
    });
}

crate::stdb::relays::declare_relays! {
    game_addon_message [false] {
        insert(Live) => |context, _event, row| addon_message_appeared(&context.view, context.shard, row);
    }
}
