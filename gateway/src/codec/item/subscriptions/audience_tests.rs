use super::*;
use crate::stdb::bindings::ItemInstance;
use crate::stdb::world_index::{CellKey, EntityLayer};
use crate::stdb::world_view::test_support::{identity, queued_job, viewer_with_tx};
use crate::stdb::world_view::WorldView;
use crate::world::{Outbound, SessionTx};
use std::sync::{Arc, Mutex};
use wow_world_messages::vanilla::opcodes::ServerOpcodeMessage;

fn worn_item(slot: u8) -> ItemInstance {
    ItemInstance {
        guid: 700,
        entry: 1337,
        owner_identity: identity(1),
        owner_guid: 9001,
        slot,
        stack_count: 1,
        durability: 20,
        created_at: spacetimedb_sdk::Timestamp::UNIX_EPOCH,
        enchant_id: 7745,
        soulbound: false,
        random_property_id: 117,
        item_text_id: 0,
    }
}

fn visible_items(out: Vec<Outbound>) -> Vec<(u8, u32, u32, u32)> {
    use wow_world_messages::vanilla::{Object, ServerMessage, UpdateMask, VisibleItemIndex};
    let mut items = Vec::new();
    for message in out {
        let Outbound::One(ServerOpcodeMessage::SMSG_UPDATE_OBJECT(update)) = message else {
            panic!("peers receive only visible item VALUES");
        };
        let [Object::Values {
            guid1,
            mask1: UpdateMask::Player(mask),
        }] = update.objects.as_slice()
        else {
            panic!("peers receive no private item objects");
        };
        assert_eq!(guid1.guid(), 9001);
        for slot in 0..=18 {
            if let Some(item) = mask.player_visible_item(VisibleItemIndex::try_from(slot).unwrap())
            {
                assert_eq!(item.enchants[1], 0);
                items.push((slot, item.item, item.random_property_id, item.enchants[0]));
            }
        }
        let mut bytes = Vec::new();
        update.write_unencrypted_server(&mut bytes).unwrap();
        let decoded = lyracore_shared::values_mask::parse_values_updates(&bytes[4..]);
        assert_eq!(decoded.len(), 1);
        // Build 5875 exposes nineteen 12-word visible items starting at field 258.
        assert!(
            decoded[0]
                .fields
                .iter()
                .all(|&(field, _)| (258..486).contains(&field)),
            "no TYPE, inventory pointers, item fields or Character sheet fields"
        );
    }
    items
}

#[test]
fn visible_gear_changes_reach_peers_with_current_slot_contents() {
    let view = WorldView::new(true);
    let (tx, rx) = SessionTx::with_depth(0);
    let peer = viewer_with_tx(2, 9002, identity(2), tx);
    peer.created.lock().unwrap().insert(9001);
    let anchor = CellKey::at(0, 0, 0, 0);
    view.add_viewer_on_shard(peer, anchor, 0);
    view.spatial
        .upsert_entity(EntityLayer::WorldEntity, 9001, anchor, 0, 0);
    let committed = Arc::new(Mutex::new(Vec::<ItemInstance>::new()));
    let read: super::ItemSlotRead = {
        let committed = committed.clone();
        Arc::new(move |owner, slot| {
            committed
                .lock()
                .unwrap()
                .iter()
                .find(|item| item.owner_guid == owner && item.slot == slot)
                .map(crate::stdb::reads::view_of_item_row)
        })
    };
    for slot in [0, 8, 18] {
        let item = worn_item(slot);
        *committed.lock().unwrap() = vec![item.clone()];
        item_visible_changed(&view, 0, None, Some(&item), read.clone());
        assert_eq!(visible_items(queued_job(&rx)), [(slot, 1337, 117, 823)]);
    }
    let item = worn_item(8);

    for changed in [
        ItemInstance {
            enchant_id: 7748,
            ..item.clone()
        },
        ItemInstance {
            enchant_id: 0,
            ..item.clone()
        },
        ItemInstance {
            entry: 42,
            ..item.clone()
        },
        ItemInstance {
            random_property_id: 118,
            ..item.clone()
        },
    ] {
        *committed.lock().unwrap() = vec![changed.clone()];
        item_visible_changed(&view, 0, Some(&item), Some(&changed), read.clone());
        let enchant = match changed.enchant_id {
            7748 => 724,
            0 => 0,
            _ => 823,
        };
        assert_eq!(
            visible_items(queued_job(&rx)),
            [(8, changed.entry, changed.random_property_id, enchant)]
        );
    }

    let bagged = ItemInstance {
        slot: 23,
        ..item.clone()
    };
    *committed.lock().unwrap() = vec![bagged.clone()];
    item_visible_changed(&view, 0, Some(&item), Some(&bagged), read.clone());
    assert_eq!(visible_items(queued_job(&rx)), [(8, 0, 0, 0)]);
    *committed.lock().unwrap() = vec![item.clone()];
    item_visible_changed(&view, 0, Some(&bagged), Some(&item), read.clone());
    assert_eq!(visible_items(queued_job(&rx)), [(8, 1337, 117, 823)]);

    committed.lock().unwrap().clear();
    item_visible_changed(&view, 0, Some(&item), None, read);
    assert_eq!(visible_items(queued_job(&rx)), [(8, 0, 0, 0)]);
    assert!(rx.try_recv().is_err());
}

#[test]
fn visible_gear_swap_and_delete_read_replacements_when_the_writer_runs() {
    let view = WorldView::new(true);
    let (tx, rx) = SessionTx::with_depth(0);
    let peer = viewer_with_tx(2, 9002, identity(2), tx);
    peer.created.lock().unwrap().insert(9001);
    let anchor = CellKey::at(0, 0, 0, 0);
    view.add_viewer_on_shard(peer, anchor, 0);
    view.spatial
        .upsert_entity(EntityLayer::WorldEntity, 9001, anchor, 0, 0);
    let old = worn_item(8);
    let moved = ItemInstance {
        slot: 23,
        ..old.clone()
    };
    let committed = Arc::new(Mutex::new(vec![old.clone()]));
    let read: super::ItemSlotRead = {
        let committed = committed.clone();
        Arc::new(move |owner, slot| {
            committed
                .lock()
                .unwrap()
                .iter()
                .find(|item| item.owner_guid == owner && item.slot == slot)
                .map(crate::stdb::reads::view_of_item_row)
        })
    };
    item_visible_changed(&view, 0, Some(&old), Some(&moved), read.clone());
    item_visible_changed(&view, 0, Some(&old), None, read);
    *committed.lock().unwrap() = vec![ItemInstance {
        guid: 701,
        entry: 42,
        enchant_id: 7748,
        ..old
    }];
    for _ in 0..2 {
        assert_eq!(visible_items(queued_job(&rx)), [(8, 42, 117, 724)]);
    }
    assert!(rx.try_recv().is_err());
}

#[test]
fn visible_gear_is_scoped_to_peers_that_still_see_the_character() {
    let view = WorldView::new(true);
    let anchor = CellKey::at(0, 0, 0, 0);
    view.spatial
        .upsert_entity(EntityLayer::WorldEntity, 9001, anchor, 0, 0);
    let item = worn_item(8);
    let read: super::ItemSlotRead =
        Arc::new(|_, _| Some(crate::stdb::reads::view_of_item_row(&worn_item(8))));
    let mut queues = Vec::new();
    for (session, guid, cell, shard) in [
        (1, 9001, anchor, 0),
        (2, 9002, anchor, 0),
        (3, 9003, CellKey::at(0, 0, 10, 0), 0),
        (4, 9004, CellKey::at(0, 1, 0, 0), 0),
        (5, 9005, CellKey::at(1, 0, 0, 0), 0),
        (6, 9006, anchor, 1),
    ] {
        let (tx, rx) = SessionTx::with_depth(0);
        let viewer = viewer_with_tx(session, guid, identity(session as u8), tx);
        viewer.created.lock().unwrap().insert(9001);
        view.add_viewer_on_shard(viewer.clone(), cell, shard);
        queues.push((viewer, rx));
    }
    let (peer, rx) = &queues[1];
    item_visible_changed(&view, 0, None, Some(&item), read.clone());
    peer.created.lock().unwrap().remove(&9001);
    assert!(queued_job(rx).is_empty(), "hidden before the writer runs");
    item_visible_changed(&view, 0, None, Some(&item), read.clone());
    peer.created.lock().unwrap().insert(9001);
    assert_eq!(
        visible_items(queued_job(rx)),
        [(8, 1337, 117, 823)],
        "CREATE ran before the gear job"
    );
    item_visible_changed(&view, 0, None, Some(&item), read.clone());
    view.remove_viewer(peer.session);
    assert!(queued_job(rx).is_empty(), "departed World Session");
    for (_, rx) in &queues {
        assert!(
            rx.try_recv().is_err(),
            "owner, distant, other map/instance/Shard receive no peer job"
        );
    }
    view.spatial
        .upsert_entity(EntityLayer::WorldEntity, 9001, anchor, 1, 0);
    item_visible_changed(&view, 0, None, Some(&item), read);
    for (_, rx) in &queues {
        assert!(
            rx.try_recv().is_err(),
            "source Shard cannot relay after Transfer"
        );
    }
}

#[test]
fn visible_gear_ignores_bagged_items_and_changes_to_private_fields() {
    let view = WorldView::new(true);
    let anchor = CellKey::at(0, 0, 0, 0);
    let (tx, rx) = SessionTx::with_depth(0);
    view.add_viewer_on_shard(viewer_with_tx(2, 9002, identity(2), tx), anchor, 0);
    view.spatial
        .upsert_entity(EntityLayer::WorldEntity, 9001, anchor, 0, 0);
    let read: super::ItemSlotRead = Arc::new(|_, _| panic!("no gear read needed"));
    for slot in [19, 23, 120] {
        let bagged = worn_item(slot);
        item_visible_changed(&view, 0, None, Some(&bagged), read.clone());
        item_visible_changed(&view, 0, Some(&bagged), None, read.clone());
        item_visible_changed(
            &view,
            0,
            Some(&bagged),
            Some(&ItemInstance {
                enchant_id: 0,
                ..bagged.clone()
            }),
            read.clone(),
        );
    }
    let old = worn_item(8);
    let new = ItemInstance {
        durability: 0,
        stack_count: 3,
        soulbound: true,
        ..old.clone()
    };
    item_visible_changed(&view, 0, Some(&old), Some(&new), read);
    assert!(rx.try_recv().is_err());
}

#[test]
fn item_rows_route_only_to_the_owner_and_follow_transfer_replacement() {
    let view = WorldView::new(true);
    let (source_tx, source_rx) = SessionTx::with_depth(0);
    let (other_tx, other_rx) = SessionTx::with_depth(0);
    let (destination_tx, destination_rx) = SessionTx::with_depth(0);
    let source = viewer_with_tx(1, 9001, identity(1), source_tx);
    let other = viewer_with_tx(2, 9002, identity(2), other_tx);
    let destination = viewer_with_tx(3, 9001, identity(1), destination_tx);
    let anchor = CellKey::at(0, 0, 0, 0);
    view.add_viewer_on_shard(source.clone(), anchor, 0);
    view.add_viewer_on_shard(other, anchor, 0);

    item_owner_job(&view, 0, source.self_guid, |_| {
        vec![Outbound::Raw {
            opcode: 1,
            body: vec![1],
        }]
    });
    assert!(matches!(
        queued_job(&source_rx).as_slice(),
        [Outbound::Raw { .. }]
    ));
    assert!(
        other_rx.try_recv().is_err(),
        "an unrelated item owner receives nothing"
    );

    view.remove_viewer(source.session);
    item_owner_job(&view, 0, source.self_guid, |_| {
        vec![Outbound::Raw {
            opcode: 2,
            body: vec![2],
        }]
    });
    assert!(
        source_rx.try_recv().is_err(),
        "a removed source viewer receives no cascade delete"
    );

    view.add_viewer_on_shard(destination, anchor, 1);
    item_owner_job(&view, 0, source.self_guid, |_| {
        vec![Outbound::Raw {
            opcode: 2,
            body: vec![2],
        }]
    });
    assert!(
        destination_rx.try_recv().is_err(),
        "a source-shard delta applied after destination registration must be dropped"
    );
    item_owner_job(&view, 1, source.self_guid, |_| {
        vec![Outbound::Raw {
            opcode: 3,
            body: vec![3],
        }]
    });
    assert!(matches!(
        queued_job(&destination_rx).as_slice(),
        [Outbound::Raw { opcode: 3, .. }]
    ));
}
