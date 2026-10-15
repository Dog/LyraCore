use crate::codec::{self};
use crate::stdb::bindings::*;
use crate::stdb::world_view::*;
use crate::world::Outbound;
use wow_world_messages::vanilla::opcodes::ServerOpcodeMessage;

pub(crate) fn mail_arrival_outbound() -> Vec<Outbound> {
    vec![Outbound::One(ServerOpcodeMessage::SMSG_RECEIVED_MAIL(
        codec::build_received_mail(),
    ))]
}

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
