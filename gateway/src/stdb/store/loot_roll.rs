use anyhow::Result;
use lyracore_shared::loot::LootRefusal;
use spacetimedb_sdk::Table;

use crate::stdb::bindings::*;
use crate::stdb::connection::call_reducer;
use crate::stdb::{classify, Coordinator, DurableFailure};
use crate::world::loot::{LootRollStore, PendingLootRoll};
use crate::world::{Actor, LootActionStatus};

impl LootRollStore for Coordinator {
    #[allow(clippy::too_many_arguments)]
    fn realm_loot_start(
        &self,
        corpse_guid: u64,
        slot: u8,
        item_entry: u32,
        deadline_micros: i64,
        recipients: Vec<u64>,
        random_property_id: u32,
        promotion_source: spacetimedb_sdk::Identity,
        source_roll_id: u64,
    ) -> Result<()> {
        call_reducer!(
            self.0.call_pipe().conn.reducers,
            "realm_loot_op",
            realm_loot_op_then(
                lyracore_shared::loot_roll::loot_op::START,
                corpse_guid,
                slot,
                item_entry,
                self.owner_actor(),
                0,
                deadline_micros,
                recipients,
                random_property_id,
                promotion_source,
                source_roll_id
            )
        )
    }

    fn realm_loot_vote(
        &self,
        corpse_guid: u64,
        slot: u8,
        actor: Actor,
        vote: u8,
    ) -> Result<LootActionStatus> {
        loot_action_status(call_reducer!(
            self.0.call_pipe().conn.reducers,
            "realm_loot_op",
            realm_loot_op_then(
                lyracore_shared::loot_roll::loot_op::VOTE,
                corpse_guid,
                slot,
                0,
                self.session_actor(actor),
                vote,
                0,
                Vec::new(),
                0,
                spacetimedb_sdk::Identity::ZERO,
                0
            )
        ))
    }

    fn pending_local_rolls(&self) -> Result<Vec<PendingLootRoll>> {
        let guard = self.0.coord();
        let db = &guard.conn.db;
        Ok(db
            .game_loot_roll()
            .iter()
            .filter(|r| !r.resolved)
            .map(|r| {
                let recipients = db
                    .game_loot_roll_vote()
                    .iter()
                    .filter(|v| v.roll_id == r.id)
                    .map(|v| v.voter_guid)
                    .collect();
                PendingLootRoll {
                    roll_id: r.id,
                    corpse_guid: r.corpse_guid,
                    slot: r.slot,
                    item_entry: r.item_entry,
                    deadline_micros: r.deadline_micros,
                    recipients,
                    random_property_id: r.random_property_id,
                    promotion_source: r.promotion_source,
                }
            })
            .collect())
    }

    fn settle_loot_roll(&self, corpse_guid: u64, slot: u8, winner_guid: u64) -> Result<()> {
        call_reducer!(
            self.0.call_pipe().conn.reducers,
            "settle_loot_roll",
            settle_loot_roll_then(corpse_guid, slot, winner_guid, self.owner_actor())
        )
    }

    fn clear_promoted_loot_roll(&self, roll_id: u64) -> Result<()> {
        call_reducer!(
            self.0.call_pipe().conn.reducers,
            "clear_promoted_loot_roll",
            clear_promoted_loot_roll_then(roll_id, self.owner_actor())
        )
    }

    fn loot_won_since(&self, after_id: u64) -> Result<(u64, Vec<(u64, u8, u64)>)> {
        use lyracore_shared::loot_roll::event_kind as roll_kind;
        let guard = self.0.coord();
        let db = &guard.conn.db;
        let mut watermark = after_id;
        let mut wins = Vec::new();
        for row in db.game_group_event().iter() {
            if row.id <= after_id || row.kind != roll_kind::ROLL_WON {
                continue;
            }
            watermark = watermark.max(row.id);
            match lyracore_shared::loot_roll::decode_won(&row.payload) {
                Some((corpse_guid, slot, ..)) => wins.push((corpse_guid, slot, row.other_guid)),
                None => log::warn!(
                    "loot-roll relay: unparseable ROLL_WON payload {:?} (event {})",
                    row.payload,
                    row.id
                ),
            }
        }
        Ok((watermark, wins))
    }

    fn loot_roll(
        &self,
        actor: Actor,
        corpse_guid: u64,
        loot_slot: u32,
        vote: u8,
    ) -> Result<LootActionStatus> {
        let coord = self.0.call_pipe();
        loot_action_status(call_reducer!(
            coord.conn.reducers,
            "gw_loot_roll",
            gw_loot_roll_then(self.session_actor(actor), corpse_guid, loot_slot, vote)
        ))
    }

    fn loot_master_give(
        &self,
        actor: Actor,
        corpse_guid: u64,
        loot_slot: u8,
        target_guid: u64,
    ) -> Result<LootActionStatus> {
        let coord = self.0.call_pipe();
        loot_action_status(call_reducer!(
            coord.conn.reducers,
            "gw_loot_master_give",
            gw_loot_master_give_then(
                self.session_actor(actor),
                corpse_guid,
                loot_slot,
                target_guid
            )
        ))
    }
}

fn loot_action_status(result: Result<()>) -> Result<LootActionStatus> {
    match result {
        Ok(()) => Ok(LootActionStatus::Applied),
        Err(error) => match classify(&error) {
            DurableFailure::Refusal { reason } => match LootRefusal::parse_tag(reason) {
                Some(refusal) => {
                    log::debug!("stdb: loot action refused: {error:#}");
                    Ok(LootActionStatus::Refused(refusal))
                }
                None => Err(error),
            },
            DurableFailure::TransportLoss => Err(error),
        },
    }
}

#[cfg(test)]
mod loot_reducer_tests {
    use super::*;
    use crate::stdb::connection::ReducerCallError;

    fn refused(reason: &str) -> Result<()> {
        Err(
            anyhow::Error::from(ReducerCallError::refused("gw_loot_roll", reason))
                .context("loot roll"),
        )
    }

    #[test]
    fn a_known_refusal_tag_is_an_answer() {
        for refusal in [
            LootRefusal::RollUnavailable,
            LootRefusal::NotMasterLooter,
            LootRefusal::RecipientUnavailable,
            LootRefusal::RecipientInventoryFull,
        ] {
            assert_eq!(
                loot_action_status(refused(refusal.as_tag())).unwrap(),
                LootActionStatus::Refused(refusal)
            );
        }
    }

    #[test]
    fn an_unknown_refusal_tag_and_a_transport_loss_stay_errors() {
        for result in [
            refused("loot:newer_module_refusal"),
            Err(ReducerCallError::transport_lost("gw_loot_roll").into()),
        ] {
            assert!(loot_action_status(result).is_err());
        }
    }
}
