use std::collections::{BTreeSet, HashMap};
use std::sync::{Arc, RwLock};

use spacetimedb_sdk::{Table, TableWithPrimaryKey};

use super::super::bindings::*;
use super::super::connection::LiveConn;

impl LiveConn {
    pub(crate) fn item_rows(&self, owner_guid: u64) -> Vec<ItemInstance> {
        self.with_cache_snapshot(|| {
            self.inventory
                .read()
                .unwrap()
                .items(&self.conn.db, owner_guid)
        })
    }

    pub(crate) fn skill_rows(&self, character_guid: u64) -> Vec<PlayerSkill> {
        self.with_cache_snapshot(|| {
            self.inventory
                .read()
                .unwrap()
                .skills(&self.conn.db, character_guid)
        })
    }
}

#[derive(Default)]
pub(crate) struct InventoryIndex {
    items: HashMap<u64, BTreeSet<u64>>,
    skills: HashMap<u64, BTreeSet<u64>>,
}

impl InventoryIndex {
    fn insert_items(&mut self, row: &ItemInstance) {
        self.items
            .entry(row.owner_guid)
            .or_default()
            .insert(row.guid);
    }

    fn remove_items(&mut self, row: &ItemInstance) {
        if let Some(ids) = self.items.get_mut(&row.owner_guid) {
            ids.remove(&row.guid);
            if ids.is_empty() {
                self.items.remove(&row.owner_guid);
            }
        }
    }

    pub(crate) fn item_guids(&self, character_guid: u64) -> Vec<u64> {
        self.items
            .get(&character_guid)
            .into_iter()
            .flatten()
            .copied()
            .collect()
    }

    pub(crate) fn items(&self, db: &RemoteTables, character_guid: u64) -> Vec<ItemInstance> {
        self.item_guids(character_guid)
            .into_iter()
            .filter_map(|id| db.game_item_instance().guid().find(&id))
            .filter(|row| row.owner_guid == character_guid)
            .collect()
    }

    fn insert_skills(&mut self, row: &PlayerSkill) {
        self.skills
            .entry(row.character_guid)
            .or_default()
            .insert(row.id);
    }

    fn remove_skills(&mut self, row: &PlayerSkill) {
        if let Some(ids) = self.skills.get_mut(&row.character_guid) {
            ids.remove(&row.id);
            if ids.is_empty() {
                self.skills.remove(&row.character_guid);
            }
        }
    }

    pub(crate) fn skill_ids(&self, character_guid: u64) -> Vec<u64> {
        self.skills
            .get(&character_guid)
            .into_iter()
            .flatten()
            .copied()
            .collect()
    }

    pub(crate) fn skills(&self, db: &RemoteTables, character_guid: u64) -> Vec<PlayerSkill> {
        self.skill_ids(character_guid)
            .into_iter()
            .filter_map(|id| db.game_player_skill().id().find(&id))
            .filter(|row| row.character_guid == character_guid)
            .collect()
    }
}

pub(crate) fn watch_inventory(conn: &DbConnection) -> Arc<RwLock<InventoryIndex>> {
    let index = Arc::new(RwLock::new(InventoryIndex::default()));
    let inserted = index.clone();
    conn.db.game_item_instance().on_insert(move |_ctx, row| {
        inserted.write().unwrap().insert_items(row);
    });
    let updated = index.clone();
    conn.db
        .game_item_instance()
        .on_update(move |_ctx, old, new| {
            let mut index = updated.write().unwrap();
            index.remove_items(old);
            index.insert_items(new);
        });
    let deleted = index.clone();
    conn.db.game_item_instance().on_delete(move |_ctx, row| {
        deleted.write().unwrap().remove_items(row);
    });
    let inserted = index.clone();
    conn.db.game_player_skill().on_insert(move |_ctx, row| {
        inserted.write().unwrap().insert_skills(row);
    });
    let updated = index.clone();
    conn.db
        .game_player_skill()
        .on_update(move |_ctx, old, new| {
            let mut index = updated.write().unwrap();
            index.remove_skills(old);
            index.insert_skills(new);
        });
    let deleted = index.clone();
    conn.db.game_player_skill().on_delete(move |_ctx, row| {
        deleted.write().unwrap().remove_skills(row);
    });
    index
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(guid: u64, owner_guid: u64) -> ItemInstance {
        ItemInstance {
            guid,
            owner_guid,
            entry: 25,
            slot: 23,
            stack_count: 1,
            durability: 0,
            owner_identity: spacetimedb_sdk::Identity::ZERO,
            created_at: spacetimedb_sdk::Timestamp::UNIX_EPOCH,
            enchant_id: 0,
            soulbound: false,
            random_property_id: 0,
            item_text_id: 0,
        }
    }

    fn skill(id: u64, character_guid: u64) -> PlayerSkill {
        PlayerSkill {
            id,
            character_guid,
            owner_identity: spacetimedb_sdk::Identity::ZERO,
            skill_line: 43,
            current: 1,
            max_rank: 5,
        }
    }

    #[test]
    fn an_item_transfer_removes_the_old_owner_and_deletion_removes_the_new_owner() {
        let mut index = InventoryIndex::default();
        let old = item(7, 1);
        index.insert_items(&old);
        index.insert_items(&item(8, 2));
        assert_eq!(index.item_guids(1), [7]);
        let moved = item(7, 2);
        index.remove_items(&old);
        index.insert_items(&moved);
        assert!(index.item_guids(1).is_empty());
        assert_eq!(index.item_guids(2), [7, 8]);
        index.remove_items(&moved);
        assert_eq!(index.item_guids(2), [8]);
    }

    #[test]
    fn skill_reassignment_and_deletion_preserve_other_characters() {
        let mut index = InventoryIndex::default();
        let old = skill(7, 1);
        index.insert_skills(&old);
        index.insert_skills(&skill(8, 2));
        assert_eq!(index.skill_ids(1), [7]);
        let moved = skill(7, 3);
        index.remove_skills(&old);
        index.insert_skills(&moved);
        assert!(index.skill_ids(1).is_empty());
        assert_eq!(index.skill_ids(3), [7]);
        index.remove_skills(&moved);
        assert!(index.skill_ids(3).is_empty());
        assert_eq!(index.skill_ids(2), [8]);
    }

    #[test]
    fn a_fresh_connection_rebuilds_only_its_snapshot() {
        let mut old = InventoryIndex::default();
        old.insert_items(&item(7, 1));
        let mut fresh = InventoryIndex::default();
        fresh.insert_items(&item(8, 1));
        old.remove_items(&item(7, 1));
        assert_eq!(fresh.item_guids(1), [8]);
        assert!(old.item_guids(1).is_empty());
    }
}
