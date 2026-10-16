use std::collections::{BTreeSet, HashMap};
use std::sync::{Arc, RwLock};

use spacetimedb_sdk::{Table, TableWithPrimaryKey};

use super::super::bindings::*;

#[derive(Default)]
pub(crate) struct CombatIndex {
    auras: HashMap<u64, BTreeSet<u64>>,
    enchantments: HashMap<u32, BTreeSet<u64>>,
}

impl CombatIndex {
    fn insert_auras(&mut self, row: &Aura) {
        self.auras
            .entry(row.target_guid)
            .or_default()
            .insert(row.id);
    }

    fn remove_auras(&mut self, row: &Aura) {
        if let Some(ids) = self.auras.get_mut(&row.target_guid) {
            ids.remove(&row.id);
            if ids.is_empty() {
                self.auras.remove(&row.target_guid);
            }
        }
    }

    pub(crate) fn aura_ids(&self, target_guid: u64) -> Vec<u64> {
        self.auras
            .get(&target_guid)
            .into_iter()
            .flatten()
            .copied()
            .collect()
    }

    pub(crate) fn auras(&self, db: &RemoteTables, target_guid: u64) -> Vec<Aura> {
        self.aura_ids(target_guid)
            .into_iter()
            .filter_map(|id| db.game_aura().id().find(&id))
            .filter(|row| row.target_guid == target_guid)
            .collect()
    }

    fn insert_enchantments(&mut self, row: &ItemEnchantment) {
        self.enchantments
            .entry(row.enchant_id)
            .or_default()
            .insert(row.id);
    }

    fn remove_enchantments(&mut self, row: &ItemEnchantment) {
        if let Some(ids) = self.enchantments.get_mut(&row.enchant_id) {
            ids.remove(&row.id);
            if ids.is_empty() {
                self.enchantments.remove(&row.enchant_id);
            }
        }
    }

    pub(crate) fn enchantment_ids(&self, enchant_id: u32) -> Vec<u64> {
        self.enchantments
            .get(&enchant_id)
            .into_iter()
            .flatten()
            .copied()
            .collect()
    }

    pub(crate) fn enchantments(&self, db: &RemoteTables, enchant_id: u32) -> Vec<ItemEnchantment> {
        self.enchantment_ids(enchant_id)
            .into_iter()
            .filter_map(|id| db.game_item_enchantment().id().find(&id))
            .filter(|row| row.enchant_id == enchant_id)
            .collect()
    }
}

pub(crate) fn watch_combat(conn: &DbConnection) -> Arc<RwLock<CombatIndex>> {
    let index = Arc::new(RwLock::new(CombatIndex::default()));
    let inserted = index.clone();
    conn.db.game_aura().on_insert(move |_ctx, row| {
        inserted.write().unwrap().insert_auras(row);
    });
    let updated = index.clone();
    conn.db.game_aura().on_update(move |_ctx, old, new| {
        let mut index = updated.write().unwrap();
        index.remove_auras(old);
        index.insert_auras(new);
    });
    let deleted = index.clone();
    conn.db.game_aura().on_delete(move |_ctx, row| {
        deleted.write().unwrap().remove_auras(row);
    });
    let inserted = index.clone();
    conn.db.game_item_enchantment().on_insert(move |_ctx, row| {
        inserted.write().unwrap().insert_enchantments(row);
    });
    let updated = index.clone();
    conn.db
        .game_item_enchantment()
        .on_update(move |_ctx, old, new| {
            let mut index = updated.write().unwrap();
            index.remove_enchantments(old);
            index.insert_enchantments(new);
        });
    let deleted = index.clone();
    conn.db.game_item_enchantment().on_delete(move |_ctx, row| {
        deleted.write().unwrap().remove_enchantments(row);
    });
    index
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::combat::subscriptions::tests::aura;

    #[test]
    fn aura_retargeting_and_removal_leave_each_target_its_current_rows() {
        let mut index = CombatIndex::default();
        let old = aura(7, 0, 0, 0, 0, 0);
        index.insert_auras(&old);
        let mut other = aura(8, 0, 0, 0, 0, 0);
        other.id = 2;
        index.insert_auras(&other);
        assert_eq!(index.aura_ids(7), [1]);
        let mut moved = old.clone();
        moved.target_guid = 8;
        index.remove_auras(&old);
        index.insert_auras(&moved);
        assert!(index.aura_ids(7).is_empty());
        assert_eq!(index.aura_ids(8), [1, 2]);
        index.remove_auras(&moved);
        assert_eq!(index.aura_ids(8), [2]);
    }

    #[test]
    fn enchantment_effects_follow_their_enchantment_on_update_and_delete() {
        let mut index = CombatIndex::default();
        let old = ItemEnchantment {
            id: 1,
            enchant_id: 10,
            effect_index: 0,
            kind: 8,
            amount: 3,
            spell_id: 0,
            school_mask: 2,
        };
        index.insert_enchantments(&old);
        index.insert_enchantments(&ItemEnchantment {
            id: 2,
            ..old.clone()
        });
        let moved = ItemEnchantment {
            enchant_id: 20,
            ..old.clone()
        };
        index.remove_enchantments(&old);
        index.insert_enchantments(&moved);
        assert_eq!(index.enchantment_ids(10), [2]);
        assert_eq!(index.enchantment_ids(20), [1]);
        index.remove_enchantments(&moved);
        assert!(index.enchantment_ids(20).is_empty());
    }
}
