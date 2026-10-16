use std::collections::{BTreeSet, HashMap};
use std::sync::{Arc, RwLock};

use spacetimedb_sdk::{Table, TableWithPrimaryKey};

use super::super::bindings::*;
use super::super::connection::LiveConn;

impl LiveConn {
    pub(crate) fn quest_rows(&self, character_guid: u64) -> Vec<CharacterQuest> {
        self.with_cache_snapshot(|| {
            self.quests
                .read()
                .unwrap()
                .quests(&self.conn.db, character_guid)
        })
    }

    pub(crate) fn quest_objectives(&self, quest_entry: u32) -> Vec<QuestObjective> {
        self.with_cache_snapshot(|| {
            self.quests
                .read()
                .unwrap()
                .objectives(&self.conn.db, quest_entry)
        })
    }
}

#[derive(Default)]
pub(crate) struct QuestIndex {
    quests: HashMap<u64, BTreeSet<u64>>,
    objectives: HashMap<u32, BTreeSet<u64>>,
    requirements: HashMap<u32, BTreeSet<u64>>,
    credits: HashMap<(u64, u32), BTreeSet<u64>>,
}

impl QuestIndex {
    pub(crate) fn requires_event_credit(&self, quest_entry: u32) -> bool {
        self.requirements.contains_key(&quest_entry)
    }

    pub(crate) fn has_event_credit(&self, character_guid: u64, quest_entry: u32) -> bool {
        self.credits.contains_key(&(character_guid, quest_entry))
    }

    fn insert_quests(&mut self, row: &CharacterQuest) {
        self.quests
            .entry(row.character_guid)
            .or_default()
            .insert(row.id);
    }

    fn remove_quests(&mut self, row: &CharacterQuest) {
        if let Some(ids) = self.quests.get_mut(&row.character_guid) {
            ids.remove(&row.id);
            if ids.is_empty() {
                self.quests.remove(&row.character_guid);
            }
        }
    }

    pub(crate) fn quest_row_ids(&self, character_guid: u64) -> Vec<u64> {
        self.quests
            .get(&character_guid)
            .into_iter()
            .flatten()
            .copied()
            .collect()
    }

    pub(crate) fn quests(&self, db: &RemoteTables, character_guid: u64) -> Vec<CharacterQuest> {
        self.quest_row_ids(character_guid)
            .into_iter()
            .filter_map(|id| db.game_character_quest().id().find(&id))
            .filter(|row| row.character_guid == character_guid)
            .collect()
    }

    fn insert_objectives(&mut self, row: &QuestObjective) {
        self.objectives
            .entry(row.quest_entry)
            .or_default()
            .insert(row.id);
    }

    fn remove_objectives(&mut self, row: &QuestObjective) {
        if let Some(ids) = self.objectives.get_mut(&row.quest_entry) {
            ids.remove(&row.id);
            if ids.is_empty() {
                self.objectives.remove(&row.quest_entry);
            }
        }
    }

    pub(crate) fn objective_ids(&self, quest_entry: u32) -> Vec<u64> {
        self.objectives
            .get(&quest_entry)
            .into_iter()
            .flatten()
            .copied()
            .collect()
    }

    pub(crate) fn objectives(&self, db: &RemoteTables, quest_entry: u32) -> Vec<QuestObjective> {
        self.objective_ids(quest_entry)
            .into_iter()
            .filter_map(|id| db.game_quest_objective().id().find(&id))
            .filter(|row| row.quest_entry == quest_entry)
            .collect()
    }

    fn insert_requirements(&mut self, row: &QuestEventRequirement) {
        self.requirements
            .entry(row.quest_entry)
            .or_default()
            .insert(row.id);
    }

    fn remove_requirements(&mut self, row: &QuestEventRequirement) {
        if let Some(ids) = self.requirements.get_mut(&row.quest_entry) {
            ids.remove(&row.id);
            if ids.is_empty() {
                self.requirements.remove(&row.quest_entry);
            }
        }
    }

    fn insert_credits(&mut self, row: &CharacterQuestEventCredit) {
        self.credits
            .entry((row.character_guid, row.quest_entry))
            .or_default()
            .insert(row.id);
    }

    fn remove_credits(&mut self, row: &CharacterQuestEventCredit) {
        if let Some(ids) = self.credits.get_mut(&(row.character_guid, row.quest_entry)) {
            ids.remove(&row.id);
            if ids.is_empty() {
                self.credits.remove(&(row.character_guid, row.quest_entry));
            }
        }
    }
}

pub(crate) fn watch_quest(conn: &DbConnection) -> Arc<RwLock<QuestIndex>> {
    let index = Arc::new(RwLock::new(QuestIndex::default()));
    let inserted = index.clone();
    conn.db.game_character_quest().on_insert(move |_ctx, row| {
        inserted.write().unwrap().insert_quests(row);
    });
    let updated = index.clone();
    conn.db
        .game_character_quest()
        .on_update(move |_ctx, old, new| {
            let mut index = updated.write().unwrap();
            index.remove_quests(old);
            index.insert_quests(new);
        });
    let deleted = index.clone();
    conn.db.game_character_quest().on_delete(move |_ctx, row| {
        deleted.write().unwrap().remove_quests(row);
    });
    let inserted = index.clone();
    conn.db.game_quest_objective().on_insert(move |_ctx, row| {
        inserted.write().unwrap().insert_objectives(row);
    });
    let updated = index.clone();
    conn.db
        .game_quest_objective()
        .on_update(move |_ctx, old, new| {
            let mut index = updated.write().unwrap();
            index.remove_objectives(old);
            index.insert_objectives(new);
        });
    let deleted = index.clone();
    conn.db.game_quest_objective().on_delete(move |_ctx, row| {
        deleted.write().unwrap().remove_objectives(row);
    });
    let inserted = index.clone();
    conn.db
        .game_quest_event_requirement()
        .on_insert(move |_ctx, row| {
            inserted.write().unwrap().insert_requirements(row);
        });
    let updated = index.clone();
    conn.db
        .game_quest_event_requirement()
        .on_update(move |_ctx, old, new| {
            let mut index = updated.write().unwrap();
            index.remove_requirements(old);
            index.insert_requirements(new);
        });
    let deleted = index.clone();
    conn.db
        .game_quest_event_requirement()
        .on_delete(move |_ctx, row| {
            deleted.write().unwrap().remove_requirements(row);
        });
    let inserted = index.clone();
    conn.db
        .game_character_quest_event_credit()
        .on_insert(move |_ctx, row| {
            inserted.write().unwrap().insert_credits(row);
        });
    let updated = index.clone();
    conn.db
        .game_character_quest_event_credit()
        .on_update(move |_ctx, old, new| {
            let mut index = updated.write().unwrap();
            index.remove_credits(old);
            index.insert_credits(new);
        });
    let deleted = index.clone();
    conn.db
        .game_character_quest_event_credit()
        .on_delete(move |_ctx, row| {
            deleted.write().unwrap().remove_credits(row);
        });
    index
}

#[cfg(test)]
mod tests {
    use super::*;

    fn quest(id: u64, character_guid: u64) -> CharacterQuest {
        CharacterQuest {
            id,
            character_guid,
            owner_identity: spacetimedb_sdk::Identity::ZERO,
            quest_entry: 10,
            counts: vec![],
            rewarded: false,
            deadline_micros: 0,
            failed: false,
        }
    }

    fn objective(id: u64, quest_entry: u32) -> QuestObjective {
        QuestObjective {
            id,
            quest_entry,
            obj_index: 0,
            kind: 1,
            target_entry: 25,
            required_count: 3,
        }
    }

    #[test]
    fn quest_logs_and_objectives_follow_reassignment_and_deletion() {
        let mut index = QuestIndex::default();
        let old = quest(7, 1);
        index.insert_quests(&old);
        let moved = quest(7, 2);
        index.remove_quests(&old);
        index.insert_quests(&moved);
        assert!(index.quest_row_ids(1).is_empty());
        assert_eq!(index.quest_row_ids(2), [7]);
        index.remove_quests(&moved);
        assert!(index.quest_row_ids(2).is_empty());
        let old = objective(3, 10);
        index.insert_objectives(&old);
        index.insert_objectives(&objective(4, 10));
        let moved = objective(3, 20);
        index.remove_objectives(&old);
        index.insert_objectives(&moved);
        assert_eq!(index.objective_ids(10), [4]);
        assert_eq!(index.objective_ids(20), [3]);
        index.remove_objectives(&moved);
        assert!(index.objective_ids(20).is_empty());
    }

    #[test]
    fn deleting_one_requirement_or_credit_preserves_another_matching_row() {
        let mut index = QuestIndex::default();
        let requirement = QuestEventRequirement {
            id: 1,
            quest_entry: 10,
        };
        let second = QuestEventRequirement {
            id: 2,
            quest_entry: 10,
        };
        index.insert_requirements(&requirement);
        index.insert_requirements(&second);
        index.remove_requirements(&requirement);
        assert!(index.requires_event_credit(10));
        index.remove_requirements(&second);
        assert!(!index.requires_event_credit(10));
        let credit = CharacterQuestEventCredit {
            id: 1,
            character_guid: 7,
            quest_entry: 10,
        };
        let second = CharacterQuestEventCredit {
            id: 2,
            ..credit.clone()
        };
        index.insert_credits(&credit);
        index.insert_credits(&second);
        index.remove_credits(&credit);
        assert!(index.has_event_credit(7, 10));
        assert!(!index.has_event_credit(8, 10));
        assert!(!index.has_event_credit(7, 20));
        index.remove_credits(&second);
        assert!(!index.has_event_credit(7, 10));
    }

    #[test]
    fn event_requirement_and_credit_updates_drop_the_old_keys() {
        let mut index = QuestIndex::default();
        let requirement = QuestEventRequirement {
            id: 1,
            quest_entry: 10,
        };
        index.insert_requirements(&requirement);
        index.remove_requirements(&requirement);
        index.insert_requirements(&QuestEventRequirement {
            id: 1,
            quest_entry: 20,
        });
        assert!(!index.requires_event_credit(10));
        assert!(index.requires_event_credit(20));
        let credit = CharacterQuestEventCredit {
            id: 1,
            character_guid: 7,
            quest_entry: 10,
        };
        index.insert_credits(&credit);
        index.remove_credits(&credit);
        index.insert_credits(&CharacterQuestEventCredit {
            id: 1,
            character_guid: 8,
            quest_entry: 20,
        });
        assert!(!index.has_event_credit(7, 10));
        assert!(index.has_event_credit(8, 20));
    }
}
