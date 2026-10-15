use crate::codec::{self};
use crate::stdb::bindings::*;
use crate::stdb::world_index::ShardId;
use crate::stdb::world_view::*;
use crate::world::Outbound;
use wow_world_messages::vanilla::opcodes::ServerOpcodeMessage;

pub(crate) fn relay_skill(
    viewer: &Viewer,
    skill_line: u32,
    current: u16,
    max_rank: u16,
) -> Vec<Outbound> {
    let mut guard = viewer.skill_slots.lock().unwrap();
    let (map, next_free) = &mut *guard;
    let slot = *map.entry(skill_line).or_insert_with(|| {
        let s = *next_free;
        *next_free += 1;
        s
    });
    if let Ok(sk) = wow_world_messages::vanilla::Skill::try_from(skill_line) {
        if let Some(m) = codec::build_skill_values(viewer.self_guid, slot, sk, current, max_rank) {
            return vec![Outbound::One(ServerOpcodeMessage::SMSG_UPDATE_OBJECT(
                Box::new(m),
            ))];
        }
    }
    Vec::new()
}

pub(crate) fn skill_changed(view: &WorldView, shard: ShardId, row: &PlayerSkill) {
    let Some(viewer) = view.viewer_of_owner_on_shard(shard, OwnerGuid(row.character_guid)) else {
        return;
    };
    let (line, cur, max) = (row.skill_line, row.current, row.max_rank);
    enqueue(viewer.clone(), move |viewer| {
        crate::codec::trainer::subscriptions::relay_skill(&viewer, line, cur, max)
    });
}

crate::stdb::relays::declare_relays! {
    game_player_skill [false] {
        insert(Snapshot) => |context, _event, row| skill_changed(&context.view, context.shard, row);
        update(Snapshot) => |context, _event, _old, row| skill_changed(&context.view, context.shard, row);
    }
}
