use spacetimedb::ReducerContext;

pub struct DamageTakenPayload {
    pub target_guid: u64,
    pub attacker_guid: u64,
    pub periodic: bool,
}

pub struct DeathPreventedPayload {
    pub creature_guid: u64,
    pub attacker_guid: u64,
}

pub struct CreatureSpawnPayload {
    pub guid: u64,
    pub entry: u32,
    pub x: f32,
    pub y: f32,
    pub z: f32,
}

pub struct GroupInvitePayload {
    pub target_guid: u64,
    pub inviter_guid: u64,
}

pub struct LevelupPayload {
    pub character_guid: u64,
    pub new_level: u32,
}

pub struct DeathPayload {
    pub victim_guid: u64,
    pub killer_guid: u64,
    pub victim_is_player: bool,
}

pub struct KillPayload {
    pub killer_guid: u64,
    pub victim_guid: u64,
    pub victim_entry: u32,
    pub victim_level: u32,
}

pub struct AggroPayload {
    pub creature_guid: u64,
    pub target_guid: u64,
    pub assist: bool,
}

pub struct CastResolvedPayload {
    pub caster_guid: u64,
    pub spell_id: u32,
    pub target_guid: u64,
}

pub struct CastFinishedPayload {
    pub caster_guid: u64,
    pub target_guid: u64,
    pub scheduled_id: u64,
    pub outcome: crate::spell::CastFinish,
}

pub struct LootPayload {
    pub looter_guid: u64,
    pub corpse_guid: u64,
    pub item_entry: u32,
    pub count: u32,
}

pub struct QuestAcceptPayload {
    pub character_guid: u64,
    pub quest_entry: u32,
}

pub struct QuestTurninPayload {
    pub character_guid: u64,
    pub quest_entry: u32,
}

pub struct LoginPayload {
    pub character_guid: u64,
}

pub struct CharacterRelocatedPayload {
    pub character_guid: u64,
}

pub struct LogoutPayload {
    pub character_guid: u64,
}

pub struct GossipSelectPayload {
    pub character_guid: u64,
    pub npc_guid: u64,
    // option_id is a per-viewer menu position; use option_row_id for stable option identity.
    pub option_id: u32,
    pub option_row_id: u32,
}

pub struct CreatureDeathPayload {
    pub creature_guid: u64,
    pub entry: u32,
    pub instance_id: u64,
    pub killer_guid: u64,
    pub current_target_guid: u64,
    pub threat_snapshot: Vec<(u64, i64)>,
}

pub struct HpThresholdPayload {
    pub creature_guid: u64,
    pub entry: u32,
    pub instance_id: u64,
    pub pct: u8,
}

pub struct GoUsedPayload {
    pub go_guid: u64,
    pub go_entry: u32,
    pub user_guid: u64,
    pub instance_id: u64,
}

include!(concat!(env!("OUT_DIR"), "/hook_dispatch.rs"));

pub(crate) fn run_package_tick_passes(ctx: &ReducerContext) {
    crate::package_teardown::forget_removed_packages(ctx);
    for (_name, f) in crate::GAME_TICK_PASSES {
        f(ctx);
    }
}
