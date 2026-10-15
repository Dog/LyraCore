use anyhow::{anyhow, Result};
use lyracore_shared::guild::GuildRefusal;

use super::{next_operation_id, wait_for_cache_row};
use crate::stdb::bindings::*;
use crate::stdb::connection::{call_reducer, reducer_refusal_reason};
use crate::stdb::Coordinator;
use crate::world::guild_fee;
use crate::world::guild_fee::{FeeHold, FeeOutcome, FeeRequest, FeeTerms, GuildFeeStore};
use crate::world::Actor;

impl GuildFeeStore for Coordinator {
    fn guild_fee_held(&self, actor: Actor) -> Result<Option<FeeHold>> {
        Ok(self.guild_fee_hold_row(actor))
    }

    fn guild_fee_hold(
        &self,
        actor: Actor,
        request: FeeRequest,
    ) -> Result<Result<FeeHold, GuildRefusal>> {
        let operation_id = next_operation_id()?;
        let request = match request {
            FeeRequest::Emblem { npc_guid, emblem } => {
                GuildFeeRequest::Emblem(GuildEmblemPurchase {
                    npc_guid,
                    emblem: guild_emblem(emblem),
                })
            }
            FeeRequest::Charter { npc_guid, name } => {
                GuildFeeRequest::Charter(GuildCharterPurchase { npc_guid, name })
            }
        };
        let result = call_reducer!(
            self.0.call_pipe().conn.reducers,
            "gw_guild_fee_hold",
            gw_guild_fee_hold_then(operation_id, self.session_actor(actor), request)
        );
        match result {
            Ok(()) => wait_for_cache_row(operation_id, "guild Fee Hold", || {
                self.guild_fee_hold_row(actor)
                    .filter(|hold| hold.operation_id == operation_id)
            })
            .map(Ok),
            Err(error) => match reducer_refusal_reason(&error).and_then(GuildRefusal::parse_tag) {
                Some(refusal) => Ok(Err(refusal)),
                None => Err(error),
            },
        }
    }

    fn guild_fee_decide(&self, actor: Actor, hold: FeeHold) -> Result<FeeOutcome> {
        let terms = match hold.terms {
            FeeTerms::Emblem(emblem) => GuildFeeTerms::Emblem(guild_emblem(emblem)),
            FeeTerms::Charter {
                charter_item_guid,
                name,
            } => {
                let facts = crate::stdb::Coordinator::guild_character_facts(self, actor.guid())?
                    .ok_or_else(|| anyhow!("Guild Charter owner {} is unreadable", actor.guid()))?;
                GuildFeeTerms::Charter(GuildCharterTerms {
                    charter_item_guid,
                    name,
                    payer_name: facts.name,
                    payer_team: lyracore_shared::faction::team_for_race(facts.race),
                })
            }
        };
        let realm = self.realm_core()?;
        call_reducer!(
            realm.0.call_pipe().conn.reducers,
            "realm_guild_fee_decide",
            realm_guild_fee_decide_then(hold.operation_id, realm.session_actor(actor), terms)
        )?;
        let decision = wait_for_cache_row(hold.operation_id, "guild fee decision", || {
            realm
                .0
                .coord()
                .conn
                .db
                .game_guild_fee_decision()
                .operation_id()
                .find(&hold.operation_id)
        })?;
        fee_outcome(&decision)
    }

    fn guild_fee_finish(&self, actor: Actor, operation_id: u64, accepted: bool) -> Result<()> {
        call_reducer!(
            self.0.call_pipe().conn.reducers,
            "gw_guild_fee_finish",
            gw_guild_fee_finish_then(operation_id, self.session_actor(actor), accepted)
        )
    }
}

impl Coordinator {
    fn guild_fee_hold_row(&self, payer: Actor) -> Option<FeeHold> {
        let hold = self
            .0
            .coord()
            .conn
            .db
            .game_guild_fee_hold()
            .payer_guid()
            .find(&payer.guid())?;
        let terms = match hold.kind {
            lyracore_shared::guild::fee_kind::EMBLEM => FeeTerms::Emblem(guild_fee::Emblem {
                emblem_style: hold.emblem_style,
                emblem_color: hold.emblem_color,
                border_style: hold.border_style,
                border_color: hold.border_color,
                background_color: hold.background_color,
            }),
            lyracore_shared::guild::fee_kind::CHARTER => FeeTerms::Charter {
                charter_item_guid: hold.charter_item_guid,
                name: hold.charter_name,
            },
            _ => return None,
        };
        Some(FeeHold {
            operation_id: hold.operation_id,
            terms,
        })
    }
}

fn guild_emblem(emblem: guild_fee::Emblem) -> GuildEmblem {
    GuildEmblem {
        emblem_style: emblem.emblem_style,
        emblem_color: emblem.emblem_color,
        border_style: emblem.border_style,
        border_color: emblem.border_color,
        background_color: emblem.background_color,
    }
}

fn fee_outcome(decision: &GuildFeeDecision) -> Result<guild_fee::FeeOutcome> {
    if decision.accepted {
        return Ok(guild_fee::FeeOutcome::Accepted);
    }
    GuildRefusal::parse_tag(&decision.refusal)
        .map(guild_fee::FeeOutcome::Refused)
        .ok_or_else(|| {
            anyhow!(
                "guild fee decision {} carries an unknown refusal {:?}",
                decision.operation_id,
                decision.refusal
            )
        })
}

#[cfg(test)]
mod guild_fee_reducer_tests {
    use super::*;

    fn decision(accepted: bool, refusal: &str) -> GuildFeeDecision {
        GuildFeeDecision {
            operation_id: 9,
            payer_guid: 5_090_401,
            kind: lyracore_shared::guild::fee_kind::EMBLEM,
            accepted,
            refusal: refusal.into(),
            petition_id: 0,
            decided_micros: 0,
        }
    }

    #[test]
    fn a_decision_row_maps_to_its_fee_outcome() {
        assert_eq!(
            fee_outcome(&decision(true, "")).unwrap(),
            guild_fee::FeeOutcome::Accepted
        );
        assert_eq!(
            fee_outcome(&decision(false, "guild:not_leader")).unwrap(),
            guild_fee::FeeOutcome::Refused(GuildRefusal::NotLeader)
        );
        assert!(
            fee_outcome(&decision(false, "guild:from_a_newer_module")).is_err(),
            "an unknown refusal must not read as a spend or a refund"
        );
    }
}
