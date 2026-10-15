use anyhow::Result;
use spacetimedb_sdk::Table;

use super::super::bindings::*;
use super::super::connection::Coordinator;

impl Coordinator {
    pub fn npc_refuses_interaction(&self, npc_guid: u64, player_guid: u64) -> Result<bool> {
        let guard = self.0.coord();
        let db = &guard.conn.db;
        let entities = db.game_world_entity();
        let (Some(npc), Some(player)) = (
            entities.guid().find(&npc_guid),
            entities.guid().find(&player_guid),
        ) else {
            return Ok(false);
        };
        let templates = db.game_faction_template();
        let Some(npc_ft) = templates.id().find(&npc.faction_template) else {
            return Ok(false);
        };
        let reputation = db
            .game_faction()
            .faction_id()
            .find(&npc_ft.faction)
            .filter(|parent| parent.reputation_index >= 0)
            .and_then(|parent| {
                db.game_player_reputation().iter().find(|row| {
                    row.character_guid == player_guid && row.faction_id == parent.faction_id
                })
            })
            .map(|row| (row.standing, row.at_war));
        let template_hostile = templates
            .id()
            .find(&player.faction_template)
            .is_some_and(|player_ft| faction_template_hostile(&npc_ft, &player_ft));
        Ok(lyracore_shared::faction::refuses_interaction(
            reputation,
            template_hostile,
        ))
    }
}

fn faction_template_hostile(
    a: &super::super::bindings::FactionTemplate,
    b: &super::super::bindings::FactionTemplate,
) -> bool {
    lyracore_shared::faction::relation_holds(
        &[a.enemy_0, a.enemy_1, a.enemy_2, a.enemy_3],
        &[a.friend_0, a.friend_1, a.friend_2, a.friend_3],
        a.enemy_group,
        b.faction,
        b.faction_group,
    )
}

#[cfg(test)]
mod tests {
    #[test]
    fn reputation_rank_thresholds_match_mangos() {
        use lyracore_shared::faction::reputation_rank;
        assert_eq!(reputation_rank(42000), 7);
        assert_eq!(reputation_rank(9000), 5);
        assert_eq!(reputation_rank(0), 3);
        assert_eq!(reputation_rank(-1), 2);
        assert_eq!(reputation_rank(-3001), 1);
        assert_eq!(reputation_rank(-42000), 0);
        assert!(reputation_rank(-1) <= 2, "Unfriendly refuses");
        assert!(reputation_rank(0) > 2, "Neutral interacts");
    }

    #[test]
    fn faction_template_hostility_mask_and_explicit_lists() {
        use super::super::super::bindings::FactionTemplate;
        let ft = |id, faction, fg, eg, enemy_0, friend_0| FactionTemplate {
            id,
            faction,
            faction_group: fg,
            friend_group: 0,
            enemy_group: eg,
            enemy_0,
            enemy_1: 0,
            enemy_2: 0,
            enemy_3: 0,
            friend_0,
            friend_1: 0,
            friend_2: 0,
            friend_3: 0,
        };
        let monster = ft(14, 14, 8, 1, 0, 0);
        let player = ft(1, 1, 1, 0, 0, 0);
        assert!(super::faction_template_hostile(&monster, &player));
        assert!(
            !super::faction_template_hostile(&player, &monster),
            "player group 0 enemy mask"
        );
        let befriended = ft(2, 14, 8, 1, 0, 1);
        assert!(!super::faction_template_hostile(&befriended, &player));
        let sworn = ft(3, 14, 8, 0, 1, 0);
        assert!(super::faction_template_hostile(&sworn, &player));
    }
}
