//! Table Relays for this Protocol Family.

use crate::codec::{self};
use crate::stdb::bindings::*;
use crate::stdb::world_index::ShardId;
use crate::stdb::world_view::*;
use crate::stdb::Coordinator;
use crate::world::Outbound;
use spacetimedb_sdk::Table;
use wow_world_messages::vanilla::opcodes::ServerOpcodeMessage;

/// Build the full quest-log update from the coordinator cache. The empty mask deliberately clears
/// every quest-log field when the final quest leaves the log.
pub(crate) fn quest_log_outbound(
    db: &crate::stdb::bindings::RemoteTables,
    self_guid: u64,
) -> Option<Outbound> {
    if !crate::config::quest_log_fields_enabled() {
        return None;
    }
    let quests: Vec<_> = db.game_character_quest().iter().collect();
    let slots = crate::stdb::reads::build_quest_log_slots(db, &quests, self_guid);
    let mask = codec::update_mask::full_quest_log_mask(&slots);
    let (opcode, body) = codec::build_values_update_raw(self_guid, &mask);
    Some(Outbound::Raw { opcode, body })
}

/// Build all packets for one quest update, in the order the client expects.
pub(crate) fn quest_update_outbound(
    db: &crate::stdb::bindings::RemoteTables,
    self_guid: u64,
    old: &CharacterQuest,
    row: &CharacterQuest,
) -> Vec<Outbound> {
    let objectives: Vec<_> = db
        .game_quest_objective()
        .iter()
        .filter(|objective| objective.quest_entry == row.quest_entry)
        .map(|objective| {
            (
                objective.kind,
                objective.obj_index,
                objective.target_entry,
                objective.required_count,
            )
        })
        .collect();
    quest_update_packets(old, row, &objectives, quest_log_outbound(db, self_guid))
}

/// Build the feedback that precedes a full quest-log sync for one update.
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
    if !old.failed && row.failed {
        outbound.push(Outbound::One(
            ServerOpcodeMessage::SMSG_QUESTUPDATE_FAILEDTIMER(
                codec::build_questupdate_failedtimer(row.quest_entry),
            ),
        ));
    }
    outbound
}

/// Append a post-update full-log sync after kill and timer feedback.
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

/// A quest-log insert names one owner. The full log is read after the delta on that owner's writer.
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
        crate::codec::quest::subscriptions::quest_log_outbound(&guard.conn.db, self_guid)
            .into_iter()
            .collect()
    });
}

/// A quest update keeps kill feedback, timer failure, then the full quest-log sync in one writer job.
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
        crate::codec::quest::subscriptions::quest_update_outbound(
            &guard.conn.db,
            self_guid,
            &old,
            &row,
        )
    });
}

/// A quest deletion re-sends the full log, including the all-zero final-log clear.
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
        crate::codec::quest::subscriptions::quest_log_outbound(&guard.conn.db, self_guid)
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
        insert(Snapshot) => |context, _event,  row| quest_credit_changed(&context.view, &context.coord, context.shard, row);
        update(Snapshot) => |context, _event,  _old, row| quest_credit_changed(&context.view, &context.coord, context.shard, row);
        delete(Snapshot) => |context, _event,  row| quest_credit_changed(&context.view, &context.coord, context.shard, row);
    }
    game_quest_event_requirement [false] {
        insert(Snapshot) => |context, _event,  _row| quest_requirements_changed(&context.view, &context.coord, context.shard);
        update(Snapshot) => |context, _event,  _old, _row| quest_requirements_changed(&context.view, &context.coord, context.shard);
        delete(Snapshot) => |context, _event,  _row| quest_requirements_changed(&context.view, &context.coord, context.shard);
    }
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
        quest_log_outbound(&guard.conn.db, viewer.self_guid)
            .into_iter()
            .collect()
    });
}

fn quest_requirements_changed(view: &WorldView, coord: &Coordinator, shard: ShardId) {
    for viewer in view.viewers_on_shard(shard) {
        let coord = coord.clone();
        enqueue(viewer, move |viewer| {
            let guard = coord.0.coord();
            quest_log_outbound(&guard.conn.db, viewer.self_guid)
                .into_iter()
                .collect()
        });
    }
}

#[cfg(test)]
mod audience_tests;
