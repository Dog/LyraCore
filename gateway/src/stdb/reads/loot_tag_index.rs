use std::collections::{BTreeSet, HashMap};
use std::sync::{Arc, RwLock};

use spacetimedb_sdk::{Table, TableWithPrimaryKey};

use super::super::bindings::*;

#[derive(Default)]
pub(crate) struct LootTagIndex {
    tap_members: HashMap<(u64, u64), BTreeSet<u64>>,
    corpse_eligible: HashMap<(u64, u64), BTreeSet<u64>>,
}

impl LootTagIndex {
    pub(crate) fn is_tap_member(&self, creature_guid: u64, character_guid: u64) -> bool {
        self.tap_members
            .contains_key(&(creature_guid, character_guid))
    }

    pub(crate) fn is_corpse_eligible(&self, corpse_guid: u64, character_guid: u64) -> bool {
        self.corpse_eligible
            .contains_key(&(corpse_guid, character_guid))
    }

    fn insert_tap_members(&mut self, row: &CreatureQuestTapMember) {
        self.tap_members
            .entry((row.creature_guid, row.character_guid))
            .or_default()
            .insert(row.id);
    }

    fn remove_tap_members(&mut self, row: &CreatureQuestTapMember) {
        if let Some(ids) = self
            .tap_members
            .get_mut(&(row.creature_guid, row.character_guid))
        {
            ids.remove(&row.id);
            if ids.is_empty() {
                self.tap_members
                    .remove(&(row.creature_guid, row.character_guid));
            }
        }
    }

    fn insert_corpse_eligible(&mut self, row: &CorpseLootEligible) {
        self.corpse_eligible
            .entry((row.corpse_guid, row.eligible_guid))
            .or_default()
            .insert(row.id);
    }

    fn remove_corpse_eligible(&mut self, row: &CorpseLootEligible) {
        if let Some(ids) = self
            .corpse_eligible
            .get_mut(&(row.corpse_guid, row.eligible_guid))
        {
            ids.remove(&row.id);
            if ids.is_empty() {
                self.corpse_eligible
                    .remove(&(row.corpse_guid, row.eligible_guid));
            }
        }
    }
}

pub(crate) fn watch_loot_tag(conn: &DbConnection) -> Arc<RwLock<LootTagIndex>> {
    let index = Arc::new(RwLock::new(LootTagIndex::default()));
    let inserted = index.clone();
    conn.db
        .game_creature_quest_tap_member()
        .on_insert(move |_ctx, row| {
            inserted.write().unwrap().insert_tap_members(row);
        });
    let updated = index.clone();
    conn.db
        .game_creature_quest_tap_member()
        .on_update(move |_ctx, old, new| {
            let mut index = updated.write().unwrap();
            index.remove_tap_members(old);
            index.insert_tap_members(new);
        });
    let deleted = index.clone();
    conn.db
        .game_creature_quest_tap_member()
        .on_delete(move |_ctx, row| {
            deleted.write().unwrap().remove_tap_members(row);
        });
    let inserted = index.clone();
    conn.db
        .game_corpse_loot_eligible()
        .on_insert(move |_ctx, row| {
            inserted.write().unwrap().insert_corpse_eligible(row);
        });
    let updated = index.clone();
    conn.db
        .game_corpse_loot_eligible()
        .on_update(move |_ctx, old, new| {
            let mut index = updated.write().unwrap();
            index.remove_corpse_eligible(old);
            index.insert_corpse_eligible(new);
        });
    let deleted = index.clone();
    conn.db
        .game_corpse_loot_eligible()
        .on_delete(move |_ctx, row| {
            deleted.write().unwrap().remove_corpse_eligible(row);
        });
    index
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tag_membership_tracks_the_creature_and_character_on_update_and_delete() {
        let mut index = LootTagIndex::default();
        let old = CreatureQuestTapMember {
            id: 1,
            creature_guid: 99,
            character_guid: 7,
        };
        index.insert_tap_members(&old);
        assert!(index.is_tap_member(99, 7));
        assert!(!index.is_tap_member(99, 8));
        assert!(!index.is_tap_member(100, 7));
        let moved = CreatureQuestTapMember {
            id: 1,
            creature_guid: 100,
            character_guid: 8,
        };
        index.remove_tap_members(&old);
        index.insert_tap_members(&moved);
        assert!(!index.is_tap_member(99, 7));
        assert!(index.is_tap_member(100, 8));
        index.remove_tap_members(&moved);
        assert!(!index.is_tap_member(100, 8));
    }

    #[test]
    fn corpse_eligibility_survives_a_duplicate_row_until_both_rows_leave() {
        let mut index = LootTagIndex::default();
        let first = CorpseLootEligible {
            id: 1,
            corpse_guid: 99,
            eligible_guid: 7,
        };
        let second = CorpseLootEligible {
            id: 2,
            ..first.clone()
        };
        index.insert_corpse_eligible(&first);
        index.insert_corpse_eligible(&second);
        index.remove_corpse_eligible(&first);
        assert!(index.is_corpse_eligible(99, 7));
        assert!(!index.is_corpse_eligible(99, 8));
        assert!(!index.is_corpse_eligible(100, 7));
        index.remove_corpse_eligible(&second);
        assert!(!index.is_corpse_eligible(99, 7));
    }

    #[test]
    fn corpse_eligibility_moves_to_the_updated_creature_and_character() {
        let mut index = LootTagIndex::default();
        let old = CorpseLootEligible {
            id: 1,
            corpse_guid: 99,
            eligible_guid: 7,
        };
        index.insert_corpse_eligible(&old);
        let moved = CorpseLootEligible {
            id: 1,
            corpse_guid: 100,
            eligible_guid: 8,
        };
        index.remove_corpse_eligible(&old);
        index.insert_corpse_eligible(&moved);
        assert!(!index.is_corpse_eligible(99, 7));
        assert!(index.is_corpse_eligible(100, 8));
    }
}
