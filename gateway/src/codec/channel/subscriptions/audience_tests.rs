use super::*;
use crate::codec::social::subscriptions::contact_changed;
use crate::stdb::world_view::test_support::{
    contact_entry as ignore_row, registered_viewer as listener,
};
use std::sync::mpsc::Receiver;

const SPEAKER: u64 = 10;

fn received_raw(rx: &Receiver<Outbound>) -> Vec<(u16, Vec<u8>)> {
    let mut packets = Vec::new();
    while let Ok(outbound) = rx.try_recv() {
        let Outbound::Job(job) = outbound else {
            panic!("the Relay must enqueue packet work as a writer job");
        };
        for packet in job() {
            let Outbound::Raw { opcode, body } = packet else {
                panic!("a Channel Notice is one raw packet");
            };
            packets.push((opcode, body));
        }
    }
    packets
}

#[test]
fn a_channel_notice_reaches_each_recipient_on_any_shard_and_nobody_else() {
    let view = WorldView::new(true);
    let (_, first_rx) = listener(&view, 0, 20);
    let (_, second_rx) = listener(&view, 1, 30);
    let (_, bystander_rx) = listener(&view, 0, 40);
    contact_changed(&view, 0, &ignore_row(20, SPEAKER, true), true);

    channel_notice_appeared(
        &view,
        &ChatChannelNoticeEvent {
            id: 1,
            notice: 0x00,
            channel_name: "Tx".to_string(),
            subject_guid: SPEAKER,
            actor_guid: 0,
            old_flags: 0,
            new_flags: 0,
            channel_flags: 0,
            text: String::new(),
            recipients: vec![20, 30, 99],
            created_at: spacetimedb_sdk::Timestamp::UNIX_EPOCH,
        },
    );

    let mut joined = vec![0x00, b'T', b'x', 0];
    joined.extend(SPEAKER.to_le_bytes());
    assert_eq!(
        received_raw(&first_rx),
        [(0x0099, joined.clone())],
        "notices skip the ignore list"
    );
    assert_eq!(received_raw(&second_rx), [(0x0099, joined)]);
    assert!(received_raw(&bystander_rx).is_empty());
}
