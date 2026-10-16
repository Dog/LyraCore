use anyhow::Result;

use super::super::bindings::*;
use super::super::connection::Coordinator;
use lyracore_shared::item_property::client_enchantment_id;

impl Coordinator {
    pub fn player_skills(&self, character_guid: u64) -> Result<Vec<(u32, u16, u16)>> {
        let guard = self.0.coord();
        Ok(guard
            .skill_rows(character_guid)
            .into_iter()
            .map(|s| (s.skill_line, s.current, s.max_rank))
            .collect())
    }

    pub fn player_items(&self, owner_guid: u64) -> Result<Vec<crate::codec::ItemInstanceView>> {
        let mut items = self.player_item_rows(owner_guid);
        self.project_charter_petitions(&mut items);
        Ok(items)
    }

    fn player_item_rows(&self, owner_guid: u64) -> Vec<crate::codec::ItemInstanceView> {
        let guard = self.0.coord();
        let db = &guard.conn.db;
        guard
            .item_rows(owner_guid)
            .into_iter()
            .filter_map(|i| {
                let tmpl = db.game_item_template().entry().find(&i.entry)?;
                Some(crate::codec::ItemInstanceView {
                    max_durability: tmpl.max_durability,
                    container_slots: tmpl.container_slots,
                    random_property_enchant_ids: property_enchant_ids(db, i.random_property_id),
                    ..view_of_item_row(&i)
                })
            })
            .collect()
    }

    /// Slot of `item_guid` when `owner_guid` holds it; another Character's item is `None`.
    pub fn item_slot_by_guid(&self, owner_guid: u64, item_guid: u64) -> Option<u8> {
        let guard = self.0.coord();
        guard
            .conn
            .db
            .game_item_instance()
            .guid()
            .find(&item_guid)
            .filter(|item| item.owner_guid == owner_guid)
            .map(|item| item.slot)
    }
}

pub(crate) fn view_of_item_row(row: &ItemInstance) -> crate::codec::ItemInstanceView {
    crate::codec::ItemInstanceView {
        guid: row.guid,
        entry: row.entry,
        owner_guid: row.owner_guid,
        slot: row.slot,
        stack_count: row.stack_count,
        durability: row.durability,
        random_property_id: row.random_property_id,
        item_text_id: row.item_text_id,
        enchantment: client_enchantment_id(row.enchant_id),
        ..Default::default()
    }
}

pub(crate) fn property_enchant_ids(db: &RemoteTables, random_property_id: u32) -> [u32; 3] {
    db.game_item_random_property()
        .property_id()
        .find(&random_property_id)
        .map_or([0; 3], |property| {
            [
                property.enchant_id_1,
                property.enchant_id_2,
                property.enchant_id_3,
            ]
        })
}
