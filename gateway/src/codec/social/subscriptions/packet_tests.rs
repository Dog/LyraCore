use super::*;
use crate::codec;
use crate::codec::entity::subscriptions::tests::player_entity;
use crate::stdb::bindings::{ChatEvent, SystemMessageEvent};
use crate::world::Outbound;
use wow_world_messages::vanilla::opcodes::ServerOpcodeMessage;

#[test]
fn chat_event_target_reaches_every_addressable_creature_packet() {
    let base = ChatEvent {
        id: 1,
        sender_guid: 0xF130_0000_0000_002A,
        chat_type: 0,
        language: 0,
        message: "You there!".into(),
        created_at: spacetimedb_sdk::Timestamp::UNIX_EPOCH,
        target_guid: 77,
    };
    for chat_type in [
        lyracore_shared::chat::broadcast_chat::SAY,
        lyracore_shared::chat::broadcast_chat::YELL,
        lyracore_shared::chat::broadcast_chat::CREATURE_TEXT_EMOTE,
    ] {
        let out = chat_event_message(
            &ChatEvent {
                chat_type,
                ..base.clone()
            },
            Some("Defias Thug".into()),
            0,
        );
        let Outbound::One(ServerOpcodeMessage::SMSG_MESSAGECHAT(message)) = out else {
            panic!("expected creature chat packet");
        };
        match &message.chat_type {
            wow_world_messages::vanilla::SMSG_MESSAGECHAT_ChatType::MonsterSay {
                target, ..
            }
            | wow_world_messages::vanilla::SMSG_MESSAGECHAT_ChatType::MonsterYell {
                target, ..
            } => assert_eq!(target.guid(), 77),
            wow_world_messages::vanilla::SMSG_MESSAGECHAT_ChatType::MonsterEmote {
                monster,
                ..
            } => assert_eq!(monster.guid(), 0xF130_0000_0000_002A),
            other => panic!("expected addressable creature chat, got {other:?}"),
        }
    }
}

#[test]
fn system_message_outbound_reuses_the_existing_system_chat_builder() {
    let row = SystemMessageEvent {
        id: 1,
        recipient_identity: spacetimedb_sdk::Identity::from_byte_array([7; 32]),
        recipient_guid: 9001,
        message: "Package loaded".to_string(),
        created_at: spacetimedb_sdk::Timestamp::UNIX_EPOCH,
    };
    let outbound = system_message_event_outbound(&row);
    let [Outbound::One(ServerOpcodeMessage::SMSG_MESSAGECHAT(actual))] = outbound.as_slice() else {
        panic!("one System Message packet expected");
    };

    assert_eq!(
        **actual,
        codec::build_gm_system_message(row.message.clone())
    );
}

#[test]
fn a_nearby_line_carries_the_player_speakers_away_tag() {
    let mut speaker = player_entity();
    assert_eq!(nearby_chat_tag(Some(&speaker)), 0);
    speaker.player_flags = 0x02;
    assert_eq!(nearby_chat_tag(Some(&speaker)), 1);
    speaker.player_flags = 0x04 | 0x10;
    assert_eq!(nearby_chat_tag(Some(&speaker)), 2);
    speaker.type_mask = 0;
    assert_eq!(nearby_chat_tag(Some(&speaker)), 0, "a creature");
    assert_eq!(nearby_chat_tag(None), 0, "a speaker already gone");
}

#[test]
fn chat_in_range_gates_by_distance() {
    let range_sq = 25.0f32 * 25.0;
    assert!(chat_in_range(
        0, 0, 100.0, 100.0, 0, 0, 105.0, 108.0, range_sq
    ));
    assert!(chat_in_range(
        0, 0, 100.0, 100.0, 0, 0, 125.0, 100.0, range_sq
    ));
    assert!(!chat_in_range(
        0, 0, 100.0, 100.0, 0, 0, 125.01, 100.0, range_sq
    ));
}

#[test]
fn chat_in_range_never_crosses_a_map_or_instance_boundary_regardless_of_distance() {
    assert!(!chat_in_range(
        0, 0, 100.0, 100.0, 1, 0, 100.0, 100.0, 90_000.0
    ));
    assert!(!chat_in_range(
        0, 0, 100.0, 100.0, 0, 1, 100.0, 100.0, 90_000.0
    ));
}

#[test]
fn chat_in_range_yell_reaches_further_than_say_at_the_same_distance() {
    assert!(!chat_in_range(
        0,
        0,
        0.0,
        0.0,
        0,
        0,
        200.0,
        0.0,
        SAY_RANGE_SQ
    ));
    assert!(chat_in_range(
        0,
        0,
        0.0,
        0.0,
        0,
        0,
        200.0,
        0.0,
        YELL_RANGE_SQ
    ));
}

#[test]
fn chat_in_range_gates_a_text_emote_at_the_same_25yd_boundary_as_say() {
    let range_sq = lyracore_shared::chat::broadcast_chat::TEXT_EMOTE_RANGE_YD
        * lyracore_shared::chat::broadcast_chat::TEXT_EMOTE_RANGE_YD;
    assert_eq!(range_sq, SAY_RANGE_SQ);
    assert!(chat_in_range(0, 0, 0.0, 0.0, 0, 0, 25.0, 0.0, range_sq));
    assert!(!chat_in_range(0, 0, 0.0, 0.0, 0, 0, 25.01, 0.0, range_sq));
}
