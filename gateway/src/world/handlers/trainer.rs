use super::super::*;
use crate::stdb::{classify, DurableFailure};
use lyracore_shared::trainer::TrainerRefusal;
use wow_world_messages::vanilla::TrainingFailureReason;

pub(crate) trait TrainerStore: Send + Sync {
    fn set_watched_faction(
        &self,
        actor: Actor,
        reputation_index: i32,
    ) -> Result<InteractionOutcome>;

    fn trainer_serves(&self, player_guid: u64, trainer_guid: u64) -> Result<bool>;

    fn trainer_list(
        &self,
        player_guid: u64,
        trainer_guid: u64,
    ) -> Result<Vec<codec::TrainerSpellView>>;

    fn buy_trainer_spell(
        &self,
        actor: Actor,
        trainer_guid: u64,
        spell_id: u32,
    ) -> Result<TrainerBuyOutcome>;

    fn trainer_offer_skill_line(&self, trainer_guid: u64, spell_id: u32) -> u32;

    fn talent_grant_spell(&self, talent_id: u32) -> u32;

    fn set_action_button(
        &self,
        actor: Actor,
        button: u8,
        action: u32,
        action_type: u8,
    ) -> Result<()>;

    fn set_faction_at_war(
        &self,
        actor: Actor,
        reputation_index: u32,
        at_war: bool,
    ) -> Result<InteractionOutcome>;

    fn talent_pane_sync(&self, character_guid: u64, talent_id: u32) -> (u32, u32, u32);

    fn talent_points_spent(&self, character_guid: u64) -> u32;

    fn learn_talent(&self, actor: Actor, talent_id: u32) -> Result<()>;

    fn talent_reset_cost(&self, character_guid: u64) -> Option<u32>;

    fn reset_talents(&self, actor: Actor, trainer_guid: u64) -> Result<InteractionOutcome>;

    fn resolve_learn_target(&self, spell_id: u32) -> u32;

    fn superseded_old_rank(&self, new_spell: u32, player_guid: u64) -> Option<u32>;

    fn character_presence(&self, guid: u64) -> Result<Option<(bool, u8, u8, u32)>>;
}

pub(super) fn settle_per_action(what: &str, account_id: u64, result: Result<()>) -> Result<()> {
    match result {
        Ok(()) => Ok(()),
        Err(error) => match classify(&error) {
            DurableFailure::Refusal { reason } => {
                log::debug!("world: {what} ignored (account {account_id}): {reason}");
                Ok(())
            }
            DurableFailure::TransportLoss => Err(error),
        },
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TrainerBuyOutcome {
    Learned,
    Refused(TrainingFailureReason),
}

impl From<TrainerRefusal> for TrainerBuyOutcome {
    fn from(refusal: TrainerRefusal) -> Self {
        Self::Refused(match refusal {
            TrainerRefusal::NotEnoughMoney => TrainingFailureReason::NotEnoughMoney,
            TrainerRefusal::LevelTooLow | TrainerRefusal::PreviousRankMissing => {
                TrainingFailureReason::NotEnoughSkill
            }
            TrainerRefusal::Unavailable
            | TrainerRefusal::NotOffered
            | TrainerRefusal::AlreadyKnown => TrainingFailureReason::Unavailable,
        })
    }
}

pub(crate) struct Trainer;

impl<St: CharacterStore + NpcStore + TrainerStore + ?Sized> ProtocolFamily<St> for Trainer {
    fn handle(
        store: &St,
        conn: &mut ProtocolSession,
        request: ProtocolRequest,
    ) -> Result<ProtocolReply> {
        let msg = match request {
            ProtocolRequest::FactionAtWar(body) => return faction_at_war(store, conn, &body),
            ProtocolRequest::WatchedFaction(body) => return watched_faction(store, conn, &body),
            other => other.message()?,
        };
        let mut outbound = Vec::new();
        let Some(actor) = (match &conn.state {
            WorldState::InWorld(iw) => Actor::new(iw.self_guid),
            WorldState::CharSelect => None,
        }) else {
            return Ok(outbound.into());
        };
        let self_guid = actor.guid();
        match msg {
            ClientOpcodeMessage::CMSG_TRAINER_LIST(c) => {
                let trainer_guid = c.guid.guid();
                if store
                    .npc_refuses_interaction(trainer_guid, self_guid)
                    .unwrap_or(false)
                {
                    return Ok(outbound.into());
                }
                if !store
                    .trainer_serves(self_guid, trainer_guid)
                    .unwrap_or(true)
                {
                    return Ok(outbound.into());
                }
                let spells = store.trainer_list(self_guid, trainer_guid)?;
                let list = codec::build_trainer_list(
                    trainer_guid,
                    &spells,
                    "I can teach you a thing or two.",
                );
                outbound.push(Outbound::One(ServerOpcodeMessage::SMSG_TRAINER_LIST(
                    Box::new(list),
                )));
            }
            ClientOpcodeMessage::CMSG_TRAINER_BUY_SPELL(c) => {
                let trainer_guid = c.guid.guid();
                let spell_id = c.id;
                match store.buy_trainer_spell(actor, trainer_guid, spell_id)? {
                    TrainerBuyOutcome::Learned => {
                        outbound.push(Outbound::One(
                            ServerOpcodeMessage::SMSG_TRAINER_BUY_SUCCEEDED(Box::new(
                                codec::build_trainer_buy_succeeded(trainer_guid, spell_id),
                            )),
                        ));
                        // Riding ids lack Spell.dbc rows; do not send them as learned spells.
                        if store.trainer_offer_skill_line(trainer_guid, spell_id)
                            == lyracore_shared::trainer::RIDING_SKILL_LINE
                        {
                            return Ok(outbound.into());
                        }
                        let resolved = store.resolve_learn_target(spell_id);
                        match store.superseded_old_rank(resolved, self_guid) {
                            Some(old_rank) => {
                                use wow_world_messages::vanilla::SMSG_SUPERCEDED_SPELL;
                                outbound.push(Outbound::One(
                                    ServerOpcodeMessage::SMSG_SUPERCEDED_SPELL(
                                        SMSG_SUPERCEDED_SPELL {
                                            new_spell_id: old_rank as u16,
                                            old_spell_id: resolved as u16,
                                        },
                                    ),
                                ));
                            }
                            None => {
                                outbound.push(Outbound::One(
                                    ServerOpcodeMessage::SMSG_LEARNED_SPELL(
                                        codec::build_learned_spell(resolved),
                                    ),
                                ));
                            }
                        }
                        if teaches_armor_proficiency(spell_id)
                            || teaches_armor_proficiency(resolved)
                        {
                            append_proficiency(
                                &mut outbound,
                                store,
                                self_guid,
                                codec::build_armor_proficiency_msg,
                            );
                        }
                        if lyracore_shared::item::is_weapon_proficiency_spell(resolved) {
                            append_proficiency(
                                &mut outbound,
                                store,
                                self_guid,
                                codec::build_weapon_proficiency_msg,
                            );
                        }
                    }
                    TrainerBuyOutcome::Refused(reason) => {
                        log::debug!(
                            "world: trainer buy refused (account {}): {reason:?}",
                            conn.account_id
                        );
                        outbound.push(Outbound::One(ServerOpcodeMessage::SMSG_TRAINER_BUY_FAILED(
                            Box::new(codec::build_trainer_buy_failed(
                                trainer_guid,
                                spell_id,
                                reason,
                            )),
                        )));
                    }
                }
            }
            ClientOpcodeMessage::CMSG_SET_ACTION_BUTTON(c) => {
                let action = c.action as u32 | ((c.misc as u32) << 16);
                settle_per_action(
                    "set_action_button",
                    conn.account_id,
                    store.set_action_button(actor, c.button, action, c.action_type),
                )?;
            }
            ClientOpcodeMessage::CMSG_LEARN_TALENT(c) => {
                let talent_id = c.talent.as_int();
                let grant_spell_id = store.talent_grant_spell(talent_id);
                match store.learn_talent(actor, talent_id) {
                    Ok(()) => {
                        if grant_spell_id != 0 {
                            outbound.push(Outbound::One(ServerOpcodeMessage::SMSG_LEARNED_SPELL(
                                codec::build_learned_spell(grant_spell_id),
                            )));
                        }

                        let (teach, superseded, remaining) =
                            store.talent_pane_sync(self_guid, talent_id);
                        if teach != 0 && teach != grant_spell_id {
                            if superseded != 0 {
                                use wow_world_messages::vanilla::SMSG_SUPERCEDED_SPELL;
                                outbound.push(Outbound::One(
                                    ServerOpcodeMessage::SMSG_SUPERCEDED_SPELL(
                                        SMSG_SUPERCEDED_SPELL {
                                            // cmangos wire order puts the old spell first.
                                            new_spell_id: superseded as u16,
                                            old_spell_id: teach as u16,
                                        },
                                    ),
                                ));
                            } else {
                                outbound.push(Outbound::One(
                                    ServerOpcodeMessage::SMSG_LEARNED_SPELL(
                                        codec::build_learned_spell(teach),
                                    ),
                                ));
                            }
                        }
                        outbound.push(Outbound::One(ServerOpcodeMessage::SMSG_UPDATE_OBJECT(
                            Box::new(codec::build_talent_points_values(self_guid, remaining)),
                        )));
                        for m in codec::build_spell_modifier_msgs(&store.spell_modifiers(self_guid))
                        {
                            outbound.push(Outbound::One(m));
                        }
                    }
                    Err(error) => settle_per_action("learn_talent", conn.account_id, Err(error))?,
                }
            }
            _ => return Err(anyhow!("opcode routed to the wrong Protocol Family")),
        }
        Ok(outbound.into())
    }
}

fn teaches_armor_proficiency(spell_id: u32) -> bool {
    use lyracore_shared::constants::armor_proficiency::*;

    matches!(
        spell_id,
        PLATE_TRAINER_SPELL_ID
            | PLATE_PASSIVE_SPELL_ID
            | MAIL_TRAINER_SPELL_ID
            | MAIL_PASSIVE_SPELL_ID
    )
}

fn append_proficiency<St: CharacterStore + TrainerStore + ?Sized>(
    outbound: &mut Vec<Outbound>,
    store: &St,
    self_guid: u64,
    build: fn(u8, &[u32]) -> ServerOpcodeMessage,
) {
    let learned = store.player_learned_spells(self_guid).unwrap_or_default();
    if let Some((_, _, player_class, _)) = store.character_presence(self_guid).ok().flatten() {
        outbound.push(Outbound::One(build(player_class, &learned)));
    }
}

fn faction_at_war<St: TrainerStore + ?Sized>(
    store: &St,
    session: &ProtocolSession,
    body: &[u8],
) -> Result<ProtocolReply> {
    anyhow::ensure!(
        body.len() == 5 && body[4] <= 1,
        "invalid CMSG_SET_FACTION_ATWAR body"
    );
    let Some(actor) = session.actor() else {
        return Ok(ProtocolReply::default());
    };
    let index = u32::from_le_bytes(body[..4].try_into()?);
    let mut outbound = Vec::new();
    if let InteractionOutcome::Refused(reason) =
        store.set_faction_at_war(actor, index, body[4] != 0)?
    {
        outbound.push(Outbound::One(ServerOpcodeMessage::SMSG_MESSAGECHAT(
            Box::new(codec::build_gm_system_message(reason)),
        )));
    }
    Ok(outbound.into())
}

fn watched_faction<St: TrainerStore + ?Sized>(
    store: &St,
    session: &ProtocolSession,
    body: &[u8],
) -> Result<ProtocolReply> {
    anyhow::ensure!(body.len() == 4, "invalid CMSG_SET_WATCHED_FACTION body");
    let Some(actor) = session.actor() else {
        return Ok(ProtocolReply::default());
    };
    let index = i32::from_le_bytes(body.try_into()?);
    let message = match store.set_watched_faction(actor, index)? {
        InteractionOutcome::Done => ServerOpcodeMessage::SMSG_UPDATE_OBJECT(Box::new(
            codec::build_watched_faction_values(actor.guid(), index),
        )),
        InteractionOutcome::Refused(reason) => {
            ServerOpcodeMessage::SMSG_MESSAGECHAT(Box::new(codec::build_gm_system_message(reason)))
        }
    };
    Ok(vec![Outbound::One(message)].into())
}
