use anyhow::Result;
use lyracore_shared::trainer::TrainerRefusal;
use spacetimedb_sdk::Table;

use crate::stdb::bindings::*;
use crate::stdb::connection::{call_reducer, reducer_refusal_reason};
use crate::stdb::reads::spell_ranks_stack_in_book;
use crate::stdb::Coordinator;
use crate::world::{Actor, InteractionOutcome, TrainerStore};

use super::interaction_outcome;

impl TrainerStore for Coordinator {
    fn talent_reset_cost(&self, character_guid: u64) -> Option<u32> {
        self.0
            .coord()
            .conn
            .db
            .game_character()
            .guid()
            .find(&character_guid)
            .map(|character| lyracore_shared::talent::respec_cost_copper(character.respec_count))
    }

    fn character_presence(&self, guid: u64) -> Result<Option<(bool, u8, u8, u32)>> {
        Ok(self
            .0
            .coord()
            .conn
            .db
            .game_character()
            .guid()
            .find(&guid)
            .map(|c| (c.online, c.level, c.class, c.zone_id)))
    }

    fn trainer_serves(&self, player_guid: u64, trainer_guid: u64) -> Result<bool> {
        let guard = self.0.coord();
        let db = &guard.conn.db;
        let (Some(trainer), Some(player)) = (
            db.game_world_entity().guid().find(&trainer_guid),
            db.game_character().guid().find(&player_guid),
        ) else {
            return Ok(true);
        };
        Ok(db
            .game_creature_template()
            .entry()
            .find(&trainer.entry)
            .is_none_or(|t| {
                lyracore_shared::trainer::serves(player.class, t.trainer_type, t.trainer_class)
            }))
    }

    fn trainer_offer_skill_line(&self, trainer_guid: u64, spell_id: u32) -> u32 {
        let guard = self.0.coord();
        let db = &guard.conn.db;
        let Some(entry) = db
            .game_world_entity()
            .guid()
            .find(&trainer_guid)
            .map(|e| e.entry)
        else {
            return 0;
        };
        let line = db
            .game_trainer_spell()
            .iter()
            .find(|t| t.trainer_entry == entry && t.spell_id == spell_id)
            .map(|t| t.learn_skill_line)
            .unwrap_or(0);
        line
    }

    fn trainer_list(
        &self,
        player_guid: u64,
        trainer_guid: u64,
    ) -> Result<Vec<crate::codec::TrainerSpellView>> {
        let guard = self.0.coord();
        let db = &guard.conn.db;
        let Some(trainer_entry) = db
            .game_world_entity()
            .guid()
            .find(&trainer_guid)
            .map(|e| e.entry)
        else {
            return Ok(Vec::new());
        };
        let Some(player) = db.game_world_entity().guid().find(&player_guid) else {
            return Ok(Vec::new());
        };
        let learned: std::collections::HashSet<u32> = db
            .game_player_spell()
            .iter()
            .filter(|s| s.character_guid == player_guid)
            .map(|s| s.spell_id)
            .collect();
        let skill_caps: std::collections::HashMap<u32, u32> = db
            .game_player_skill()
            .iter()
            .filter(|s| s.character_guid == player_guid)
            .map(|s| (s.skill_line, s.max_rank as u32))
            .collect();
        let mut rows: Vec<_> = db
            .game_trainer_spell()
            .iter()
            .filter(|t| t.trainer_entry == trainer_entry)
            .collect();
        rows.sort_by_key(|t| t.spell_id);
        let wrapper_ids: std::collections::HashSet<_> = rows
            .iter()
            .filter(|offering| offering.learn_skill_line == 0)
            .map(|offering| offering.spell_id)
            .collect();
        let mut effects: std::collections::HashMap<
            u32,
            Vec<lyracore_shared::trainer::LearnEffect>,
        > = std::collections::HashMap::new();
        for effect in db
            .game_spell_effect()
            .iter()
            .filter(|effect| wrapper_ids.contains(&effect.spell_id))
        {
            effects.entry(effect.spell_id).or_default().push(
                lyracore_shared::trainer::LearnEffect {
                    index: effect.effect_index,
                    id: effect.id,
                    kind: effect.kind,
                    trigger_spell: effect.trigger_spell,
                },
            );
        }
        Ok(rows
            .into_iter()
            .map(|t| crate::codec::TrainerSpellView {
                spell_id: t.spell_id,
                cost: t.cost,
                required_level: t.required_level,
                player_level: player.level,
                known: lyracore_shared::trainer::offering_known(
                    t.learn_skill_line,
                    t.learn_skill_cap,
                    skill_caps.get(&t.learn_skill_line).copied(),
                    learned.contains(&lyracore_shared::trainer::resolve_learn_target(
                        t.spell_id,
                        effects.get(&t.spell_id).into_iter().flatten().copied(),
                    )),
                ),
                learn_skill_line: t.learn_skill_line,
            })
            .collect())
    }

    fn superseded_old_rank(&self, new_spell: u32, player_guid: u64) -> Option<u32> {
        let guard = self.0.coord();
        let db = &guard.conn.db;
        if spell_ranks_stack_in_book(db, new_spell) {
            return None;
        }
        let prev = db
            .game_spell_chain()
            .spell_id()
            .find(&new_spell)
            .map(|c| c.prev_spell)?;
        if prev == 0 || prev == new_spell {
            return None;
        }
        let knows_prev = db
            .game_player_spell()
            .iter()
            .any(|s| s.character_guid == player_guid && s.spell_id == prev);
        knows_prev.then_some(prev)
    }

    fn resolve_learn_target(&self, spell_id: u32) -> u32 {
        let guard = self.0.coord();
        lyracore_shared::trainer::resolve_learn_target(
            spell_id,
            guard
                .conn
                .db
                .game_spell_effect()
                .iter()
                .filter(|effect| effect.spell_id == spell_id)
                .map(|effect| lyracore_shared::trainer::LearnEffect {
                    index: effect.effect_index,
                    id: effect.id,
                    kind: effect.kind,
                    trigger_spell: effect.trigger_spell,
                }),
        )
    }

    fn talent_grant_spell(&self, talent_id: u32) -> u32 {
        self.0
            .coord()
            .conn
            .db
            .game_talent()
            .iter()
            .find(|t| t.talent_id == talent_id)
            .map(|t| t.grant_spell_id)
            .unwrap_or(0)
    }

    fn talent_points_spent(&self, character_guid: u64) -> u32 {
        let guard = self.0.coord();
        guard
            .conn
            .db
            .game_character_talent()
            .iter()
            .filter(|t| t.character_guid == character_guid)
            .map(|t| t.rank as u32)
            .sum()
    }

    fn talent_pane_sync(&self, character_guid: u64, talent_id: u32) -> (u32, u32, u32) {
        let guard = self.0.coord();
        let db = &guard.conn.db;
        let level = db
            .game_world_entity()
            .guid()
            .find(&character_guid)
            .map(|e| e.level)
            .unwrap_or(0);
        let spent: u32 = db
            .game_character_talent()
            .iter()
            .filter(|t| t.character_guid == character_guid)
            .map(|t| t.rank as u32)
            .sum();
        let remaining = lyracore_shared::talent::points_available(level, spent);
        let Some(t) = db.game_talent().iter().find(|t| t.talent_id == talent_id) else {
            return (0, 0, remaining);
        };
        let rank = db
            .game_character_talent()
            .iter()
            .find(|r| r.character_guid == character_guid && r.talent_id == talent_id)
            .map(|r| r.rank)
            .unwrap_or(0);
        let new_spell = pick_rank_spell(rank, &t);
        let prev_spell = if rank >= 2 {
            pick_rank_spell(rank - 1, &t)
        } else {
            0
        };
        let teach = if new_spell != 0
            && db
                .game_player_spell()
                .iter()
                .any(|spell| spell.character_guid == character_guid && spell.spell_id == new_spell)
        {
            new_spell
        } else {
            0
        };
        let superseded = if teach != 0 && prev_spell != 0 && prev_spell != teach {
            prev_spell
        } else {
            0
        };
        (teach, superseded, remaining)
    }

    fn buy_trainer_spell(
        &self,
        actor: Actor,
        trainer_guid: u64,
        spell_id: u32,
    ) -> Result<crate::world::TrainerBuyOutcome> {
        let coord = self.0.call_pipe();
        let result: Result<()> = call_reducer!(
            coord.conn.reducers,
            "gw_trainer_buy",
            gw_trainer_buy_then(self.session_actor(actor), trainer_guid, spell_id)
        );
        match result {
            Ok(()) => Ok(crate::world::TrainerBuyOutcome::Learned),
            Err(error) => match trainer_refusal(&error) {
                Some(refusal) => Ok(refusal.into()),
                None => Err(error),
            },
        }
    }

    fn learn_talent(&self, actor: Actor, talent_id: u32) -> Result<()> {
        let coord = self.0.call_pipe();
        call_reducer!(
            coord.conn.reducers,
            "gw_learn_talent",
            gw_learn_talent_then(self.session_actor(actor), talent_id)
        )
    }

    fn reset_talents(&self, actor: Actor, trainer_guid: u64) -> Result<InteractionOutcome> {
        let coord = self.0.call_pipe();
        interaction_outcome(call_reducer!(
            coord.conn.reducers,
            "gw_reset_talents",
            gw_reset_talents_then(self.session_actor(actor), trainer_guid)
        ))
    }

    fn set_action_button(
        &self,
        actor: Actor,
        button: u8,
        action: u32,
        action_type: u8,
    ) -> Result<()> {
        let coord = self.0.call_pipe();
        call_reducer!(
            coord.conn.reducers,
            "gw_set_action_button",
            gw_set_action_button_then(self.session_actor(actor), button, action, action_type)
        )
    }

    /// The wire's Faction field is a ReputationListID slot, 0..63, not a faction id.
    fn set_faction_at_war(
        &self,
        actor: Actor,
        reputation_index: u32,
        at_war: bool,
    ) -> Result<InteractionOutcome> {
        let coord = self.0.call_pipe();
        interaction_outcome(call_reducer!(
            coord.conn.reducers,
            "gw_set_faction_at_war",
            gw_set_faction_at_war_then(self.session_actor(actor), reputation_index, at_war)
        ))
    }

    fn set_watched_faction(
        &self,
        actor: Actor,
        reputation_index: i32,
    ) -> Result<InteractionOutcome> {
        let coord = self.0.call_pipe();
        let outcome = interaction_outcome(call_reducer!(
            coord.conn.reducers,
            "gw_set_watched_faction",
            gw_set_watched_faction_then(self.session_actor(actor), reputation_index)
        ))?;
        if matches!(outcome, InteractionOutcome::Done) {
            super::wait_for_cache_row(actor.guid(), "watched faction", || {
                self.0
                    .coord()
                    .conn
                    .db
                    .game_character()
                    .guid()
                    .find(&actor.guid())
                    .filter(|character| character.watched_faction_index == reputation_index)
            })?;
        }
        Ok(outcome)
    }
}

fn pick_rank_spell(rank: u8, t: &Talent) -> u32 {
    lyracore_shared::talent::rank_spell(
        rank,
        [
            t.spell_id,
            t.rank_spell_2,
            t.rank_spell_3,
            t.rank_spell_4,
            t.rank_spell_5,
        ],
    )
}

fn trainer_refusal(error: &anyhow::Error) -> Option<TrainerRefusal> {
    reducer_refusal_reason(error).and_then(TrainerRefusal::parse_tag)
}

#[cfg(test)]
mod trainer_reducer_tests {
    use super::*;
    use crate::stdb::connection::ReducerCallError;
    use anyhow::anyhow;

    #[test]
    fn only_a_rejected_reducer_carries_a_typed_refusal() {
        for refusal in TrainerRefusal::ALL {
            let error = anyhow::Error::from(ReducerCallError::Rejected {
                operation: "gw_trainer_buy".to_string(),
                reason: refusal.as_tag().to_string(),
            })
            .context("trainer buy");
            assert_eq!(trainer_refusal(&error), Some(refusal));
        }

        let not_refusals = [
            anyhow::Error::from(ReducerCallError::fatal(
                "gw_trainer_buy reducer timed out after 10s".to_string(),
            )),
            anyhow::Error::from(ReducerCallError::Rejected {
                operation: "gw_trainer_buy".to_string(),
                reason: "operator only".to_string(),
            }),
            anyhow!(
                "wrapped text that mentions {}",
                TrainerRefusal::AlreadyKnown.as_tag()
            ),
        ];
        for error in not_refusals {
            assert_eq!(trainer_refusal(&error), None, "{error:#}");
        }
    }
}
