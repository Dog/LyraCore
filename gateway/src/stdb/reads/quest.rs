use spacetimedb_sdk::Table;

use super::super::bindings::*;
use super::player_item_count;

pub(in crate::stdb) fn quest_objectives_complete(
    db: &RemoteTables,
    quest_entry: u32,
    logrow: Option<&CharacterQuest>,
    owner_guid: u64,
) -> bool {
    let requires_event_credit = db
        .game_quest_event_requirement()
        .iter()
        .any(|requirement| requirement.quest_entry == quest_entry);
    let has_event_credit = db
        .game_character_quest_event_credit()
        .iter()
        .any(|credit| credit.character_guid == owner_guid && credit.quest_entry == quest_entry);
    lyracore_shared::quest::objectives_complete(
        db.game_quest_objective()
            .iter()
            .filter(|o| o.quest_entry == quest_entry)
            .map(|o| lyracore_shared::quest::Objective {
                kind: o.kind,
                index: o.obj_index,
                target_entry: o.target_entry,
                required_count: o.required_count,
            }),
        logrow.map_or(&[], |q| q.counts.as_slice()),
        |entry| player_item_count(db, owner_guid, entry),
        requires_event_credit,
        has_event_credit,
    )
}

pub(crate) fn build_quest_log_slots(
    db: &RemoteTables,
    quests: &[CharacterQuest],
    player_guid: u64,
) -> Vec<crate::codec::update_mask::QuestLogSlot> {
    let mut active: Vec<&CharacterQuest> = quests
        .iter()
        .filter(|q| q.character_guid == player_guid && !q.rewarded)
        .collect();
    active.sort_by_key(|q| q.quest_entry);
    active
        .into_iter()
        .take(crate::codec::update_mask::idx::QUEST_LOG_SLOTS as usize)
        .enumerate()
        .map(|(i, q)| {
            let complete = quest_objectives_complete(db, q.quest_entry, Some(q), player_guid);
            crate::codec::update_mask::QuestLogSlot {
                slot: i as u8,
                quest_id: q.quest_entry,
                counts: q.counts.clone(),
                state: u8::from(complete),
                timer: 0,
            }
        })
        .collect()
}
