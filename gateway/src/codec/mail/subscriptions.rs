//! Table Relays for this Protocol Family.

use crate::codec::{self};
use crate::stdb::bindings::*;
use crate::stdb::world_view::*;
use crate::world::Outbound;
use wow_world_messages::vanilla::opcodes::ServerOpcodeMessage;

/// Build a Mail Arrival after the caller validates the recipient.
pub(crate) fn mail_arrival_outbound() -> Vec<Outbound> {
    vec![Outbound::One(ServerOpcodeMessage::SMSG_RECEIVED_MAIL(
        codec::build_received_mail(),
    ))]
}

/// A Mail became visible to its recipient → SMSG_RECEIVED_MAIL to that recipient's World Session on
/// any Shard. An offline recipient gets nothing and learns of the Mail from the unread poll at the
/// next login (cmangos `Player.cpp:3098-3110`, `MailHandler.cpp:712-720`).
pub(crate) fn mail_arrived(view: &WorldView, row: &MailArrival) {
    let Some(session) = view.session_of_owner(row.recipient_guid) else {
        return;
    };
    let Some(viewer) = view.viewer(session) else {
        return;
    };
    if !crate::codec::social::subscriptions::private_recipient_audience(
        row.recipient_guid,
        viewer.self_guid,
    ) {
        return;
    }
    enqueue(viewer.clone(), |_| {
        crate::codec::mail::subscriptions::mail_arrival_outbound()
    });
}

crate::stdb::relays::declare_relays! {
    game_mail_arrival [true] {
        insert(Live) => |context, _event, row| mail_arrived(&context.view, row);
    }
}

#[cfg(test)]
mod audience_tests;
