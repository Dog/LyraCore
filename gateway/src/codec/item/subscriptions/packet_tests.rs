use super::*;
use crate::codec;
use crate::stdb::bindings::{ItemInstance, TradeEvent};
use crate::stdb::reads::view_of_item_row;
use crate::world::Outbound;
use wow_world_messages::vanilla::opcodes::ServerOpcodeMessage;

#[test]
fn trade_event_kinds_decode_to_their_trade_status_variants() {
    use lyracore_shared::trade::event_kind as kind;
    use wow_world_messages::vanilla::SMSG_TRADE_STATUS;

    let event = |k: u8| TradeEvent {
        id: 1,
        recipient_identity: spacetimedb_sdk::Identity::from_byte_array([0u8; 32]),
        kind: k,
        other_guid: 77,
        created_at: spacetimedb_sdk::Timestamp::UNIX_EPOCH,
        recipient_guid: 1,
        payload: String::new(),
    };
    let status_of = |k: u8| -> SMSG_TRADE_STATUS {
        let out = trade_event_outbound(&event(k));
        assert_eq!(out.len(), 1, "kind {k} must decode to exactly one packet");
        match &out[0] {
            Outbound::One(ServerOpcodeMessage::SMSG_TRADE_STATUS(s)) => **s,
            Outbound::One(other) => panic!("kind {k}: expected SMSG_TRADE_STATUS, got {other}"),
            _ => panic!("kind {k}: expected a single SMSG_TRADE_STATUS packet"),
        }
    };

    assert_eq!(
        status_of(kind::BEGIN_TRADE),
        SMSG_TRADE_STATUS::BeginTrade {
            unknown1: wow_world_messages::Guid::new(77)
        },
        "BeginTrade must carry the initiator guid off the row"
    );
    assert_eq!(status_of(kind::OPEN_WINDOW), SMSG_TRADE_STATUS::OpenWindow);
    assert_eq!(
        status_of(kind::TRADE_CANCELED),
        SMSG_TRADE_STATUS::TradeCanceled
    );
    assert_eq!(status_of(kind::BUSY), SMSG_TRADE_STATUS::Busy);
    assert_eq!(status_of(kind::NO_TARGET), SMSG_TRADE_STATUS::NoTarget);
    assert_eq!(
        status_of(kind::TARGET_TO_FAR),
        SMSG_TRADE_STATUS::TargetToFar
    );
    assert_eq!(
        status_of(kind::WRONG_FACTION),
        SMSG_TRADE_STATUS::WrongFaction
    );
    assert_eq!(status_of(kind::YOU_DEAD), SMSG_TRADE_STATUS::YouDead);
    assert_eq!(status_of(kind::TARGET_DEAD), SMSG_TRADE_STATUS::TargetDead);
    assert_eq!(status_of(kind::IGNORE_YOU), SMSG_TRADE_STATUS::IgnoreYou);
    assert_eq!(
        status_of(kind::TRADE_ACCEPT),
        SMSG_TRADE_STATUS::TradeAccept
    );
    assert_eq!(
        status_of(kind::BACK_TO_TRADE),
        SMSG_TRADE_STATUS::BackToTrade
    );
    assert_eq!(
        status_of(kind::TRADE_COMPLETE),
        SMSG_TRADE_STATUS::TradeComplete
    );
    let full_self = status_of(kind::INV_FULL_SELF);
    let full_partner = status_of(kind::INV_FULL_PARTNER);
    match (&full_self, &full_partner) {
        (
            SMSG_TRADE_STATUS::CloseWindow {
                target_error: false,
                inventory_result: a,
                ..
            },
            SMSG_TRADE_STATUS::CloseWindow {
                target_error: true,
                inventory_result: b,
                ..
            },
        ) => {
            assert_eq!(
                *a,
                wow_world_messages::vanilla::InventoryResult::InventoryFull
            );
            assert_eq!(
                *b,
                wow_world_messages::vanilla::InventoryResult::InventoryFull
            );
        }
        other => panic!("expected CloseWindow pair with flipped target_error, got {other:?}"),
    }
    match (
        &status_of(kind::GOLD_FAIL_SELF),
        &status_of(kind::GOLD_FAIL_PARTNER),
    ) {
        (
            SMSG_TRADE_STATUS::CloseWindow {
                target_error: false,
                inventory_result: a,
                ..
            },
            SMSG_TRADE_STATUS::CloseWindow {
                target_error: true,
                inventory_result: b,
                ..
            },
        ) => {
            assert_eq!(
                *a,
                wow_world_messages::vanilla::InventoryResult::NotEnoughMoney
            );
            assert_eq!(
                *b,
                wow_world_messages::vanilla::InventoryResult::NotEnoughMoney
            );
        }
        other => panic!("expected NotEnoughMoney CloseWindow pair, got {other:?}"),
    }

    assert!(
        trade_event_outbound(&event(200)).is_empty(),
        "an unknown kind must drop, not guess a status"
    );
}

#[test]
fn offer_events_decode_to_the_extended_status_with_the_kind_deciding_self_player() {
    use lyracore_shared::trade::{encode_offer, event_kind as kind, OfferSlot};
    use wow_world_messages::vanilla::SMSG_TRADE_STATUS_EXTENDED;

    let payload = encode_offer(
        1_2345,
        &[OfferSlot {
            trade_slot: 6,
            entry: 6948,
            display_id: 6418,
            stack_count: 5,
            enchantment: 2564,
            durability: 34,
            max_durability: 40,
            random_property_id: 0,
        }],
    );
    let event = |k: u8, p: &str| TradeEvent {
        id: 1,
        recipient_identity: spacetimedb_sdk::Identity::from_byte_array([0u8; 32]),
        kind: k,
        other_guid: 77,
        created_at: spacetimedb_sdk::Timestamp::UNIX_EPOCH,
        recipient_guid: 1,
        payload: p.to_string(),
    };
    let extended = |k: u8| -> SMSG_TRADE_STATUS_EXTENDED {
        let out = trade_event_outbound(&event(k, &payload));
        match out.first() {
            Some(Outbound::One(ServerOpcodeMessage::SMSG_TRADE_STATUS_EXTENDED(m))) => **m,
            _ => panic!("kind {k}: expected one SMSG_TRADE_STATUS_EXTENDED"),
        }
    };

    let own = extended(kind::OFFER_SELF);
    assert!(
        !own.self_player,
        "OFFER_SELF describes your own window: wire byte 0"
    );
    let theirs = extended(kind::OFFER_PARTNER);
    assert!(
        theirs.self_player,
        "OFFER_PARTNER describes the partner's side: wire byte 1"
    );

    assert_eq!(own.money_in_trade.as_int(), 1_2345);
    assert_eq!((own.trade_slot_count1, own.trade_slot_count2), (7, 7));
    let s = &own.trade_slots[6];
    assert_eq!(
        (s.trade_slot_number, s.item, s.display_id, s.stack_count),
        (6, 6948, 6418, 5)
    );
    assert_eq!(
        (s.enchantment, s.durability, s.max_durability),
        (2564, 34, 40)
    );
    assert_eq!(
        own.trade_slots[0],
        Default::default(),
        "untouched slots stay zeroed"
    );

    assert!(
        trade_event_outbound(&event(kind::OFFER_SELF, "not|a,valid,payload")).is_empty(),
        "a malformed offer payload must drop the packet"
    );
}

#[test]
fn a_compatibility_enchant_in_an_offer_goes_out_as_its_client_enchantment() {
    use lyracore_shared::trade::{encode_offer, event_kind as kind, OfferSlot};
    let slot = |trade_slot, enchantment| OfferSlot {
        trade_slot,
        entry: 6948,
        display_id: 6418,
        stack_count: 1,
        enchantment,
        durability: 40,
        max_durability: 40,
        random_property_id: 0,
    };
    let payload = encode_offer(0, &[slot(0, 7745), slot(1, 7748)]);
    let event = TradeEvent {
        id: 1,
        recipient_identity: spacetimedb_sdk::Identity::from_byte_array([0u8; 32]),
        kind: kind::OFFER_SELF,
        other_guid: 77,
        created_at: spacetimedb_sdk::Timestamp::UNIX_EPOCH,
        recipient_guid: 1,
        payload,
    };
    let out = trade_event_outbound(&event);
    let Some(Outbound::One(ServerOpcodeMessage::SMSG_TRADE_STATUS_EXTENDED(extended))) =
        out.first()
    else {
        panic!("expected one SMSG_TRADE_STATUS_EXTENDED");
    };
    assert_eq!(extended.trade_slots[0].enchantment, 823);
    assert_eq!(extended.trade_slots[1].enchantment, 724);
}

#[test]
fn bag_content_parts_boundaries() {
    assert_eq!(codec::bag_content_parts(119), None);
    assert_eq!(codec::bag_content_parts(120), Some((19, 0)));
    assert_eq!(codec::bag_content_parts(137), Some((19, 17)));
    assert_eq!(codec::bag_content_parts(138), Some((20, 0)));
    assert_eq!(codec::bag_content_parts(191), Some((22, 17)));
    assert_eq!(codec::bag_content_parts(192), None);
}

fn item_view(guid: u64, entry: u32) -> codec::ItemInstanceView {
    codec::ItemInstanceView {
        guid,
        entry,
        ..Default::default()
    }
}

fn item_row(slot: u8, enchant_id: u32) -> ItemInstance {
    ItemInstance {
        guid: 0x4000_0000_0000_0007,
        entry: 1337,
        owner_identity: spacetimedb_sdk::Identity::from_byte_array([0u8; 32]),
        owner_guid: 7,
        slot,
        stack_count: 1,
        durability: 20,
        created_at: spacetimedb_sdk::Timestamp::UNIX_EPOCH,
        enchant_id,
        soulbound: false,
        random_property_id: 0,
        item_text_id: 0,
    }
}

fn enchant_words(out: &[Outbound]) -> (Option<u32>, Option<u32>, Vec<Vec<u16>>) {
    use wow_world_messages::vanilla::{Object, ServerMessage, UpdateMask, VisibleItemIndex};
    let (mut item_word, mut visible_word) = (None, None);
    let mut field_indexes = Vec::new();
    for message in out {
        let Outbound::One(ServerOpcodeMessage::SMSG_UPDATE_OBJECT(update)) = message else {
            continue;
        };
        let mut bytes = Vec::new();
        update.write_unencrypted_server(&mut bytes).unwrap();
        for values in lyracore_shared::values_mask::parse_values_updates(&bytes[4..]) {
            field_indexes.push(values.fields.iter().map(|&(idx, _)| idx).collect());
        }
        match &update.objects[0] {
            Object::Values {
                mask1: UpdateMask::Item(item),
                ..
            } => item_word = item.item_enchantment().map(|word| word as u32),
            Object::Values {
                mask1: UpdateMask::Player(player),
                ..
            } => {
                visible_word = player
                    .player_visible_item(VisibleItemIndex::Index8)
                    .map(|visible| visible.enchants[0])
            }
            _ => {}
        }
    }
    (item_word, visible_word, field_indexes)
}

#[test]
fn enchanting_a_worn_item_in_place_updates_its_enchant_word_and_visible_item() {
    for (stored, client) in [(7745, 823), (7748, 724), (2564, 2564)] {
        let out = enchant_change_outbound(7, &item_row(8, 0), &item_row(8, stored));
        let (item_word, visible_word, fields) = enchant_words(&out);
        assert_eq!(item_word, Some(client), "stored {stored}");
        assert_eq!(visible_word, Some(client), "stored {stored}");
        assert!(
            fields.iter().all(|idx| !idx.contains(&2)),
            "OBJECT_FIELD_TYPE must stay off the partial update"
        );
    }
}

#[test]
fn enchanting_a_bagged_item_in_place_updates_only_its_enchant_word() {
    let out = enchant_change_outbound(7, &item_row(23, 0), &item_row(23, 7745));
    let (item_word, visible_word, _) = enchant_words(&out);
    assert_eq!(item_word, Some(823));
    assert_eq!(visible_word, None);
}

#[test]
fn a_change_that_leaves_the_enchant_alone_sends_no_enchant_word() {
    let mut worn = item_row(8, 7745);
    worn.durability = 3;
    assert!(enchant_change_outbound(7, &item_row(8, 7745), &worn).is_empty());
}

#[test]
fn an_enchant_change_on_a_guild_charter_leaves_its_petition_id_alone() {
    let charter = |enchant_id| ItemInstance {
        entry: lyracore_shared::guild::GUILD_CHARTER_ENTRY,
        ..item_row(23, enchant_id)
    };
    assert!(enchant_change_outbound(7, &charter(0), &charter(7745)).is_empty());
}

#[test]
fn removing_a_permanent_enchant_in_place_clears_the_word() {
    let out = enchant_change_outbound(7, &item_row(8, 7745), &item_row(8, 0));
    let (item_word, visible_word, _) = enchant_words(&out);
    assert_eq!((item_word, visible_word), (Some(0), Some(0)));
}

#[test]
fn a_slot_change_with_an_enchant_change_leaves_the_visible_item_to_the_slot_projection() {
    let out = enchant_change_outbound(7, &item_row(23, 0), &item_row(8, 7745));
    let (item_word, visible_word, _) = enchant_words(&out);
    assert_eq!((item_word, visible_word), (Some(823), None));
}

#[test]
fn a_worn_item_moved_into_a_slot_shows_its_client_enchantment_on_the_visible_item() {
    use wow_world_messages::vanilla::{Object, UpdateMask, VisibleItemIndex};
    let committed = item_row(8, 7748);
    let mut out = Vec::new();
    append_final_item_slots(7, 23, 8, &mut out, |slot| {
        (slot == 8).then(|| view_of_item_row(&committed))
    });
    let mut words = Vec::new();
    for message in &out {
        let Outbound::One(ServerOpcodeMessage::SMSG_UPDATE_OBJECT(update)) = message else {
            continue;
        };
        if let Object::Values {
            mask1: UpdateMask::Player(player),
            ..
        } = &update.objects[0]
        {
            words.extend(
                player
                    .player_visible_item(VisibleItemIndex::Index8)
                    .map(|visible| visible.enchants),
            );
        }
    }
    assert_eq!(words, [[724, 0]]);
}

#[test]
fn occupied_equipment_swap_projects_committed_slot_occupants() {
    use wow_world_messages::vanilla::{Guid, ItemSlot, Object, UpdateMask, VisibleItemIndex};

    const CHARACTER: u64 = 7;
    const EQUIPMENT_SLOT: u8 = 0;
    const BACKPACK_SLOT: u8 = 23;
    const DISPLACED_GUID: u64 = 101;
    const INCOMING_GUID: u64 = 202;
    const INCOMING_ENTRY: u32 = 1337;

    let committed = [
        (EQUIPMENT_SLOT, INCOMING_GUID, INCOMING_ENTRY),
        (BACKPACK_SLOT, DISPLACED_GUID, 42),
    ];
    let item_in_slot = |slot| {
        committed
            .iter()
            .find(|item| item.0 == slot)
            .map(|item| codec::ItemInstanceView {
                guid: item.1,
                entry: item.2,
                ..Default::default()
            })
    };
    let mut outbound = Vec::new();

    append_final_item_slots(
        CHARACTER,
        EQUIPMENT_SLOT,
        BACKPACK_SLOT,
        &mut outbound,
        item_in_slot,
    );
    append_final_item_slots(
        CHARACTER,
        BACKPACK_SLOT,
        EQUIPMENT_SLOT,
        &mut outbound,
        item_in_slot,
    );

    let mut equipment = Vec::new();
    let mut backpack = Vec::new();
    let mut visible_head = Vec::new();
    for message in &outbound {
        let Outbound::One(ServerOpcodeMessage::SMSG_UPDATE_OBJECT(update)) = message else {
            continue;
        };
        let Object::Values {
            mask1: UpdateMask::Player(player),
            ..
        } = &update.objects[0]
        else {
            continue;
        };
        equipment.extend(player.player_field_inv(ItemSlot::Head));
        backpack.extend(player.player_field_inv(ItemSlot::Inventory0));
        visible_head.extend(
            player
                .player_visible_item(VisibleItemIndex::Index0)
                .map(|item| item.item),
        );
    }

    assert_eq!(equipment, [Guid::new(INCOMING_GUID)]);
    assert_eq!(backpack, [Guid::new(DISPLACED_GUID)]);
    assert_eq!(visible_head, [INCOMING_ENTRY]);
    assert!(!equipment.contains(&Guid::new(0)));
    assert!(!backpack.contains(&Guid::new(0)));
}

#[test]
fn empty_equipment_destination_clears_backpack_and_sets_equipment() {
    use wow_world_messages::vanilla::{Guid, ItemSlot, Object, UpdateMask};

    let mut outbound = Vec::new();
    append_final_item_slots(7, 23, 0, &mut outbound, |slot| {
        (slot == 0).then(|| item_view(202, 1337))
    });

    let mut equipment = Vec::new();
    let mut backpack = Vec::new();
    for message in &outbound {
        let Outbound::One(ServerOpcodeMessage::SMSG_UPDATE_OBJECT(update)) = message else {
            continue;
        };
        let Object::Values {
            mask1: UpdateMask::Player(player),
            ..
        } = &update.objects[0]
        else {
            continue;
        };
        equipment.extend(player.player_field_inv(ItemSlot::Head));
        backpack.extend(player.player_field_inv(ItemSlot::Inventory0));
    }

    assert_eq!(equipment, [Guid::new(202)]);
    assert_eq!(backpack, [Guid::new(0)]);
}

#[test]
fn ordinary_backpack_move_clears_source_and_sets_destination() {
    use wow_world_messages::vanilla::{Guid, ItemSlot, Object, UpdateMask};

    let mut outbound = Vec::new();
    append_final_item_slots(7, 23, 24, &mut outbound, |slot| {
        (slot == 24).then(|| item_view(202, 1337))
    });

    let mut source = Vec::new();
    let mut destination = Vec::new();
    for message in &outbound {
        let Outbound::One(ServerOpcodeMessage::SMSG_UPDATE_OBJECT(update)) = message else {
            continue;
        };
        let Object::Values {
            mask1: UpdateMask::Player(player),
            ..
        } = &update.objects[0]
        else {
            continue;
        };
        source.extend(player.player_field_inv(ItemSlot::Inventory0));
        destination.extend(player.player_field_inv(ItemSlot::Inventory1));
    }

    assert_eq!(source, [Guid::new(0)]);
    assert_eq!(destination, [Guid::new(202)]);
}
