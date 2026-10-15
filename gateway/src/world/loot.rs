//! Before changing Loot Roll promotion or voting, read docs/realm-loot-routing.md.

use crate::world::ShardRoutingStore;
use anyhow::Result;

use super::{Actor, LootActionStatus, WorldStore};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingLootRoll {
    pub roll_id: u64,
    pub corpse_guid: u64,
    pub slot: u8,
    pub item_entry: u32,
    pub random_property_id: u32,
    pub deadline_micros: i64,
    pub recipients: Vec<u64>,
    pub promotion_source: spacetimedb_sdk::Identity,
}

pub(crate) trait LootRollStore: Send + Sync {
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
    ) -> Result<()>;

    fn realm_loot_vote(
        &self,
        corpse_guid: u64,
        slot: u8,
        actor: Actor,
        vote: u8,
    ) -> Result<LootActionStatus>;

    fn pending_local_rolls(&self) -> Result<Vec<PendingLootRoll>>;

    fn settle_loot_roll(&self, corpse_guid: u64, slot: u8, winner_guid: u64) -> Result<()>;

    fn clear_promoted_loot_roll(&self, roll_id: u64) -> Result<()>;

    #[allow(clippy::type_complexity)]
    fn loot_won_since(&self, after_id: u64) -> Result<(u64, Vec<(u64, u8, u64)>)>;

    fn loot_roll(
        &self,
        actor: Actor,
        corpse_guid: u64,
        loot_slot: u32,
        vote: u8,
    ) -> Result<LootActionStatus>;

    fn loot_master_give(
        &self,
        actor: Actor,
        corpse_guid: u64,
        loot_slot: u8,
        target_guid: u64,
    ) -> Result<LootActionStatus>;
}

pub(crate) fn run_vote<St: LootRollStore + ShardRoutingStore + ?Sized>(
    store: &St,
    actor: Actor,
    corpse_guid: u64,
    slot: u32,
    vote: u8,
) -> Result<LootActionStatus> {
    let Some(realm) = store.realm_store()? else {
        return store.loot_roll(actor, corpse_guid, slot, vote);
    };
    flush_pending_promotions(store, realm.as_ref());
    realm.realm_loot_vote(corpse_guid, slot as u8, actor, vote)
}

fn promote_one(shard: &dyn WorldStore, realm: &dyn WorldStore, roll: &PendingLootRoll) {
    match realm.realm_loot_start(
        roll.corpse_guid,
        roll.slot,
        roll.item_entry,
        roll.deadline_micros,
        roll.recipients.clone(),
        roll.random_property_id,
        roll.promotion_source,
        roll.roll_id,
    ) {
        Ok(()) => {
            if let Err(e) = shard.clear_promoted_loot_roll(roll.roll_id) {
                log::warn!(
                    "loot-roll relay: promoted roll {} on {} but could not clear the staging copy \
                     ({e:#}) — retried next tick; the roll is authoritative on realm-core either way",
                    roll.roll_id,
                    shard.shard_name()
                );
            }
        }
        Err(e) => log::warn!(
            "loot-roll relay: could not promote roll {} on {} ({e:#}) — retried next tick",
            roll.roll_id,
            shard.shard_name()
        ),
    }
}

pub(crate) fn flush_pending_promotions<St: ShardRoutingStore + ?Sized>(
    store: &St,
    realm: &dyn WorldStore,
) {
    for shard in store.world_stores() {
        let pending = match shard.pending_local_rolls() {
            Ok(p) => p,
            Err(e) => {
                log::warn!(
                    "loot-roll flush: could not read {}'s pending rolls before a disband-capable op \
                     ({e:#}) — its rolls stay unpromoted until the next scheduled relay tick",
                    shard.shard_name()
                );
                continue;
            }
        };
        for roll in &pending {
            promote_one(shard.as_ref(), realm, roll);
        }
    }
}

/// Advance the win watermark only after every settlement succeeds, or failed wins are lost.
pub(crate) fn relay_tick<St: ShardRoutingStore + ?Sized>(store: &St, won_watermark: &mut u64) {
    let realm = match store.realm_store() {
        Ok(Some(realm)) => realm,
        Ok(None) => return,
        Err(error) => {
            log::warn!("loot-roll Relay waits for Realm-core: {error:#}");
            return;
        }
    };
    flush_pending_promotions(store, realm.as_ref());

    match realm.loot_won_since(*won_watermark) {
        Ok((new_watermark, wins)) => {
            let mut settled = true;
            for (corpse_guid, slot, winner_guid) in wins {
                for shard in store.world_stores() {
                    if let Err(e) = shard.settle_loot_roll(corpse_guid, slot, winner_guid) {
                        settled = false;
                        log::warn!(
                            "loot-roll relay: could not settle ({corpse_guid}, {slot}) on {} ({e:#}); \
                             retrying while the result event remains",
                            shard.shard_name()
                        );
                    }
                }
            }
            if settled {
                *won_watermark = new_watermark;
            }
        }
        Err(e) => {
            log::warn!("loot-roll relay: could not read realm-core's ROLL_WON events ({e:#})")
        }
    }
}
