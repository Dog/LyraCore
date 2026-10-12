use super::*;
use crate::stdb::bindings::MailArrival;
use crate::stdb::world_index::CellKey;
use crate::stdb::world_view::test_support::{identity, queued_job, viewer_with_tx};
use crate::stdb::world_view::WorldView;
use crate::world::{Outbound, SessionTx};
use wow_world_messages::vanilla::opcodes::ServerOpcodeMessage;

#[test]
fn a_mail_arrival_reaches_its_recipient_on_any_shard_and_an_unknown_guid_gets_nothing() {
    let view = WorldView::new(true);
    let (recipient_tx, recipient_rx) = SessionTx::with_depth(0);
    let recipient = viewer_with_tx(1, 9001, identity(1), recipient_tx);
    let (bystander_tx, bystander_rx) = SessionTx::with_depth(0);
    let bystander = viewer_with_tx(2, 9002, identity(2), bystander_tx);
    let anchor = CellKey::at(0, 0, 0, 0);
    view.add_viewer_on_shard(recipient.clone(), anchor, 1);
    view.add_viewer_on_shard(bystander, anchor, 0);

    let row = MailArrival {
        id: 1,
        recipient_guid: recipient.self_guid,
        created_at: spacetimedb_sdk::Timestamp::UNIX_EPOCH,
    };
    mail_arrived(&view, &row);

    let outbound = queued_job(&recipient_rx);
    let [Outbound::One(ServerOpcodeMessage::SMSG_RECEIVED_MAIL(packet))] = outbound.as_slice()
    else {
        panic!("the recipient must receive one SMSG_RECEIVED_MAIL");
    };
    assert_eq!(packet.unknown1, 0);
    assert!(
        recipient_rx.try_recv().is_err(),
        "one arrival queues one job"
    );
    assert!(
        bystander_rx.try_recv().is_err(),
        "a second Character must receive no queued work"
    );

    mail_arrived(
        &view,
        &MailArrival {
            recipient_guid: 9999,
            ..row
        },
    );
    assert!(recipient_rx.try_recv().is_err());
    assert!(bystander_rx.try_recv().is_err());
}
