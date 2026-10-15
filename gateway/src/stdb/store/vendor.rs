use anyhow::Result;
use spacetimedb_sdk::Table;

use crate::codec;
use crate::stdb::bindings::*;
use crate::stdb::connection::call_reducer;
use crate::stdb::reads::property_enchant_ids;
use crate::world::{Actor, VendorActionStore};

impl VendorActionStore for crate::stdb::Coordinator {
    fn vendor_stock(&self, vendor_guid: u64) -> Result<Vec<codec::VendorItemView>> {
        let guard = self.0.coord();
        let db = &guard.conn.db;
        let Some(creature_entry) = db
            .game_world_entity()
            .guid()
            .find(&vendor_guid)
            .map(|e| e.entry)
        else {
            return Ok(Vec::new());
        };
        let mut rows: Vec<_> = db
            .game_npc_vendor()
            .iter()
            .filter(|v| v.creature_entry == creature_entry)
            .collect();
        rows.sort_by_key(|v| v.slot);
        let items = rows
            .into_iter()
            .filter_map(|v| {
                db.game_item_template()
                    .entry()
                    .find(&v.item_entry)
                    .map(|t| codec::VendorItemView {
                        item_entry: v.item_entry,
                        display_id: t.display_id,
                        buy_price: t.buy_price,
                        max_durability: t.max_durability,
                        max_count: v.max_count,
                        buy_count: t.buy_count,
                    })
            })
            .collect();
        Ok(items)
    }

    fn vendor_refuses_interaction(&self, vendor_guid: u64, actor: Actor) -> Result<bool> {
        self.npc_refuses_interaction(vendor_guid, actor.guid())
    }

    fn vendor_buy(
        &self,
        actor: Actor,
        vendor_guid: u64,
        item_entry: u32,
        count: u32,
    ) -> Result<()> {
        call_reducer!(
            self.0.call_pipe().conn.reducers,
            "gw_buy_item",
            gw_buy_item_then(self.session_actor(actor), vendor_guid, item_entry, count)
        )
    }

    fn buyback_slots(&self, player_guid: u64) -> Vec<(u32, u32, u32, u32)> {
        let guard = self.0.coord();
        let db = &guard.conn.db;
        let mut rows: Vec<_> = db
            .game_character_buyback()
            .iter()
            .filter(|b| b.player_guid == player_guid)
            .collect();
        rows.sort_by(|a, b| b.id.cmp(&a.id));
        rows.into_iter()
            .map(|b| (b.item_entry, b.stack_count, b.price, b.random_property_id))
            .collect()
    }

    fn random_property_enchant_ids(&self, random_property_id: u32) -> [u32; 3] {
        let guard = self.0.coord();
        property_enchant_ids(&guard.conn.db, random_property_id)
    }

    fn vendor_item_slot(&self, owner_guid: u64, item_guid: u64) -> Option<u8> {
        crate::stdb::Coordinator::item_slot_by_guid(self, owner_guid, item_guid)
    }

    fn vendor_repair(&self, actor: Actor, npc_guid: u64, slot: u8) -> Result<()> {
        call_reducer!(
            self.0.call_pipe().conn.reducers,
            "gw_repair_item",
            gw_repair_item_then(self.session_actor(actor), npc_guid, slot)
        )
    }

    fn vendor_sell(&self, actor: Actor, vendor_guid: u64, slot: u8) -> Result<()> {
        call_reducer!(
            self.0.call_pipe().conn.reducers,
            "gw_sell_item",
            gw_sell_item_then(self.session_actor(actor), vendor_guid, slot)
        )
    }

    fn vendor_buyback(&self, actor: Actor, vendor_guid: u64, slot: u8) -> Result<()> {
        call_reducer!(
            self.0.call_pipe().conn.reducers,
            "gw_buyback_item",
            gw_buyback_item_then(self.session_actor(actor), vendor_guid, slot)
        )
    }
}
