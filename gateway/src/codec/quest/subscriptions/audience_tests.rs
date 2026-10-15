use super::*;
use crate::stdb::bindings::CharacterQuest;
use crate::stdb::world_view::test_support::identity;
use crate::world::Outbound;
use wow_world_messages::vanilla::opcodes::ServerOpcodeMessage;

#[test]
fn quest_update_orders_kills_then_timer_then_full_log() {
    let old = CharacterQuest {
        id: 1,
        character_guid: 9001,
        owner_identity: identity(1),
        quest_entry: 77,
        counts: vec![1],
        rewarded: false,
        deadline_micros: 0,
        failed: false,
    };
    let updated = CharacterQuest {
        counts: vec![2],
        failed: true,
        ..old.clone()
    };
    let outbound = quest_update_packets(
        &old,
        &updated,
        &[(0, 0, 123, 4)],
        Some(Outbound::Raw {
            opcode: 1,
            body: vec![0],
        }),
    );

    assert!(matches!(
        outbound.as_slice(),
        [
            Outbound::One(ServerOpcodeMessage::SMSG_QUESTUPDATE_ADD_KILL(_)),
            Outbound::One(ServerOpcodeMessage::SMSG_QUESTUPDATE_FAILEDTIMER(_)),
            Outbound::Raw { .. },
        ]
    ));
}

#[test]
fn event_credit_announces_completion_before_the_full_log() {
    let outbound = event_credit_packets(
        77,
        Some(Outbound::Raw {
            opcode: 1,
            body: vec![0],
        }),
    );

    match outbound.as_slice() {
        [Outbound::One(ServerOpcodeMessage::SMSG_QUESTUPDATE_COMPLETE(complete)), Outbound::Raw { .. }] =>
        {
            assert_eq!(complete.quest_id, 77)
        }
        _ => panic!("expected completion then the quest log"),
    }
}

#[test]
fn a_finished_explore_objective_announces_completion_once() {
    let old = CharacterQuest {
        id: 1,
        character_guid: 9001,
        owner_identity: identity(1),
        quest_entry: 77,
        counts: vec![0],
        rewarded: false,
        deadline_micros: 0,
        failed: false,
    };
    let done = CharacterQuest {
        counts: vec![1],
        ..old.clone()
    };
    let objectives = [(3, 0, 555, 1)];

    assert!(matches!(
        quest_update_feedback(&old, &done, &objectives).as_slice(),
        [Outbound::One(ServerOpcodeMessage::SMSG_QUESTUPDATE_COMPLETE(c))] if c.quest_id == 77
    ));
    assert!(quest_update_feedback(&done, &done, &objectives).is_empty());
}
