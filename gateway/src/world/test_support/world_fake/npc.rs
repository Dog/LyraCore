use super::super::*;
use crate::stdb::ReducerCallError;

#[derive(Default)]
pub(crate) struct NpcState {
    pub(crate) innkeeper: bool,
    pub(crate) home_bound: std::sync::atomic::AtomicBool,
    pub(crate) gossip_opts: Vec<codec::GossipOptionView>,
    pub(crate) gossip_source_menu_id: std::sync::atomic::AtomicU32,
    pub(crate) gossip_profiles: std::collections::HashMap<u32, Vec<codec::GossipOptionView>>,
    pub(crate) gossip_selects: std::sync::Mutex<Vec<(u32, u32)>>,
    pub(crate) npc_text_view: Option<codec::NpcTextView>,
    pub(crate) gameobject_type: Option<u8>,
    pub(crate) inspect_error: Option<fn() -> anyhow::Error>,
    pub(crate) gossip_conditions:
        std::collections::HashMap<u32, lyracore_shared::gossip::Condition>,
    pub(crate) gossip_select_error: Option<fn() -> anyhow::Error>,
}

impl NpcStore for WorldFake {
    fn creature_template(&self, _entry: u32) -> Result<Option<codec::CreatureView>> {
        Ok(None)
    }

    fn pet_name(
        &self,
        _requester: Actor,
        _pet_number: u32,
        _pet_guid: u64,
    ) -> Result<Option<codec::PetNameView>> {
        Ok(None)
    }

    fn gameobject_template(&self, _entry: u32) -> Result<Option<codec::GameObjectTemplateView>> {
        Ok(None)
    }

    fn gameobject_type(&self, _go_guid: u64) -> Result<Option<u8>> {
        Ok(self.npc.gameobject_type)
    }

    fn enter_areatrigger(&self, _actor: Actor, _trigger_id: u32) -> Result<()> {
        Ok(())
    }

    fn npc_refuses_interaction(&self, _npc_guid: u64, _player_guid: u64) -> Result<bool> {
        Ok(self.npc_refuses)
    }

    fn bind_home(&self, _actor: Actor, _innkeeper_guid: u64) -> Result<InteractionOutcome> {
        if !self.npc.innkeeper {
            return Ok(InteractionOutcome::Refused(
                "target is not an innkeeper".into(),
            ));
        }
        self.npc
            .home_bound
            .store(true, std::sync::atomic::Ordering::SeqCst);
        Ok(InteractionOutcome::Done)
    }

    fn npc_is_innkeeper(&self, _guid: u64) -> Result<bool> {
        Ok(self.npc.innkeeper)
    }

    fn npc_gossip_text_id(&self, _npc_guid: u64) -> u32 {
        1
    }

    fn npc_text_for_id(&self, _text_id: u32) -> Option<codec::NpcTextView> {
        self.npc.npc_text_view.clone()
    }

    fn gossip_options(&self, _npc_guid: u64) -> Result<codec::GossipMenuView> {
        let source_menu_id = self
            .npc
            .gossip_source_menu_id
            .load(std::sync::atomic::Ordering::SeqCst);
        let options = if source_menu_id == 0 {
            self.npc.gossip_opts.clone()
        } else {
            self.npc
                .gossip_profiles
                .get(&source_menu_id)
                .cloned()
                .unwrap_or_default()
        };
        Ok(codec::GossipMenuView {
            source_menu_id,
            options,
        })
    }

    fn gossip_option_available(
        &self,
        character_guid: u64,
        option: &codec::GossipOptionView,
    ) -> Result<bool> {
        use lyracore_shared::gossip::{evaluate_option, CharacterFacts, QuestState};
        let Some(character) = self.character_by_guid(character_guid)? else {
            return Ok(false);
        };
        Ok(evaluate_option(
            option.cond_type,
            option.cond_value1,
            option.cond_value2,
            CharacterFacts {
                level: u32::from(character.level),
                race: character.race,
                class: character.class,
            },
            |quest_id| match self.quest_status(character_guid, quest_id) {
                _ if self.quest.missing_quests.contains(&quest_id) => QuestState::Missing,
                (_, true) if self.quest.repeatable_quests.contains(&quest_id) => QuestState::Absent,
                (_, true) => QuestState::Rewarded,
                (true, false)
                    if self
                        .quest
                        .quest_log_slots
                        .iter()
                        .any(|slot| slot.quest_id == quest_id && slot.state == 2) =>
                {
                    QuestState::Failed
                }
                (true, false)
                    if self
                        .quest
                        .quest_evals
                        .iter()
                        .any(|quest| quest.quest_id == quest_id && quest.complete)
                        || self
                            .quest
                            .quest_log_slots
                            .iter()
                            .any(|slot| slot.quest_id == quest_id && slot.state == 1) =>
                {
                    QuestState::Complete
                }
                (true, false) => QuestState::Incomplete,
                _ => QuestState::Absent,
            },
            |id| self.npc.gossip_conditions.get(&id).copied(),
        ) == Ok(true))
    }

    fn inspect(&self, _actor: Actor, target_guid: u64) -> Result<()> {
        if let Some(error) = self.npc.inspect_error {
            return Err(error());
        }
        if let Some(reason) = &self.trade_error {
            return Err(ReducerCallError::refused("gw_inspect", reason).into());
        }
        if target_guid == 0 {
            return Err(ReducerCallError::refused("gw_inspect", "no such inspect target").into());
        }
        Ok(())
    }

    fn gossip_select(
        &self,
        actor: Actor,
        npc_guid: u64,
        option_id: u32,
        option_row_id: u32,
        source_menu_id: u32,
        selected_action: u32,
    ) -> Result<()> {
        if let Some(error) = self.npc.gossip_select_error {
            return Err(error());
        }
        use lyracore_shared::constants::{gossip_option, MIN_TALENT_LEVEL};
        let service_available = match selected_action {
            gossip_option::VENDOR => !self.vendor_stock(npc_guid)?.is_empty(),
            gossip_option::INNKEEPER => self.npc_is_innkeeper(npc_guid)?,
            gossip_option::TRAINER | gossip_option::UNLEARNTALENTS => {
                self.trainer_serves(actor.guid(), npc_guid)?
                    && (selected_action != gossip_option::UNLEARNTALENTS
                        || self
                            .character_by_guid(actor.guid())?
                            .is_some_and(|character| character.level >= MIN_TALENT_LEVEL))
            }
            _ => true,
        };
        if !service_available {
            return Err(ReducerCallError::refused(
                "gw_gossip_select",
                "gossip service is no longer available",
            )
            .into());
        }
        let menu = self.gossip_options(npc_guid)?;
        if menu.source_menu_id != source_menu_id {
            return Err(
                ReducerCallError::refused("gw_gossip_select", "gossip menu has changed").into(),
            );
        }
        if option_row_id != codec::SYNTHESIZED_ROW_ID {
            let option = menu
                .options
                .iter()
                .find(|option| option.row_id == option_row_id)
                .ok_or_else(|| {
                    ReducerCallError::refused("gw_gossip_select", "no such gossip option")
                })?;
            if option.action != selected_action {
                return Err(ReducerCallError::refused(
                    "gw_gossip_select",
                    "gossip option action has changed",
                )
                .into());
            }
            if !self.gossip_option_available(actor.guid(), option)? {
                return Err(ReducerCallError::refused(
                    "gw_gossip_select",
                    "gossip option is no longer available",
                )
                .into());
            }
        } else {
            if !matches!(
                selected_action,
                gossip_option::VENDOR | gossip_option::INNKEEPER
            ) || menu
                .options
                .iter()
                .any(|option| option.action == selected_action)
            {
                return Err(ReducerCallError::refused(
                    "gw_gossip_select",
                    "gossip service is no longer available",
                )
                .into());
            }
        }
        self.npc
            .gossip_selects
            .lock()
            .unwrap()
            .push((option_id, option_row_id));
        Ok(())
    }
}
