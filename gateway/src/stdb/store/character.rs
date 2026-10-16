use lyracore_shared::spell::taxonomy::{A_SPELLMOD_FLAT, A_SPELLMOD_PCT};

use anyhow::Result;
use spacetimedb_sdk::Table;

use crate::codec;
use crate::stdb::bindings::*;
use crate::stdb::connection::{call_reducer, CharacterPresenceSnapshot, Coordinator};
use crate::stdb::reads::spell_ranks_stack_in_book;
use crate::stdb::views::character_view;
use crate::stdb::{classify, DurableFailure};
use crate::world::{Actor, CharacterStore};

fn stable_character_absence(
    first: &[CharacterPresenceSnapshot],
    second: &[CharacterPresenceSnapshot],
) -> bool {
    first.len() == second.len()
        && first.iter().zip(second).all(|(before, after)| {
            !before.present
                && !after.present
                && before.generation == after.generation
                && before.revision == after.revision
        })
}

fn durable_character_presence_snapshots(
    shards: &[(String, Coordinator)],
    guid: u64,
) -> Result<Vec<CharacterPresenceSnapshot>> {
    shards
        .iter()
        .map(|(name, shard)| {
            shard
                .0
                .coord()
                .durable_character_presence_snapshot(guid, name)
        })
        .collect()
}

impl CharacterStore for Coordinator {
    fn characters(&self, account_id: u64) -> Result<Vec<codec::CharacterView>> {
        let mut out: Vec<codec::CharacterView> = if self.is_sharded() {
            let mut out: Vec<codec::CharacterView> = Vec::new();
            for shard in self.all_shards() {
                for c in shard.characters(account_id)? {
                    if !out.iter().any(|existing| existing.guid == c.guid) {
                        out.push(c);
                    }
                }
            }
            out
        } else {
            self.characters(account_id)?
        };
        match self.realm_core() {
            Ok(realm) => {
                for c in &mut out {
                    c.guild_id = realm.guild_projection(c.guid).0;
                }
            }
            Err(error) => log::warn!("world: character list without guild ids: {error:#}"),
        }
        Ok(out)
    }

    fn create_character(
        &self,
        account_id: u64,
        name: &str,
        race: u8,
        class: u8,
        gender: u8,
        appearance: codec::Appearance,
    ) -> Result<codec::CharCreateOutcome> {
        let coordinator = self.0.visibility_pipe();
        let result = call_reducer!(
            coordinator.conn.reducers,
            "create_character",
            create_character_then(
                account_id,
                name.to_string(),
                race,
                class,
                gender,
                appearance.skin,
                appearance.face,
                appearance.hair_style,
                appearance.hair_color,
                appearance.facial_hair
            )
        );
        create_outcome(result, self.shard_name())
    }

    fn delete_character(
        &self,
        account_id: u64,
        character: Actor,
    ) -> Result<codec::CharDeleteOutcome> {
        match crate::realm_core::resolve_delete_shard(self, character.guid()) {
            Ok(Some(owner)) => owner.delete_here(account_id, character),
            Ok(None) => self.delete_here(account_id, character),
            Err(e) => {
                log::warn!("world: {e:#}");
                Ok(codec::CharDeleteOutcome::Failed)
            }
        }
    }

    fn character_by_guid(&self, guid: u64) -> Result<Option<codec::CharacterView>> {
        self.character_by_guid(guid)
    }

    fn character_exists_on_any_world_shard(&self, guid: u64) -> Result<bool> {
        let shards = self.world_shards_for_absence()?;
        let first = durable_character_presence_snapshots(&shards, guid)?;
        if first.iter().any(|snapshot| snapshot.present) {
            return Ok(true);
        }
        let second = durable_character_presence_snapshots(&shards, guid)?;
        if second.iter().any(|snapshot| snapshot.present) {
            return Ok(true);
        }
        if !stable_character_absence(&first, &second) {
            anyhow::bail!(
                "World Shard Character presence changed while checking {guid}; cleanup is deferred"
            );
        }
        Ok(false)
    }

    fn player_skills(&self, character_guid: u64) -> Result<Vec<(u32, u16, u16)>> {
        self.player_skills(character_guid)
    }

    fn effective_armor(&self, guid: u64) -> u32 {
        self.effective_armor(guid)
    }

    fn effective_magic_resistances(&self, guid: u64) -> [u32; 6] {
        self.effective_magic_resistances(guid)
    }

    fn spell_modifiers(&self, character_guid: u64) -> Vec<(u32, u8, i32, bool)> {
        self.0
            .coord()
            .conn
            .db
            .game_aura()
            .iter()
            .filter(|a| {
                a.target_guid == character_guid
                    && (a.eff_kind == A_SPELLMOD_FLAT || a.eff_kind == A_SPELLMOD_PCT)
            })
            .map(|a| {
                (
                    a.eff_p_1 as u32,
                    a.eff_p_0 as u8,
                    a.amount,
                    a.eff_kind == A_SPELLMOD_PCT,
                )
            })
            .collect()
    }

    fn player_learned_spells(&self, player_guid: u64) -> Result<Vec<u32>> {
        let guard = self.0.coord();
        let db = &guard.conn.db;
        let known: Vec<u32> = db
            .game_player_spell()
            .iter()
            .filter(|s| s.character_guid == player_guid)
            .map(|s| s.spell_id)
            .collect();
        let known_set: std::collections::HashSet<u32> = known.iter().copied().collect();
        let superseded: std::collections::HashSet<u32> = db
            .game_spell_chain()
            .iter()
            .filter(|c| {
                c.prev_spell != 0
                    && known_set.contains(&c.prev_spell)
                    && known_set.contains(&c.spell_id)
                    && !spell_ranks_stack_in_book(db, c.spell_id)
            })
            .map(|c| c.prev_spell)
            .collect();
        Ok(known
            .into_iter()
            .filter(|id| !superseded.contains(id))
            .collect())
    }

    fn player_actions(&self, player_guid: u64) -> Result<Vec<(u8, u32, u8)>> {
        let guard = self.0.coord();
        let db = &guard.conn.db;
        Ok(db
            .game_player_action()
            .iter()
            .filter(|a| a.character_guid == player_guid)
            .map(|a| (a.button, a.action, a.action_type))
            .collect())
    }

    fn set_stand_state(&self, actor: Actor, state: u8) -> Result<()> {
        call_reducer!(
            self.0.call_pipe().conn.reducers,
            "gw_set_stand_state",
            gw_set_stand_state_then(self.session_actor(actor), state)
        )
    }

    fn stand_state(&self, guid: u64) -> u8 {
        self.0
            .coord()
            .conn
            .db
            .game_world_entity()
            .guid()
            .find(&guid)
            .map_or(0, |entity| (entity.unit_bytes_1 & 0xFF) as u8)
    }

    fn player_reputations(&self, player_guid: u64) -> Result<Vec<(i32, i32, bool)>> {
        let guard = self.0.coord();
        let db = &guard.conn.db;
        Ok(db
            .game_player_reputation()
            .iter()
            .filter(|r| r.character_guid == player_guid && r.reputation_index >= 0)
            .map(|r| (r.reputation_index, r.standing, r.at_war))
            .collect())
    }
}

impl Coordinator {
    pub fn characters(&self, account_id: u64) -> Result<Vec<crate::codec::CharacterView>> {
        use wow_world_messages::vanilla::{CharacterGear, InventoryType};
        let guard = self.0.coord();
        let db = &guard.conn.db;
        let mut views: Vec<crate::codec::CharacterView> = db
            .game_character()
            .iter()
            .filter(|c| c.account_id == account_id)
            .map(character_view)
            .collect();
        for view in &mut views {
            for item in guard
                .item_rows(view.guid)
                .into_iter()
                .filter(|i| i.slot <= 18)
            {
                let Some(tmpl) = db.game_item_template().entry().find(&item.entry) else {
                    continue;
                };
                let inv_type =
                    InventoryType::try_from(u32::from(tmpl.inventory_type)).unwrap_or_default();
                view.equipment[item.slot as usize] = CharacterGear {
                    equipment_display_id: tmpl.display_id,
                    inventory_type: inv_type,
                };
            }
        }
        Ok(views)
    }

    pub fn character_by_guid(&self, guid: u64) -> Result<Option<crate::codec::CharacterView>> {
        Ok(self
            .0
            .coord()
            .conn
            .db
            .game_character()
            .guid()
            .find(&guid)
            .map(character_view))
    }

    pub fn effective_armor(&self, guid: u64) -> u32 {
        let guard = self.0.coord();
        super::super::armor::effective_armor(&guard, guid)
    }

    pub fn effective_magic_resistances(&self, guid: u64) -> [u32; 6] {
        let guard = self.0.coord();
        super::super::armor::effective_magic_resistances(&guard, guid)
    }

    fn has_auction(&self, character_guid: u64) -> bool {
        let auctions = self.auction_keys(|index| &index.auctions, character_guid);
        let guard = self.0.coord();
        auctions.into_iter().any(|id| {
            u32::try_from(id)
                .ok()
                .and_then(|id| guard.conn.db.game_auction().id().find(&id))
                .is_some_and(|auction| {
                    auction.owner_guid == character_guid
                        || auction.highest_bidder_guid == character_guid
                })
        })
    }

    fn delete_here(
        &self,
        account_id: u64,
        character: Actor,
    ) -> Result<crate::codec::CharDeleteOutcome> {
        use crate::codec::CharDeleteOutcome;
        let character_guid = character.guid();
        match self.character_has_auction_value(character_guid) {
            Ok(true) => return Ok(CharDeleteOutcome::Failed),
            Ok(false) => {}
            Err(error) => {
                log::warn!(
                    "delete_character: could not verify auction value for {character_guid}: {error:#}"
                );
                return Ok(CharDeleteOutcome::Failed);
            }
        }
        delete_outcome(
            call_reducer!(
                self.0.call_pipe().conn.reducers,
                "delete_character",
                delete_character_then(account_id, self.session_actor(character))
            ),
            character_guid,
        )
    }

    fn character_has_auction_value(&self, character_guid: u64) -> Result<bool> {
        for (_, shard) in self.world_shards() {
            if !shard.listing_holds(character_guid).is_empty()
                || shard
                    .unfinished_auction_holds(character_guid)
                    .next()
                    .is_some()
            {
                return Ok(true);
            }
        }
        Ok(self.realm_core()?.has_auction(character_guid))
    }
}

fn create_outcome(result: Result<()>, shard_name: &str) -> Result<codec::CharCreateOutcome> {
    use codec::CharCreateOutcome;
    let Err(error) = result else {
        return Ok(CharCreateOutcome::Success);
    };
    match classify(&error) {
        DurableFailure::TransportLoss => Err(error),
        DurableFailure::Refusal { reason } if reason.contains("NAME_IN_USE") => {
            Ok(CharCreateOutcome::NameInUse)
        }
        DurableFailure::Refusal { reason } if reason.contains("SERVER_LIMIT") => {
            Ok(CharCreateOutcome::ServerLimit)
        }
        DurableFailure::Refusal { .. } => {
            log::warn!("create_character on {shard_name} failed: {error:#}");
            Ok(CharCreateOutcome::Failed)
        }
    }
}

fn delete_outcome(result: Result<()>, character_guid: u64) -> Result<codec::CharDeleteOutcome> {
    let Err(error) = result else {
        return Ok(codec::CharDeleteOutcome::Success);
    };
    match classify(&error) {
        DurableFailure::Refusal { .. } => {
            log::info!("delete_character: {character_guid} not deleted: {error:#}");
            Ok(codec::CharDeleteOutcome::Failed)
        }
        DurableFailure::TransportLoss => Err(error),
    }
}

#[cfg(test)]
mod outcome_tests {
    use super::{create_outcome, delete_outcome};
    use crate::codec::{CharCreateOutcome, CharDeleteOutcome};
    use crate::stdb::ReducerCallError;

    #[test]
    fn a_create_refusal_is_an_outcome_and_a_transport_loss_is_an_error() {
        let refused = ReducerCallError::refused("create_character", "NAME_IN_USE");
        assert!(matches!(
            create_outcome(Err(refused.into()), "world"),
            Ok(CharCreateOutcome::NameInUse)
        ));
        let lost = ReducerCallError::transport_lost("create_character");
        assert!(create_outcome(Err(lost.into()), "world").is_err());
    }

    #[test]
    fn a_delete_refusal_fails_and_a_transport_loss_is_an_error() {
        let refused = ReducerCallError::refused("delete_character", "NOT_OWNER");
        assert!(matches!(
            delete_outcome(Err(refused.into()), 7),
            Ok(CharDeleteOutcome::Failed)
        ));
        let lost = ReducerCallError::transport_lost("delete_character");
        assert!(delete_outcome(Err(lost.into()), 7).is_err());
    }
}

#[cfg(test)]
mod stable_absence_tests {
    #[test]
    fn a_transfer_between_durable_scans_cannot_prove_stable_absence() {
        use super::{stable_character_absence, CharacterPresenceSnapshot};

        let snapshot = |generation, revision, present| CharacterPresenceSnapshot {
            generation,
            revision,
            present,
        };
        let first = [snapshot(1, 0, false), snapshot(2, 1, false)];
        let destination_visible = [snapshot(1, 1, true), snapshot(2, 1, false)];
        assert!(!stable_character_absence(&first, &destination_visible));

        let moved_again = [snapshot(1, 2, false), snapshot(2, 2, false)];
        assert!(!stable_character_absence(&first, &moved_again));

        let unchanged = [snapshot(1, 0, false), snapshot(2, 1, false)];
        assert!(stable_character_absence(&first, &unchanged));
    }
}
