//! Table Relays for this Protocol Family.

use crate::codec::{self};
use crate::stdb::bindings::*;
use crate::stdb::world_index::ShardId;
use crate::stdb::world_view::*;
use crate::stdb::Coordinator;
use crate::world::Outbound;
use spacetimedb_sdk::Table;
use wow_world_messages::vanilla::opcodes::ServerOpcodeMessage;

/// Build the XP-gain result for one identity-addressed row.
pub(crate) fn xp_event_outbound(row: &XpEvent) -> Vec<Outbound> {
    let (opcode, body) = codec::build_log_xpgain_raw(
        row.killed_guid,
        row.total_exp,
        row.is_kill,
        row.rested_bonus,
    );
    vec![Outbound::Raw { opcode, body }]
}

/// Build the level-up result for one identity-addressed row.
pub(crate) fn levelup_event_outbound(row: &LevelupEvent) -> Vec<Outbound> {
    let message = codec::build_levelup_info(
        row.new_level,
        row.health_gained,
        row.mana_gained,
        row.strength_gained,
        row.agility_gained,
        row.stamina_gained,
        row.intellect_gained,
        row.spirit_gained,
    );
    vec![Outbound::One(ServerOpcodeMessage::SMSG_LEVELUP_INFO(
        Box::new(message),
    ))]
}

/// The shared-dispatch twin of the per-player
/// `on_rest_insert` relay (rest state). Same raw PLAYER_BYTES_2 VALUES packet; the audience (owner
/// only) was already resolved by the owner-session lookup in `world_view::rest_state_appeared`.
pub(crate) fn relay_rest_state(self_guid: u64, player_bytes_2: u32) -> Vec<Outbound> {
    let (opcode, body) = codec::build_rest_state_values(self_guid, player_bytes_2);
    vec![Outbound::Raw { opcode, body }]
}

/// XP rows are delivered to the one viewer bound to their recipient identity.
pub(crate) fn xp_appeared(view: &WorldView, shard: ShardId, row: &XpEvent) {
    let Some(viewer) =
        view.viewer_of_identity_on_shard(shard, BoundIdentity(row.recipient_identity))
    else {
        return;
    };
    let row = row.clone();
    enqueue(viewer.clone(), move |_| {
        crate::codec::leveling::subscriptions::xp_event_outbound(&row)
    });
}

/// Level-up rows are delivered to the one viewer bound to their recipient identity.
pub(crate) fn levelup_appeared(view: &WorldView, shard: ShardId, row: &LevelupEvent) {
    let Some(viewer) =
        view.viewer_of_identity_on_shard(shard, BoundIdentity(row.recipient_identity))
    else {
        return;
    };
    let row = row.clone();
    enqueue(viewer.clone(), move |_| {
        crate::codec::leveling::subscriptions::levelup_event_outbound(&row)
    });
}

/// An explored-area row restores the complete fog word first, then reports a new discovery.
/// Cache reads and replay-state mutation stay on the addressed session's writer queue.
pub(crate) fn exploration_appeared(
    view: &WorldView,
    coord: &Coordinator,
    shard: ShardId,
    row: &CharacterExplored,
) {
    let Some(viewer) = view.viewer_of_owner_on_shard(shard, OwnerGuid(row.character_guid)) else {
        return;
    };
    let (row, coord) = (row.clone(), coord.clone());
    enqueue(viewer.clone(), move |viewer| {
        exploration_outbound(&coord, &viewer, &row)
    });
}

pub(crate) fn exploration_outbound(
    coord: &Coordinator,
    viewer: &Viewer,
    row: &CharacterExplored,
) -> Vec<Outbound> {
    if row.area_bit < 0 {
        return Vec::new();
    }
    let word_idx = (row.area_bit / 32) as u16;
    let lo = word_idx as i32 * 32;
    let word = {
        let guard = coord.0.coord();
        guard
            .conn
            .db
            .game_character_explored()
            .iter()
            .filter(|candidate| {
                candidate.character_guid == viewer.self_guid
                    && candidate.area_bit >= lo
                    && candidate.area_bit < lo + 32
            })
            .fold(0u32, |word, candidate| {
                word | (1u32 << (candidate.area_bit - lo))
            })
    };
    exploration_outbound_for_word(viewer, row, word_idx, word)
}

pub(crate) fn exploration_outbound_for_word(
    viewer: &Viewer,
    row: &CharacterExplored,
    word_idx: u16,
    word: u32,
) -> Vec<Outbound> {
    let (opcode, body) =
        crate::codec::build_explored_zones_values(viewer.self_guid, word_idx, word);
    let mut outbound = vec![Outbound::Raw { opcode, body }];
    if viewer.explored.lock().unwrap().admit_once(row.area_bit) {
        let (opcode, body) =
            crate::codec::build_exploration_experience_raw(row.area_id, row.experience);
        outbound.push(Outbound::Raw { opcode, body });
    }
    outbound
}

/// Send a rest-state change to its owner on the callback's Shard.
pub(crate) fn rest_state_appeared(view: &WorldView, shard: ShardId, row: &RestStateEvent) {
    let Some(viewer) = view.viewer_of_owner_on_shard(shard, OwnerGuid(row.character_guid)) else {
        return;
    };
    let (self_guid, bytes) = (row.character_guid, row.player_bytes_2);
    enqueue(viewer.clone(), move |_| {
        crate::codec::leveling::subscriptions::relay_rest_state(self_guid, bytes)
    });
}

crate::stdb::relays::declare_relays! {
    game_xp_event [false] {
        insert(Live) => |context, _event, row| xp_appeared(&context.view, context.shard, row);
    }
    game_levelup_event [false] {
        insert(Live) => |context, _event, row| levelup_appeared(&context.view, context.shard, row);
    }
    game_character_explored [false] {
        insert(Snapshot) => |context, _event, row| exploration_appeared(&context.view, &context.coord, context.shard, row);
    }
    game_rest_state_event [false] {
        insert(Snapshot) => |context, _event, row| rest_state_appeared(&context.view, context.shard, row);
    }
}
