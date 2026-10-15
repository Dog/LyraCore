use super::*;
use crate::codec;
use crate::stdb::bindings::DuelEvent;
use crate::world::Outbound;
use wow_world_messages::vanilla::opcodes::ServerOpcodeMessage;

fn duel_event(kind: u8, completion_kind: u8) -> DuelEvent {
    DuelEvent {
        id: 1,
        recipient_identity: spacetimedb_sdk::Identity::from_byte_array([0u8; 32]),
        recipient_guid: 10,
        kind,
        completion_kind,
        duel_id: 3,
        flag_guid: 0xF110_2000_0000_0003,
        flag_entry: 21680,
        initiator_guid: 10,
        challenged_guid: 20,
        winner_guid: 0,
        loser_guid: 0,
        map_id: 1,
        instance_id: 7,
        flag_x: 3.0,
        flag_y: 4.0,
        flag_z: 5.0,
        flag_orientation: 1.25,
        created_at: spacetimedb_sdk::Timestamp::UNIX_EPOCH,
        winner_name: String::new(),
        loser_name: String::new(),
    }
}

#[test]
fn duel_request_projects_one_typed_request_with_flag_arbiter_and_challenger() {
    let template = codec::GameObjectTemplateView {
        type_id: 16,
        display_id: 787,
        name: "Duel Arbiter".into(),
        data0: 0,
        data1: 0,
        data2: 0,
    };
    let row = duel_event(0, 0);
    let out = duel_event_outbound(&row, Some(&template));

    assert_eq!(out.len(), 5);
    let requests: Vec<_> = out
        .iter()
        .filter_map(|message| match message {
            Outbound::One(ServerOpcodeMessage::SMSG_DUEL_REQUESTED(request)) => Some(request),
            _ => None,
        })
        .collect();
    assert_eq!(requests.len(), 1);
    assert_eq!(
        requests[0].initiator,
        wow_world_messages::Guid::new(row.flag_guid)
    );
    assert_eq!(
        requests[0].target,
        wow_world_messages::Guid::new(row.initiator_guid)
    );
}

#[test]
fn duel_countdown_active_and_interruption_project_the_expected_edges() {
    let countdown = duel_event_outbound(&duel_event(1, 0), None);
    assert!(matches!(
        countdown.as_slice(),
        [Outbound::One(ServerOpcodeMessage::SMSG_DUEL_COUNTDOWN(message))]
            if message.time.as_secs() == 3_000
    ));

    let active = duel_event_outbound(&duel_event(2, 0), None);
    assert_eq!(active.len(), 2);
    assert!(active.iter().all(|out| matches!(out, Outbound::Raw { .. })));

    let interrupted = duel_event_outbound(&duel_event(3, 0), None);
    assert_eq!(interrupted.len(), 4);
    assert!(matches!(
        &interrupted[0],
        Outbound::One(ServerOpcodeMessage::SMSG_DUEL_COMPLETE(message))
            if !message.ended_without_interruption
    ));
    assert!(matches!(
        interrupted.last(),
        Some(Outbound::One(ServerOpcodeMessage::SMSG_DESTROY_OBJECT(_)))
    ));
}

#[test]
fn duel_winner_projects_names_and_the_terminal_reason() {
    let mut won = duel_event(3, 2);
    won.winner_guid = 10;
    won.loser_guid = 20;
    won.winner_name = "Winner".into();
    won.loser_name = "Loser".into();
    let out = duel_event_outbound(&won, None);
    assert!(matches!(
                out.as_slice(),
                [
                    Outbound::One(ServerOpcodeMessage::SMSG_DUEL_COMPLETE(_)),
                    Outbound::One(ServerOpcodeMessage::SMSG_DUEL_WINNER(message)),
    ..
                ] if message.reason == wow_world_base::shared::duel_winner_reason_vanilla_tbc_wrath::DuelWinnerReason::Won
                    && message.opponent_name == "Winner"
                    && message.initiator_name == "Loser"
            ));

    let mut fled_event = duel_event(3, 3);
    fled_event.winner_guid = 10;
    fled_event.loser_guid = 20;
    fled_event.winner_name = "Winner".into();
    fled_event.loser_name = "Loser".into();
    let fled = duel_event_outbound(&fled_event, None);
    assert!(matches!(
        &fled[1],
        Outbound::One(ServerOpcodeMessage::SMSG_DUEL_WINNER(message))
            if message.reason == wow_world_base::shared::duel_winner_reason_vanilla_tbc_wrath::DuelWinnerReason::Fled
    ));
}

#[test]
fn duel_boundary_edges_project_only_the_recipient_transition() {
    let out_of_bounds = duel_event_outbound(&duel_event(4, 0), None);
    assert!(matches!(
        out_of_bounds.as_slice(),
        [Outbound::One(ServerOpcodeMessage::SMSG_DUEL_OUTOFBOUNDS)]
    ));

    let in_bounds = duel_event_outbound(&duel_event(5, 0), None);
    assert!(matches!(
        in_bounds.as_slice(),
        [Outbound::One(ServerOpcodeMessage::SMSG_DUEL_INBOUNDS)]
    ));
}
