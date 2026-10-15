use anyhow::Result;
use spacetimedb_sdk::Table;

use crate::stdb::bindings::*;
use crate::stdb::connection::call_reducer;
use crate::stdb::reads::{build_quest_log_slots, quest_objectives_complete};
use crate::stdb::Coordinator;
use crate::world::{Actor, QuestActionStore};

impl QuestActionStore for Coordinator {
    fn item_start_quest(&self, owner_guid: u64, slot: u8) -> Option<(u64, u32)> {
        let guard = self.0.coord();
        let db = &guard.conn.db;
        let hit: Option<(u64, u32)> = db
            .game_item_instance()
            .iter()
            .filter(|i| i.owner_guid == owner_guid && i.slot == slot)
            .find_map(|i| {
                db.game_item_template()
                    .entry()
                    .find(&i.entry)
                    .filter(|t| t.start_quest != 0)
                    .map(|t| (i.guid, t.start_quest))
            });
        hit
    }

    fn quest_status(&self, guid: u64, quest_id: u32) -> (bool, bool) {
        let guard = self.0.coord();
        let db = &guard.conn.db;
        let found = db
            .game_character_quest()
            .iter()
            .find(|q| q.character_guid == guid && q.quest_entry == quest_id)
            .map(|row| row.rewarded);
        match found {
            Some(rewarded) => (true, rewarded),
            None => (false, false),
        }
    }

    fn giver_quest_evals(
        &self,
        giver_guid: u64,
        player_guid: u64,
    ) -> Result<Vec<crate::codec::GiverQuestEval>> {
        let guard = self.0.coord();
        let db = &guard.conn.db;
        enum Giver {
            Creature(u32),
            GameObject(u32),
        }
        let giver = if let Some(e) = db.game_world_entity().guid().find(&giver_guid) {
            Giver::Creature(e.entry)
        } else if let Some(go) = db.game_gameobject().guid().find(&giver_guid) {
            Giver::GameObject(go.template_entry)
        } else {
            return Ok(Vec::new());
        };
        let Some(player) = db.game_world_entity().guid().find(&player_guid) else {
            return Ok(Vec::new());
        };
        if player.dead {
            return Ok(Vec::new());
        }
        let player_level = player.level;
        let race = (player.unit_bytes_0 & 0xFF) as u8;
        let class = ((player.unit_bytes_0 >> 8) & 0xFF) as u8;
        let log: Vec<_> = db
            .game_character_quest()
            .iter()
            .filter(|q| q.character_guid == player_guid)
            .collect();
        let evals = match giver {
            Giver::Creature(entry) => db
                .game_creature_quest()
                .iter()
                .filter(|r| r.creature_entry == entry)
                .filter_map(|rel| {
                    eval_relation(
                        db,
                        rel.quest_entry,
                        rel.role,
                        &log,
                        player_guid,
                        player_level,
                        race,
                        class,
                    )
                })
                .collect(),
            Giver::GameObject(entry) => db
                .game_gameobject_quest()
                .iter()
                .filter(|r| r.go_entry == entry)
                .filter_map(|rel| {
                    eval_relation(
                        db,
                        rel.quest_entry,
                        rel.role,
                        &log,
                        player_guid,
                        player_level,
                        race,
                        class,
                    )
                })
                .collect(),
        };
        Ok(evals)
    }

    fn quest_detail_view(&self, quest_id: u32) -> Result<Option<crate::codec::QuestDetailView>> {
        let guard = self.0.coord();
        Ok(quest_detail_view(&guard.conn.db, quest_id))
    }

    fn giver_refuses_interaction(&self, giver_guid: u64, player_guid: u64) -> Result<bool> {
        Coordinator::npc_refuses_interaction(self, giver_guid, player_guid)
    }

    fn player_quest_log(
        &self,
        player_guid: u64,
    ) -> Result<Vec<crate::codec::update_mask::QuestLogSlot>> {
        let guard = self.0.coord();
        let db = &guard.conn.db;
        let quests: Vec<_> = db.game_character_quest().iter().collect();
        Ok(build_quest_log_slots(db, &quests, player_guid))
    }

    fn push_quest(&self, actor: Actor, quest_id: u32) -> Result<()> {
        let coord = self.0.call_pipe();
        call_reducer!(
            coord.conn.reducers,
            "gw_push_quest_to_party",
            gw_push_quest_to_party_then(self.session_actor(actor), quest_id)
        )
    }

    fn accept_quest(&self, actor: Actor, giver_guid: u64, quest_id: u32) -> Result<()> {
        let coord = self.0.call_pipe();
        call_reducer!(
            coord.conn.reducers,
            "gw_accept_quest",
            gw_accept_quest_then(self.session_actor(actor), giver_guid, quest_id)
        )
    }

    fn turn_in_quest(
        &self,
        actor: Actor,
        giver_guid: u64,
        quest_id: u32,
        reward_index: u32,
    ) -> Result<()> {
        let coord = self.0.visibility_pipe();
        call_reducer!(
            coord.conn.reducers,
            "gw_turn_in_quest",
            gw_turn_in_quest_then(
                self.session_actor(actor),
                giver_guid,
                quest_id,
                reward_index
            )
        )
    }

    fn abandon_quest(&self, actor: Actor, quest_id: u32) -> Result<()> {
        let coord = self.0.call_pipe();
        call_reducer!(
            coord.conn.reducers,
            "gw_abandon_quest",
            gw_abandon_quest_then(self.session_actor(actor), quest_id)
        )
    }
}

pub(crate) fn quest_detail_view(
    db: &RemoteTables,
    quest_id: u32,
) -> Option<crate::codec::QuestDetailView> {
    use crate::codec::QuestDetailView;
    let tmpl = db.game_quest_template().entry().find(&quest_id)?;
    let reward_xp = lyracore_shared::quest::base_reward_xp(
        tmpl.reward_xp,
        tmpl.reward_money_max_level,
        tmpl.quest_level,
    );
    let text = db.game_quest_text().quest_entry().find(&quest_id);
    let details = text.as_ref().map(|t| t.details.clone()).unwrap_or_default();
    let offer_reward_text = text
        .as_ref()
        .map(|t| t.offer_reward_text.clone())
        .unwrap_or_default();
    let request_items_text = text
        .as_ref()
        .map(|t| t.request_items_text.clone())
        .unwrap_or_default();
    let imported_objectives = text.map(|t| t.objectives).unwrap_or_default();
    let mut raw_objs: Vec<(u8, u32, u32, u32, u32)> = db
        .game_quest_objective()
        .iter()
        .filter(|o| o.quest_entry == quest_id)
        .filter_map(|o| {
            let slot = match o.kind {
                0 => (o.target_entry, o.required_count, 0u32, 0u32),
                1 => (0, 0, o.target_entry, o.required_count),
                2 => ((-(o.target_entry as i32)) as u32, o.required_count, 0, 0),
                _ => return None,
            };
            Some((o.obj_index, slot.0, slot.1, slot.2, slot.3))
        })
        .collect();
    raw_objs.sort_by_key(|(i, _, _, _, _)| *i);
    Some(QuestDetailView {
        quest_id,
        quest_level: tmpl.quest_level,
        zone_or_sort: tmpl.zone_or_sort,
        title: tmpl.title.clone(),
        details,
        objectives_text: synthesized_objectives(db, quest_id, imported_objectives),
        offer_reward_text,
        request_items_text,
        money_reward: tmpl.reward_money,
        reward_xp,
        next_quest_id: tmpl.next_quest_id,
        max_level_money_reward: lyracore_shared::quest::max_level_money_reward(reward_xp),
        rewards: quest_rewards(db, quest_id),
        choice_rewards: quest_choice_rewards(db, quest_id),
        objectives: raw_objs
            .into_iter()
            .map(|(_, a, b, c, d)| (a, b, c, d))
            .collect(),
    })
}

#[allow(clippy::too_many_arguments)]
fn eval_relation(
    db: &RemoteTables,
    quest_entry: u32,
    role: u8,
    log: &[CharacterQuest],
    player_guid: u64,
    player_level: u32,
    race: u8,
    class: u8,
) -> Option<crate::codec::GiverQuestEval> {
    use crate::codec::{GiverQuestEval, ROLE_START};
    let tmpl = db.game_quest_template().entry().find(&quest_entry)?;
    let logrow = log.iter().find(|q| q.quest_entry == quest_entry);
    let active = logrow.is_some_and(|q| !q.rewarded && !q.failed);
    let complete = active && quest_objectives_complete(db, quest_entry, logrow, player_guid);
    let startable = role == ROLE_START
        && lyracore_shared::quest::acceptance(
            lyracore_shared::quest::AcceptanceRequirements {
                min_level: tmpl.min_level,
                required_races: tmpl.required_races,
                required_classes: tmpl.required_classes,
                repeatable: tmpl.repeatable,
            },
            player_level,
            race,
            class,
            tmpl.prev_quest_id == 0
                || log
                    .iter()
                    .any(|q| q.quest_entry == tmpl.prev_quest_id && q.rewarded),
            logrow.map(|q| lyracore_shared::quest::Attempt {
                rewarded: q.rewarded,
                failed: q.failed,
            }),
        )
        .is_ok();
    Some(GiverQuestEval {
        quest_id: tmpl.entry,
        title: tmpl.title.clone(),
        level: tmpl.quest_level,
        role,
        startable,
        active,
        complete,
    })
}

fn quest_rewards(db: &RemoteTables, quest_id: u32) -> Vec<crate::codec::QuestRewardView> {
    db.game_quest_reward_item()
        .iter()
        .filter(|r| r.quest_entry == quest_id)
        .map(|r| {
            let display_id = db
                .game_item_template()
                .entry()
                .find(&r.item_entry)
                .map(|t| t.display_id)
                .unwrap_or(0);
            crate::codec::QuestRewardView {
                item_entry: r.item_entry,
                count: r.count,
                display_id,
            }
        })
        .collect()
}

fn quest_choice_rewards(db: &RemoteTables, quest_id: u32) -> Vec<crate::codec::QuestRewardView> {
    let mut rows: Vec<_> = db
        .game_quest_reward_choice()
        .iter()
        .filter(|r| r.quest_entry == quest_id)
        .collect();
    rows.sort_by_key(|r| r.choice_index);
    rows.into_iter()
        .map(|r| {
            let display_id = db
                .game_item_template()
                .entry()
                .find(&r.item_entry)
                .map(|t| t.display_id)
                .unwrap_or(0);
            crate::codec::QuestRewardView {
                item_entry: r.item_entry,
                count: r.count,
                display_id,
            }
        })
        .collect()
}

fn synthesized_objectives(db: &RemoteTables, quest_id: u32, imported: String) -> String {
    if !imported.trim().is_empty() {
        return imported;
    }
    let mut objs: Vec<(u8, String)> = db
        .game_quest_objective()
        .iter()
        .filter(|o| o.quest_entry == quest_id)
        .map(|o| {
            let text = match o.kind {
                0 => {
                    let name = db
                        .game_creature_template()
                        .entry()
                        .find(&o.target_entry)
                        .map(|c| c.name.clone())
                        .unwrap_or_default();
                    format!("{name} slain: 0/{}", o.required_count)
                }
                1 => {
                    let name = db
                        .game_item_template()
                        .entry()
                        .find(&o.target_entry)
                        .map(|t| t.name.clone())
                        .unwrap_or_default();
                    format!("{name}: 0/{}", o.required_count)
                }
                _ => String::new(),
            };
            (o.obj_index, text)
        })
        .filter(|(_, t)| !t.is_empty())
        .collect();
    objs.sort_by_key(|(i, _)| *i);
    objs.into_iter()
        .map(|(_, s)| s)
        .collect::<Vec<_>>()
        .join("\n")
}
