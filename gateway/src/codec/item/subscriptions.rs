use crate::codec::{self};
use crate::stdb::bindings::*;
use crate::stdb::reads::view_of_item_row;
use crate::stdb::world_index::{EntityLayer, ShardId};
use crate::stdb::world_view::*;
use crate::stdb::Coordinator;
use crate::world::Outbound;
use lyracore_shared::item_property::client_enchantment_id;
use lyracore_shared::spatial::BOX_HALF_SPAN;
use spacetimedb_sdk::Table;
use std::sync::{Arc, OnceLock};
use wow_world_messages::vanilla::opcodes::ServerOpcodeMessage;

pub(crate) fn trade_event_outbound(row: &TradeEvent) -> Vec<Outbound> {
    use lyracore_shared::trade::event_kind as kind;
    use wow_world_messages::vanilla::SMSG_TRADE_STATUS;
    if row.kind == kind::OFFER_SELF || row.kind == kind::OFFER_PARTNER {
        // The misnamed self_player byte is 1 for the partner's data and 0 for one's own.
        return match trade_offer_extended(row.kind == kind::OFFER_PARTNER, &row.payload) {
            Some(msg) => vec![Outbound::One(
                ServerOpcodeMessage::SMSG_TRADE_STATUS_EXTENDED(Box::new(msg)),
            )],
            None => {
                log::warn!(
                    "trade relay: unparseable offer payload {:?} (event {})",
                    row.payload,
                    row.id
                );
                Vec::new()
            }
        };
    }
    let status = match row.kind {
        kind::BEGIN_TRADE => Some(SMSG_TRADE_STATUS::BeginTrade {
            unknown1: wow_world_messages::Guid::new(row.other_guid),
        }),
        kind::OPEN_WINDOW => Some(SMSG_TRADE_STATUS::OpenWindow),
        kind::TRADE_CANCELED => Some(SMSG_TRADE_STATUS::TradeCanceled),
        kind::BUSY => Some(SMSG_TRADE_STATUS::Busy),
        kind::NO_TARGET => Some(SMSG_TRADE_STATUS::NoTarget),
        kind::TARGET_TO_FAR => Some(SMSG_TRADE_STATUS::TargetToFar),
        kind::WRONG_FACTION => Some(SMSG_TRADE_STATUS::WrongFaction),
        kind::YOU_DEAD => Some(SMSG_TRADE_STATUS::YouDead),
        kind::TARGET_DEAD => Some(SMSG_TRADE_STATUS::TargetDead),
        kind::IGNORE_YOU => Some(SMSG_TRADE_STATUS::IgnoreYou),
        kind::TRADE_ACCEPT => Some(SMSG_TRADE_STATUS::TradeAccept),
        kind::BACK_TO_TRADE => Some(SMSG_TRADE_STATUS::BackToTrade),
        kind::TRADE_COMPLETE => Some(SMSG_TRADE_STATUS::TradeComplete),
        kind::INV_FULL_SELF => Some(SMSG_TRADE_STATUS::CloseWindow {
            inventory_result: wow_world_messages::vanilla::InventoryResult::InventoryFull,
            item_limit_category_id: 0,
            target_error: false,
        }),
        kind::INV_FULL_PARTNER => Some(SMSG_TRADE_STATUS::CloseWindow {
            inventory_result: wow_world_messages::vanilla::InventoryResult::InventoryFull,
            item_limit_category_id: 0,
            target_error: true,
        }),
        kind::GOLD_FAIL_SELF => Some(SMSG_TRADE_STATUS::CloseWindow {
            inventory_result: wow_world_messages::vanilla::InventoryResult::NotEnoughMoney,
            item_limit_category_id: 0,
            target_error: false,
        }),
        kind::GOLD_FAIL_PARTNER => Some(SMSG_TRADE_STATUS::CloseWindow {
            inventory_result: wow_world_messages::vanilla::InventoryResult::NotEnoughMoney,
            item_limit_category_id: 0,
            target_error: true,
        }),
        other => {
            log::warn!("trade relay: unknown kind {other} (event {})", row.id);
            None
        }
    };
    match status {
        Some(s) => vec![Outbound::One(ServerOpcodeMessage::SMSG_TRADE_STATUS(
            Box::new(s),
        ))],
        None => Vec::new(),
    }
}

pub(crate) fn trade_offer_extended(
    describes_partner: bool,
    payload: &str,
) -> Option<wow_world_messages::vanilla::SMSG_TRADE_STATUS_EXTENDED> {
    use wow_world_messages::vanilla::{TradeSlot, SMSG_TRADE_STATUS_EXTENDED};
    let (gold, views) = lyracore_shared::trade::decode_offer(payload)?;
    let mut trade_slots = [TradeSlot::default(); 7];
    for v in views {
        let slot = trade_slots.get_mut(v.trade_slot as usize)?;
        *slot = TradeSlot {
            trade_slot_number: v.trade_slot,
            item: v.entry,
            display_id: v.display_id,
            stack_count: v.stack_count,
            enchantment: client_enchantment_id(v.enchantment),
            item_random_properties_id: v.random_property_id,
            max_durability: v.max_durability,
            durability: v.durability,
            ..TradeSlot::default()
        };
    }
    Some(SMSG_TRADE_STATUS_EXTENDED {
        self_player: describes_partner,
        trade_slot_count1: 7,
        trade_slot_count2: 7,
        money_in_trade: wow_world_messages::vanilla::Gold::new(gold),
        spell_on_lowest_slot: 0,
        trade_slots,
    })
}

pub(crate) fn item_gain_feedback(
    db: &RemoteTables,
    self_guid: u64,
    slot: u8,
    entry: u32,
    gained: u32,
    stack_add: bool,
    random_property_id: u32,
) -> Vec<Outbound> {
    if gained == 0 {
        return Vec::new();
    }
    let mut out = vec![Outbound::One(ServerOpcodeMessage::SMSG_ITEM_PUSH_RESULT(
        Box::new(codec::build_item_push_result(
            self_guid,
            codec::bag_content_parts(slot).map_or(255, |(bag, _)| bag),
            u32::from(codec::bag_content_parts(slot).map_or(slot, |(_, position)| position)),
            entry,
            gained,
            stack_add,
            random_property_id,
        )),
    ))];
    let wanted = db.game_character_quest().iter().any(|q| {
        q.character_guid == self_guid
            && !q.rewarded
            && !q.failed
            && db
                .game_quest_objective()
                .iter()
                .any(|o| o.quest_entry == q.quest_entry && o.kind == 1 && o.target_entry == entry)
    });
    if wanted {
        use wow_world_messages::vanilla::SMSG_QUESTUPDATE_ADD_ITEM;
        out.push(Outbound::One(
            ServerOpcodeMessage::SMSG_QUESTUPDATE_ADD_ITEM(SMSG_QUESTUPDATE_ADD_ITEM {
                required_item_id: entry,
                items_required: gained,
            }),
        ));
    }
    out
}

pub(crate) fn item_instance_insert_outbound(
    db: &RemoteTables,
    self_guid: u64,
    row: &ItemInstance,
    enchantment: u32,
) -> Vec<Outbound> {
    let mut out = Vec::new();
    let (max_durability, container_slots) = db
        .game_item_template()
        .entry()
        .find(&row.entry)
        .map(|t| (t.max_durability, t.container_slots))
        .unwrap_or((row.durability, 0));
    let view = codec::ItemInstanceView {
        max_durability,
        container_slots,
        random_property_enchant_ids: crate::stdb::reads::property_enchant_ids(
            db,
            row.random_property_id,
        ),
        enchantment,
        ..view_of_item_row(row)
    };
    out.push(Outbound::One(ServerOpcodeMessage::SMSG_UPDATE_OBJECT(
        Box::new(codec::build_item_create_object(&view)),
    )));
    if let Some((opcode, body)) = codec::build_random_property_values(&view) {
        out.push(Outbound::Raw { opcode, body });
    }
    if let Some(values) = codec::build_inv_slot_values(self_guid, row.slot, row.guid) {
        out.push(Outbound::One(ServerOpcodeMessage::SMSG_UPDATE_OBJECT(
            Box::new(values),
        )));
    }
    if let Some(values) = codec::build_visible_item_values(self_guid, row.slot, &view) {
        out.push(Outbound::One(ServerOpcodeMessage::SMSG_UPDATE_OBJECT(
            Box::new(values),
        )));
    }
    if let Some((bag_slot, slot_in_bag)) = codec::bag_content_parts(row.slot) {
        if let Some(bag) = db
            .game_item_instance()
            .iter()
            .find(|item| item.owner_guid == self_guid && item.slot == bag_slot)
        {
            let (opcode, body) =
                codec::build_container_slot_values(bag.guid, slot_in_bag, row.guid);
            out.push(Outbound::Raw { opcode, body });
        }
    }
    // Queue CREATE and inventory pointers together before gain or quest-completion feedback.
    out.extend(item_gain_feedback(
        db,
        self_guid,
        row.slot,
        row.entry,
        row.stack_count,
        false,
        row.random_property_id,
    ));
    if row.slot <= 18 {
        append_item_armor_and_sheet(db, self_guid, &mut out);
    }
    out
}

pub(crate) fn item_instance_delete_outbound(
    db: &RemoteTables,
    self_guid: u64,
    row: &ItemInstance,
) -> Vec<Outbound> {
    let mut out = Vec::new();
    if let Some(values) = codec::build_inv_slot_values(self_guid, row.slot, 0) {
        out.push(Outbound::One(ServerOpcodeMessage::SMSG_UPDATE_OBJECT(
            Box::new(values),
        )));
    }
    if let Some(values) =
        codec::build_visible_item_values(self_guid, row.slot, &codec::ItemInstanceView::default())
    {
        out.push(Outbound::One(ServerOpcodeMessage::SMSG_UPDATE_OBJECT(
            Box::new(values),
        )));
    }
    if let Some((bag_slot, slot_in_bag)) = codec::bag_content_parts(row.slot) {
        if let Some(bag) = db
            .game_item_instance()
            .iter()
            .find(|item| item.owner_guid == self_guid && item.slot == bag_slot)
        {
            let (opcode, body) = codec::build_container_slot_values(bag.guid, slot_in_bag, 0);
            out.push(Outbound::Raw { opcode, body });
        }
    }
    out.push(Outbound::One(ServerOpcodeMessage::SMSG_DESTROY_OBJECT(
        codec::build_destroy_object(row.guid),
    )));
    if row.slot <= 18 {
        append_item_armor_and_sheet(db, self_guid, &mut out);
    }
    out
}

pub(crate) fn item_instance_update_outbound(
    db: &RemoteTables,
    self_guid: u64,
    old: &ItemInstance,
    row: &ItemInstance,
) -> Vec<Outbound> {
    let old_slot_is_empty = old.slot != row.slot
        && !db
            .game_item_instance()
            .iter()
            .any(|item| item.owner_guid == self_guid && item.slot == old.slot);
    let mut out = if row.slot == old.slot && row.stack_count > old.stack_count {
        item_gain_feedback(
            db,
            self_guid,
            row.slot,
            row.entry,
            row.stack_count - old.stack_count,
            true,
            row.random_property_id,
        )
    } else {
        Vec::new()
    };
    if old.slot != row.slot {
        append_final_item_slots(self_guid, old.slot, row.slot, &mut out, |slot| {
            db.game_item_instance()
                .iter()
                .find(|item| item.owner_guid == self_guid && item.slot == slot)
                .map(|item| view_of_item_row(&item))
        });
    }
    out.extend(enchant_change_outbound(self_guid, old, row));
    if old.stack_count != row.stack_count || old.durability != row.durability {
        out.push(Outbound::One(ServerOpcodeMessage::SMSG_UPDATE_OBJECT(
            Box::new(codec::build_item_values(
                row.guid,
                row.stack_count,
                row.durability,
            )),
        )));
    }
    let moved_into_equipment = old.slot != row.slot && row.slot <= 18;
    let emptied_equipment = old.slot != row.slot && old.slot <= 18 && old_slot_is_empty;
    if moved_into_equipment
        || emptied_equipment
        || row.slot <= 18
            && ((old.durability == 0) != (row.durability == 0) || old.enchant_id != row.enchant_id)
    {
        append_item_armor_and_sheet(db, self_guid, &mut out);
    }
    out
}

pub(crate) fn enchant_change_outbound(
    self_guid: u64,
    old: &ItemInstance,
    row: &ItemInstance,
) -> Vec<Outbound> {
    if old.enchant_id == row.enchant_id || row.entry == lyracore_shared::guild::GUILD_CHARTER_ENTRY
    {
        return Vec::new();
    }
    let view = view_of_item_row(row);
    let mut out = vec![Outbound::One(ServerOpcodeMessage::SMSG_UPDATE_OBJECT(
        Box::new(codec::build_item_enchantment_values(
            row.guid,
            view.enchantment,
        )),
    ))];
    if old.slot == row.slot {
        if let Some(values) = codec::build_visible_item_values(self_guid, row.slot, &view) {
            out.push(Outbound::One(ServerOpcodeMessage::SMSG_UPDATE_OBJECT(
                Box::new(values),
            )));
        }
    }
    out
}

pub(crate) fn append_final_item_slots(
    self_guid: u64,
    old_slot: u8,
    new_slot: u8,
    out: &mut Vec<Outbound>,
    item_in_slot: impl Fn(u8) -> Option<codec::ItemInstanceView>,
) {
    let old_item = item_in_slot(old_slot);
    let new_item = item_in_slot(new_slot).unwrap_or_default();
    let slots = old_item
        .is_none()
        .then_some((old_slot, codec::ItemInstanceView::default()))
        .into_iter()
        .chain([(new_slot, new_item)]);
    for (slot, item) in slots.clone() {
        if let Some(values) = codec::build_inv_slot_values(self_guid, slot, item.guid) {
            out.push(Outbound::One(ServerOpcodeMessage::SMSG_UPDATE_OBJECT(
                Box::new(values),
            )));
        }
    }
    for (slot, item) in slots.clone() {
        if let Some(values) = codec::build_visible_item_values(self_guid, slot, &item) {
            out.push(Outbound::One(ServerOpcodeMessage::SMSG_UPDATE_OBJECT(
                Box::new(values),
            )));
        }
    }
    for (slot, item) in slots {
        if let Some((bag_slot, slot_in_bag)) = codec::bag_content_parts(slot) {
            if let Some(bag) = item_in_slot(bag_slot) {
                let (opcode, body) =
                    codec::build_container_slot_values(bag.guid, slot_in_bag, item.guid);
                out.push(Outbound::Raw { opcode, body });
            }
        }
    }
}

pub(crate) fn append_item_armor_and_sheet(
    db: &RemoteTables,
    self_guid: u64,
    out: &mut Vec<Outbound>,
) {
    let resistances = crate::stdb::armor::effective_resistances(db, self_guid);
    out.push(Outbound::One(ServerOpcodeMessage::SMSG_UPDATE_OBJECT(
        Box::new(codec::build_resistance_values(self_guid, resistances)),
    )));
    if let Some(stats) = crate::stdb::armor::sheet_stats(db, self_guid) {
        out.push(Outbound::One(ServerOpcodeMessage::SMSG_UPDATE_OBJECT(
            Box::new(codec::build_sheet_stats_values(self_guid, &stats)),
        )));
    }
}

pub(crate) fn item_owner_job(
    view: &WorldView,
    shard: ShardId,
    owner_guid: u64,
    job: impl FnOnce(Arc<Viewer>) -> Vec<Outbound> + Send + 'static,
) {
    let Some(viewer) = view.viewer_of_owner_on_shard(shard, OwnerGuid(owner_guid)) else {
        return;
    };
    enqueue(viewer, job);
}

pub(crate) fn item_inserted(
    view: &WorldView,
    coord: &Coordinator,
    shard: ShardId,
    row: &ItemInstance,
) {
    item_visible_changed(view, shard, None, Some(row), item_slot_read(coord));
    let (coord, row) = (coord.clone(), row.clone());
    item_owner_job(view, shard, row.owner_guid, move |viewer| {
        let enchantment = item_enchantment_word(&coord, &row);
        let guard = coord.0.coord();
        crate::codec::item::subscriptions::item_instance_insert_outbound(
            &guard.conn.db,
            viewer.self_guid,
            &row,
            enchantment,
        )
    });
}

/// Read the Guild Charter's Petition before taking the Home Shard cache lock.
pub(crate) fn item_enchantment_word(coord: &Coordinator, row: &ItemInstance) -> u32 {
    if row.entry != lyracore_shared::guild::GUILD_CHARTER_ENTRY {
        return client_enchantment_id(row.enchant_id);
    }
    coord
        .realm_core()
        .ok()
        .and_then(|realm| realm.charter_petition_id(row.guid))
        .unwrap_or(0)
}

pub(crate) fn item_updated(
    view: &WorldView,
    coord: &Coordinator,
    shard: ShardId,
    old: &ItemInstance,
    row: &ItemInstance,
) {
    item_visible_changed(view, shard, Some(old), Some(row), item_slot_read(coord));
    let (coord, old, row) = (coord.clone(), old.clone(), row.clone());
    item_owner_job(view, shard, row.owner_guid, move |viewer| {
        let guard = coord.0.coord();
        crate::codec::item::subscriptions::item_instance_update_outbound(
            &guard.conn.db,
            viewer.self_guid,
            &old,
            &row,
        )
    });
}

pub(crate) fn item_deleted(
    view: &WorldView,
    coord: &Coordinator,
    shard: ShardId,
    row: &ItemInstance,
) {
    item_visible_changed(view, shard, Some(row), None, item_slot_read(coord));
    let (coord, row) = (coord.clone(), row.clone());
    item_owner_job(view, shard, row.owner_guid, move |viewer| {
        let guard = coord.0.coord();
        crate::codec::item::subscriptions::item_instance_delete_outbound(
            &guard.conn.db,
            viewer.self_guid,
            &row,
        )
    });
}

pub(crate) type ItemSlotRead =
    Arc<dyn Fn(u64, u8) -> Option<crate::codec::ItemInstanceView> + Send + Sync>;

pub(crate) fn item_slot_read(coord: &Coordinator) -> ItemSlotRead {
    let coord = coord.clone();
    Arc::new(move |owner_guid, slot| {
        coord
            .0
            .coord()
            .conn
            .db
            .game_item_instance()
            .iter()
            .find(|item| item.owner_guid == owner_guid && item.slot == slot)
            .map(|item| crate::stdb::reads::view_of_item_row(&item))
    })
}

pub(crate) fn item_visible_changed(
    view: &WorldView,
    shard: ShardId,
    old: Option<&ItemInstance>,
    new: Option<&ItemInstance>,
    item_in_slot: ItemSlotRead,
) {
    let visible = |row: &ItemInstance| {
        (
            row.owner_guid,
            row.slot,
            row.entry,
            row.random_property_id,
            row.enchant_id,
        )
    };
    if old.map(visible) == new.map(visible) {
        return;
    }
    let mut slots = Vec::with_capacity(2);
    for row in old.into_iter().chain(new) {
        let slot = (row.owner_guid, row.slot);
        if row.slot <= 18 && !slots.contains(&slot) {
            slots.push(slot);
        }
    }
    for (owner_guid, slot) in slots {
        let item = Arc::new(OnceLock::new());
        let key = view
            .spatial
            .entity_cell_on_shard(EntityLayer::WorldEntity, owner_guid, shard);
        for viewer in view.cell_audience(shard, key, BOX_HALF_SPAN, &[]) {
            if viewer.self_guid == owner_guid {
                continue;
            }
            let item_in_slot = item_in_slot.clone();
            let item = item.clone();
            enqueue(viewer, move |viewer| {
                if !viewer.created.lock().unwrap().contains(&owner_guid) {
                    return Vec::new();
                }
                let item = item.get_or_init(|| item_in_slot(owner_guid, slot).unwrap_or_default());
                crate::codec::build_visible_item_values(owner_guid, slot, item)
                    .map(|values| {
                        vec![Outbound::One(ServerOpcodeMessage::SMSG_UPDATE_OBJECT(
                            Box::new(values),
                        ))]
                    })
                    .unwrap_or_default()
            });
        }
    }
}

pub(crate) fn trade_event_appeared(view: &WorldView, shard: ShardId, row: &TradeEvent) {
    let Some(viewer) = view.viewer_of_owner_on_shard(shard, OwnerGuid(row.recipient_guid)) else {
        return;
    };
    if !crate::codec::social::subscriptions::private_recipient_audience(
        row.recipient_guid,
        viewer.self_guid,
    ) {
        return;
    }
    let row = row.clone();
    enqueue(viewer.clone(), move |_| {
        crate::codec::item::subscriptions::trade_event_outbound(&row)
    });
}

crate::stdb::relays::declare_relays! {
    game_item_instance [false] {
        insert(Live) => |context, _event, row| item_inserted(&context.view, &context.coord, context.shard, row);
        update(Snapshot) => |context, _event, old, row| item_updated(&context.view, &context.coord, context.shard, old, row);
        delete(Snapshot) => |context, _event, row| item_deleted(&context.view, &context.coord, context.shard, row);
    }
    game_trade_event [false] {
        insert(Live) => |context, _event, row| trade_event_appeared(&context.view, context.shard, row);
    }
}

#[cfg(test)]
mod packet_tests;

#[cfg(test)]
mod audience_tests;
