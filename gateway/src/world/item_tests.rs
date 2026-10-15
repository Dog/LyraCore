use super::handlers::{InMemoryItemActions, Item};
use super::*;

fn player() -> ProtocolSession {
    ProtocolSession::in_world(7, 1)
}

fn try_run(
    actions: &InMemoryItemActions,
    mut player: ProtocolSession,
    msg: impl Into<ClientOpcodeMessage>,
) -> Result<Vec<Outbound>> {
    Ok(Item::handle(actions, &mut player, ProtocolRequest::Message(msg.into()))?.outbound)
}

fn autoequip(source_slot: u8) -> CMSG_AUTOEQUIP_ITEM {
    CMSG_AUTOEQUIP_ITEM {
        source_bag: 255,
        source_slot,
    }
}

#[test]
fn equip_item_err_sends_smsg_inventory_change_failure() {
    let actions = InMemoryItemActions {
        equip_result: Some(ItemActionResult::Refused(ItemRefusal::CannotEquip)),
        ..Default::default()
    };
    for slot in [24, 25] {
        let sent = try_run(&actions, player(), autoequip(slot)).unwrap();
        assert!(
            matches!(
                sent.as_slice(),
                [Outbound::One(
                    ServerOpcodeMessage::SMSG_INVENTORY_CHANGE_FAILURE(_)
                )]
            ),
            "slot {slot}: expected one SMSG_INVENTORY_CHANGE_FAILURE"
        );
    }
}

#[test]
fn item_action_before_player_login_answers_a_refusal_without_a_durable_request() {
    let actions = InMemoryItemActions::default();
    let player = ProtocolSession::new(7, "TESTER".into());
    let sent = try_run(&actions, player, autoequip(24))
        .expect("a session without a Character stays alive");
    assert!(matches!(
        sent.as_slice(),
        [Outbound::One(
            ServerOpcodeMessage::SMSG_INVENTORY_CHANGE_FAILURE(_)
        )]
    ));
    assert!(actions.equip_requests.lock().unwrap().is_empty());
}

#[test]
fn item_reducer_transport_loss_ends_the_world_session() {
    let actions = InMemoryItemActions {
        transport_lost: true,
        ..Default::default()
    };
    try_run(&actions, player(), autoequip(24))
        .err()
        .expect("a lost item reducer transport must be session-fatal");
}
