//! Table Relays for this Protocol Family.

use crate::codec::{self};
use crate::stdb::bindings::*;
use crate::stdb::world_view::*;
use crate::world::Outbound;
use std::sync::Arc;

/// A Channel Notice: one raw `SMSG_CHANNEL_NOTIFY` for one recipient. The Relay already chose the
/// audience.
pub(crate) fn channel_notice_outbound(row: &ChatChannelNoticeEvent) -> Vec<Outbound> {
    let (opcode, body) = codec::channel::notify(&codec::channel::ChannelNoticeView {
        notice: row.notice,
        channel_name: row.channel_name.clone(),
        subject_guid: row.subject_guid,
        actor_guid: row.actor_guid,
        old_flags: row.old_flags,
        new_flags: row.new_flags,
        channel_flags: row.channel_flags,
        text: row.text.clone(),
    });
    vec![Outbound::Raw { opcode, body }]
}

/// A Channel Notice landed → one `SMSG_CHANNEL_NOTIFY` per recipient with a World Session on this
/// Gateway, whatever Shard it plays on. The Module chose the audience; no ignore list applies.
pub(crate) fn channel_notice_appeared(view: &WorldView, row: &ChatChannelNoticeEvent) {
    let row = Arc::new(row.clone());
    for &recipient in &row.recipients {
        let Some(viewer) = view
            .session_of_owner(recipient)
            .and_then(|session| view.viewer(session))
        else {
            continue;
        };
        if !crate::codec::social::subscriptions::private_recipient_audience(
            recipient,
            viewer.self_guid,
        ) {
            continue;
        }
        let row = row.clone();
        enqueue(viewer.clone(), move |_| {
            crate::codec::channel::subscriptions::channel_notice_outbound(&row)
        });
    }
}

crate::stdb::relays::declare_relays! {
    game_chat_channel_notice_event [true] {
        insert(Live) => |context, _event, row| channel_notice_appeared(&context.view, row);
    }
}
