use super::super::bindings::*;
use super::super::connection::LiveConn;

pub(in crate::stdb) fn quest_objectives_complete(
    live: &LiveConn,
    quest_entry: u32,
    logrow: Option<&CharacterQuest>,
    owner_guid: u64,
) -> bool {
    let (objectives, requires_event_credit, has_event_credit, inventory) = live
        .with_cache_snapshot(|| {
            let quests = live.quests.read().unwrap();
            (
                quests.objectives(&live.conn.db, quest_entry),
                quests.requires_event_credit(quest_entry),
                quests.has_event_credit(owner_guid, quest_entry),
                live.inventory
                    .read()
                    .unwrap()
                    .items(&live.conn.db, owner_guid),
            )
        });
    lyracore_shared::quest::objectives_complete(
        objectives
            .into_iter()
            .map(|o| lyracore_shared::quest::Objective {
                kind: o.kind,
                index: o.obj_index,
                target_entry: o.target_entry,
                required_count: o.required_count,
            }),
        logrow.map_or(&[], |q| q.counts.as_slice()),
        |entry| {
            lyracore_shared::item::carried_quantity(
                entry,
                inventory
                    .iter()
                    .map(|item| (item.slot, item.entry, item.stack_count)),
            )
        },
        requires_event_credit,
        has_event_credit,
    )
}

pub(crate) fn build_quest_log_slots(
    live: &LiveConn,
    player_guid: u64,
) -> Vec<crate::codec::update_mask::QuestLogSlot> {
    let mut active: Vec<CharacterQuest> = live
        .quest_rows(player_guid)
        .into_iter()
        .filter(|q| !q.rewarded)
        .collect();
    active.sort_by_key(|q| q.quest_entry);
    active
        .into_iter()
        .take(crate::codec::update_mask::idx::QUEST_LOG_SLOTS as usize)
        .enumerate()
        .map(|(i, q)| {
            let complete = quest_objectives_complete(live, q.quest_entry, Some(&q), player_guid);
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
