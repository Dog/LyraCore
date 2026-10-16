use crate::codec::{self};
use crate::stdb::bindings::*;
use crate::stdb::world_index::ShardId;
use crate::stdb::world_view::*;
use crate::stdb::Coordinator;
use crate::stdb::LiveConn;
use crate::world::Outbound;
use wow_world_messages::vanilla::opcodes::ServerOpcodeMessage;

pub(crate) fn quest_log_outbound(live: &LiveConn, self_guid: u64) -> Option<Outbound> {
    if !crate::config::quest_log_fields_enabled() {
        return None;
    }
    let slots = crate::stdb::reads::build_quest_log_slots(live, self_guid);
    let mask = codec::update_mask::full_quest_log_mask(&slots);
    let (opcode, body) = codec::build_values_update_raw(self_guid, &mask);
    Some(Outbound::Raw { opcode, body })
}

pub(crate) fn quest_update_outbound(
    live: &LiveConn,
    self_guid: u64,
    old: &CharacterQuest,
    row: &CharacterQuest,
) -> Vec<Outbound> {
    let objectives: Vec<_> = live
        .quest_objectives(row.quest_entry)
        .into_iter()
        .map(|objective| {
            (
                objective.kind,
                objective.obj_index,
                objective.target_entry,
                objective.required_count,
            )
        })
        .collect();
    quest_update_packets(old, row, &objectives, quest_log_outbound(live, self_guid))
}

pub(crate) fn quest_update_feedback(
    old: &CharacterQuest,
    row: &CharacterQuest,
    objectives: &[(u8, u8, u32, u32)],
) -> Vec<Outbound> {
    let mut outbound =
        codec::kill_progress_add_kills(row.quest_entry, &old.counts, &row.counts, objectives)
            .into_iter()
            .map(|packet| {
                Outbound::One(ServerOpcodeMessage::SMSG_QUESTUPDATE_ADD_KILL(Box::new(
                    packet,
                )))
            })
            .collect::<Vec<_>>();
    let explored = objectives.iter().any(|&(kind, index, _, required)| {
        let count = |counts: &[u32]| counts.get(index as usize).copied().unwrap_or(0);
        kind == lyracore_shared::quest::objective_kind::EXPLORE_AREATRIGGER
            && count(&old.counts) < required
            && count(&row.counts) >= required
    });
    if explored {
        outbound.push(Outbound::One(
            ServerOpcodeMessage::SMSG_QUESTUPDATE_COMPLETE(codec::build_questupdate_complete(
                row.quest_entry,
            )),
        ));
    }
    if !old.failed && row.failed {
        outbound.push(Outbound::One(
            ServerOpcodeMessage::SMSG_QUESTUPDATE_FAILEDTIMER(
                codec::build_questupdate_failedtimer(row.quest_entry),
            ),
        ));
    }
    outbound
}

pub(crate) fn quest_update_packets(
    old: &CharacterQuest,
    row: &CharacterQuest,
    objectives: &[(u8, u8, u32, u32)],
    quest_log: Option<Outbound>,
) -> Vec<Outbound> {
    let mut outbound = quest_update_feedback(old, row, objectives);
    if let Some(packet) = quest_log {
        outbound.push(packet);
    }
    outbound
}

pub(crate) fn quest_inserted(
    view: &WorldView,
    coord: &Coordinator,
    shard: ShardId,
    row: &CharacterQuest,
) {
    let Some(viewer) = view.viewer_of_owner_on_shard(shard, OwnerGuid(row.character_guid)) else {
        return;
    };
    let (coord, self_guid) = (coord.clone(), viewer.self_guid);
    enqueue(viewer.clone(), move |_| {
        let guard = coord.0.coord();
        crate::codec::quest::subscriptions::quest_log_outbound(&guard, self_guid)
            .into_iter()
            .collect()
    });
}

pub(crate) fn quest_updated(
    view: &WorldView,
    coord: &Coordinator,
    shard: ShardId,
    old: &CharacterQuest,
    row: &CharacterQuest,
) {
    let Some(viewer) = view.viewer_of_owner_on_shard(shard, OwnerGuid(row.character_guid)) else {
        return;
    };
    let (coord, self_guid, old, row) = (coord.clone(), viewer.self_guid, old.clone(), row.clone());
    enqueue(viewer.clone(), move |_| {
        let guard = coord.0.coord();
        crate::codec::quest::subscriptions::quest_update_outbound(&guard, self_guid, &old, &row)
    });
}

pub(crate) fn quest_deleted(
    view: &WorldView,
    coord: &Coordinator,
    shard: ShardId,
    row: &CharacterQuest,
) {
    let Some(viewer) = view.viewer_of_owner_on_shard(shard, OwnerGuid(row.character_guid)) else {
        return;
    };
    let (coord, self_guid) = (coord.clone(), viewer.self_guid);
    enqueue(viewer.clone(), move |_| {
        let guard = coord.0.coord();
        crate::codec::quest::subscriptions::quest_log_outbound(&guard, self_guid)
            .into_iter()
            .collect()
    });
}

crate::stdb::relays::declare_relays! {
    game_character_quest [false] {
        insert(Snapshot) => |context, _event, row| quest_inserted(&context.view, &context.coord, context.shard, row);
        update(Snapshot) => |context, _event, old, row| quest_updated(&context.view, &context.coord, context.shard, old, row);
        delete(Snapshot) => |context, _event, row| quest_deleted(&context.view, &context.coord, context.shard, row);
    }
    game_character_quest_event_credit [false] {
        insert(Live) => |context, _event,  row| quest_credit_granted(&context.view, &context.coord, context.shard, row);
        update(Snapshot) => |context, _event,  _old, row| quest_credit_changed(&context.view, &context.coord, context.shard, row);
        delete(Snapshot) => |context, _event,  row| quest_credit_changed(&context.view, &context.coord, context.shard, row);
    }
    game_quest_event_requirement [false] {
        insert(Snapshot) => |context, _event,  _row| quest_requirements_changed(&context.view, &context.coord, context.shard);
        update(Snapshot) => |context, _event,  _old, _row| quest_requirements_changed(&context.view, &context.coord, context.shard);
        delete(Snapshot) => |context, _event,  _row| quest_requirements_changed(&context.view, &context.coord, context.shard);
    }
}

/// `Live` insert: a login replay of old credits stays silent.
fn quest_credit_granted(
    view: &WorldView,
    coord: &Coordinator,
    shard: ShardId,
    row: &CharacterQuestEventCredit,
) {
    let Some(viewer) = view.viewer_of_owner_on_shard(shard, OwnerGuid(row.character_guid)) else {
        return;
    };
    let (coord, quest_entry) = (coord.clone(), row.quest_entry);
    enqueue(viewer, move |viewer| {
        let guard = coord.0.coord();
        event_credit_packets(quest_entry, quest_log_outbound(&guard, viewer.self_guid))
    });
}

pub(crate) fn event_credit_packets(quest_entry: u32, quest_log: Option<Outbound>) -> Vec<Outbound> {
    let mut outbound = vec![Outbound::One(
        ServerOpcodeMessage::SMSG_QUESTUPDATE_COMPLETE(codec::build_questupdate_complete(
            quest_entry,
        )),
    )];
    outbound.extend(quest_log);
    outbound
}

fn quest_credit_changed(
    view: &WorldView,
    coord: &Coordinator,
    shard: ShardId,
    row: &CharacterQuestEventCredit,
) {
    let Some(viewer) = view.viewer_of_owner_on_shard(shard, OwnerGuid(row.character_guid)) else {
        return;
    };
    let coord = coord.clone();
    enqueue(viewer, move |viewer| {
        let guard = coord.0.coord();
        quest_log_outbound(&guard, viewer.self_guid)
            .into_iter()
            .collect()
    });
}

fn quest_requirements_changed(view: &WorldView, coord: &Coordinator, shard: ShardId) {
    for viewer in view.viewers_on_shard(shard) {
        let coord = coord.clone();
        enqueue(viewer, move |viewer| {
            let guard = coord.0.coord();
            quest_log_outbound(&guard, viewer.self_guid)
                .into_iter()
                .collect()
        });
    }
}

#[cfg(test)]
mod audience_tests;
