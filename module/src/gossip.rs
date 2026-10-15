use lyracore_shared::gossip::{self, CharacterFacts, Condition, QuestState};
use spacetimedb::{table, ReducerContext};

use crate::creatures::{
    game_creature_gossip_menu_override, game_gossip_menu_profile, game_gossip_menu_profile_option,
    game_gossip_option,
};
use crate::quest::game_character_quest;

#[table(accessor = game_gossip_condition, public)]
pub struct GossipCondition {
    #[primary_key]
    pub condition_id: u32,
    pub kind: i32,
    pub value1: u32,
    pub value2: u32,
    pub value3: u32,
    pub value4: u32,
    pub flags: u8,
}

/// `source_menu_id` is 0 for the entry menu, or the active profile id.
pub(crate) fn select_gate(
    ctx: &ReducerContext,
    character_guid: u64,
    npc_guid: u64,
    option_row_id: u32,
    source_menu_id: u32,
) -> Result<(), String> {
    let (actor, npc) = crate::items::npc_interaction_gate(
        ctx,
        character_guid,
        npc_guid,
        u32::MAX,
        "gossip creature",
        "target has no gossip",
        "use gossip",
    )?;
    if npc.dead || crate::reputation::npc_refuses_interaction(ctx, &npc, &actor) {
        return Err("gossip creature is unavailable".into());
    }
    let active_menu = ctx
        .db
        .game_creature_gossip_menu_override()
        .creature_guid()
        .find(npc_guid)
        .filter(|menu| menu.map_id == npc.map_id && menu.instance_id == npc.instance_id)
        .and_then(|menu| {
            ctx.db
                .game_gossip_menu_profile()
                .menu_id()
                .find(menu.menu_id)
        });
    if source_menu_id != active_menu.as_ref().map_or(0, |menu| menu.menu_id) {
        return Err("gossip menu has changed".into());
    }
    // u32::MAX denotes a synthesized service line without an imported row.
    if option_row_id == u32::MAX {
        return Ok(());
    }
    let condition = if let Some(menu) = active_menu {
        ctx.db
            .game_gossip_menu_profile_option()
            .row_id()
            .find(option_row_id)
            .filter(|option| option.menu_id == menu.menu_id)
            .map(|option| (option.cond_type, option.cond_value1, option.cond_value2))
    } else {
        ctx.db
            .game_gossip_option()
            .row_id()
            .find(option_row_id)
            .filter(|option| option.entry == npc.entry)
            .map(|option| (option.cond_type, option.cond_value1, option.cond_value2))
    }
    .ok_or_else(|| "gossip option does not belong to this creature's active menu".to_string())?;
    let character = crate::helpers::character_by_guid(ctx, character_guid)
        .ok_or_else(|| "gossip Character is unavailable".to_string())?;
    let result = gossip::evaluate_option(
        condition.0,
        condition.1,
        condition.2,
        CharacterFacts {
            level: actor.level,
            race: character.race,
            class: character.class,
        },
        |quest_entry| {
            let Some(quest) = ctx
                .db
                .game_character_quest()
                .by_character_quest()
                .filter((character_guid, quest_entry))
                .next()
            else {
                return QuestState::Absent;
            };
            if quest.rewarded {
                QuestState::Rewarded
            } else if quest.failed {
                QuestState::Failed
            } else if crate::quest::quest_is_complete(ctx, &quest) {
                QuestState::Complete
            } else {
                QuestState::Incomplete
            }
        },
        |id| {
            ctx.db
                .game_gossip_condition()
                .condition_id()
                .find(id)
                .map(|row| Condition {
                    kind: row.kind,
                    values: [row.value1, row.value2, row.value3, row.value4],
                    flags: row.flags,
                })
        },
    );
    match result {
        Ok(true) => Ok(()),
        Ok(false) => Err("gossip option is no longer available".into()),
        Err(reason) => Err(format!(
            "gossip option condition is unavailable: {reason:?}"
        )),
    }
}
