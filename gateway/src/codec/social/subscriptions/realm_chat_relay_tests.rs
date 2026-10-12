use super::*;
use crate::stdb::bindings::RealmChatEvent;
use crate::stdb::world_view::test_support::{
    contact_entry as ignore_row, registered_viewer as listener,
};
use crate::stdb::world_view::WorldView;
use crate::world::Outbound;
use std::sync::mpsc::Receiver;

const SPEAKER: u64 = 10;

fn party_line(recipients: &[u64], ignorable: bool) -> RealmChatEvent {
    RealmChatEvent {
        id: 1,
        kind: 1,
        speaker_guid: SPEAKER,
        language: 7,
        chat_tag: 0,
        channel_name: String::new(),
        message: "form up".to_string(),
        recipients: recipients.to_vec(),
        ignorable,
        created_at: spacetimedb_sdk::Timestamp::UNIX_EPOCH,
    }
}

/// Every queued packet for one viewer, jobs run in order.
/// The body of every packet queued for one viewer, jobs run in order. Each packet must be one
/// framed `SMSG_MESSAGECHAT`: size u16 big-endian, then opcode 0x0096 little-endian.
fn received(rx: &Receiver<Outbound>) -> Vec<Vec<u8>> {
    let mut bodies = Vec::new();
    while let Ok(outbound) = rx.try_recv() {
        let Outbound::Job(job) = outbound else {
            panic!("the Relay must enqueue packet work as a writer job");
        };
        for packet in job() {
            let Outbound::One(message) = packet else {
                panic!("a Realm Chat Line is one packet");
            };
            let mut frame = Vec::new();
            message.write_unencrypted_server(&mut frame).unwrap();
            let body = frame.split_off(4);
            assert_eq!(frame[..2], ((body.len() + 2) as u16).to_be_bytes());
            assert_eq!(frame[2..], [0x96, 0x00], "SMSG_MESSAGECHAT");
            bodies.push(body);
        }
    }
    bodies
}

/// A PARTY line in Common from `SPEAKER` saying "form up", written out from cm:Chat.cpp:3637-3641
/// rather than built with the codec the Relay calls: type, language, the sender guid twice, the
/// sized message with its terminator, then the chat tag.
fn expected_line() -> Vec<u8> {
    let mut body = vec![0x01]; // CHAT_MSG_PARTY
    body.extend(7u32.to_le_bytes()); // LANG_COMMON
    body.extend(SPEAKER.to_le_bytes());
    body.extend(SPEAKER.to_le_bytes());
    body.extend(8u32.to_le_bytes()); // "form up" and its terminator
    body.extend(b"form up\0");
    body.push(0); // CHAT_TAG_NONE
    body
}

#[test]
fn a_line_reaches_each_recipient_once_on_any_shard_and_nobody_else() {
    let view = WorldView::new(true);
    let (_, speaker_rx) = listener(&view, 0, SPEAKER);
    let (_, member_rx) = listener(&view, 1, 20);
    let (_, bystander_rx) = listener(&view, 0, 30);

    realm_chat_appeared(&view, &party_line(&[SPEAKER, 20, 99], false));

    assert_eq!(received(&speaker_rx), [expected_line()]);
    assert_eq!(received(&member_rx), [expected_line()]);
    assert!(
        received(&bystander_rx).is_empty(),
        "a Character the Module did not name hears nothing"
    );
}

#[test]
fn an_ignorable_line_skips_only_the_listener_who_ignores_the_speaker() {
    let view = WorldView::new(true);
    let (_, ignorer_rx) = listener(&view, 0, 20);
    let (_, other_rx) = listener(&view, 1, 30);
    contact_changed(&view, 0, &ignore_row(20, SPEAKER, true), true);

    realm_chat_appeared(&view, &party_line(&[20, 30], true));
    assert!(received(&ignorer_rx).is_empty());
    assert_eq!(received(&other_rx), [expected_line()]);

    realm_chat_appeared(&view, &party_line(&[20, 30], false));
    assert_eq!(
        received(&ignorer_rx),
        [expected_line()],
        "a line the Module marked not ignorable reaches an ignorer too"
    );
    assert_eq!(received(&other_rx), [expected_line()]);
}

#[test]
fn the_ignore_set_follows_its_owners_ignore_rows_on_the_owners_shard() {
    let view = WorldView::new(true);
    let (viewer, _rx) = listener(&view, 0, 20);

    contact_changed(&view, 0, &ignore_row(20, SPEAKER, false), true);
    assert!(!viewer.ignores(SPEAKER), "a friend row is not an ignore");

    contact_changed(&view, 1, &ignore_row(20, SPEAKER, true), true);
    assert!(
        !viewer.ignores(SPEAKER),
        "another Shard's copy of the owner's rows does not address this viewer"
    );

    contact_changed(&view, 0, &ignore_row(20, SPEAKER, true), true);
    assert!(viewer.ignores(SPEAKER));

    contact_changed(&view, 0, &ignore_row(20, SPEAKER, true), false);
    assert!(!viewer.ignores(SPEAKER));
}

/// The friend set follows the same rule as the ignore set, off the SAME contact rows and
/// callbacks, filtered by `is_ignore` the other way.
#[test]
fn the_friend_set_follows_its_owners_friend_rows_on_the_owners_shard() {
    let view = WorldView::new(true);
    let (viewer, _rx) = listener(&view, 0, 20);

    contact_changed(&view, 0, &ignore_row(20, SPEAKER, true), true);
    assert!(!viewer.is_friend(SPEAKER), "an ignore row is not a friend");

    contact_changed(&view, 1, &ignore_row(20, SPEAKER, false), true);
    assert!(
        !viewer.is_friend(SPEAKER),
        "another Shard's copy of the owner's rows does not address this viewer"
    );

    contact_changed(&view, 0, &ignore_row(20, SPEAKER, false), true);
    assert!(viewer.is_friend(SPEAKER));

    contact_changed(&view, 0, &ignore_row(20, SPEAKER, false), false);
    assert!(!viewer.is_friend(SPEAKER));
}
