use anyhow::Result;
use lyracore_shared::social::ContactRefusal;
use spacetimedb_sdk::Table;

use crate::stdb::bindings::*;
use crate::stdb::connection::call_reducer;
use crate::stdb::Coordinator;
use crate::stdb::{classify, DurableFailure};
use crate::world::{Actor, ContactOutcome, SocialStore};

impl SocialStore for Coordinator {
    fn character_identity(
        &self,
        guid: u64,
    ) -> Result<Option<crate::world::presence::CharacterIdentity>> {
        Ok(self
            .0
            .coord()
            .conn
            .db
            .game_character()
            .guid()
            .find(&guid)
            .map(|ch| crate::world::presence::CharacterIdentity {
                guid,
                name: ch.name,
                race: ch.race,
                class: ch.class,
                level: ch.level,
                zone_id: ch.zone_id,
                session_online: ch.online,
            }))
    }

    fn live_entity(&self, guid: u64) -> Option<crate::codec::MemberEntity> {
        let guard = self.0.coord();
        let entity = guard.conn.db.game_world_entity().guid().find(&guid)?;
        Some(crate::codec::MemberEntity {
            health: entity.health,
            max_health: entity.max_health,
            power: entity.power,
            max_power: entity.max_power,
            unit_bytes_0: entity.unit_bytes_0,
            level: entity.level,
            zone_id: entity.zone_id,
            x: entity.x,
            y: entity.y,
            dead: entity.dead,
            player_flags: entity.player_flags,
            ..Default::default()
        })
    }

    fn character_in_transit(&self, guid: u64) -> bool {
        let guard = self.0.coord();
        let db = &guard.conn.db;
        let session_online = db
            .game_character()
            .guid()
            .find(&guid)
            .is_some_and(|character| character.online);
        session_online
            || db
                .game_bot_transfer_intent()
                .iter()
                .any(|intent| intent.bot_guid == guid)
    }

    fn auto_reply_text(&self, guid: u64) -> Result<Option<String>> {
        Ok(self
            .0
            .coord()
            .conn
            .db
            .game_character_away()
            .character_guid()
            .find(&guid)
            .map(|row| row.message))
    }

    fn every_shard_vouches_for_absence(&self) -> Result<()> {
        self.world_shards_for_absence()?;
        Ok(())
    }

    fn in_world_players(&self) -> Result<Vec<crate::world::presence::RealmPresence>> {
        let guard = self.0.coord();
        let db = &guard.conn.db;
        let shard_name = self.shard_name().to_string();
        let rows = db
            .game_world_entity()
            .iter()
            .filter(|e| e.entry == 0)
            .filter_map(|e| {
                let ch = db.game_character().guid().find(&e.guid)?;
                let away = crate::world::presence::away_from_player_flags(e.player_flags);
                let entity = crate::codec::MemberEntity {
                    health: e.health,
                    max_health: e.max_health,
                    power: e.power,
                    max_power: e.max_power,
                    unit_bytes_0: e.unit_bytes_0,
                    level: e.level,
                    zone_id: e.zone_id,
                    x: e.x,
                    y: e.y,
                    dead: e.dead,
                    player_flags: e.player_flags,
                    ..Default::default()
                };
                Some(crate::world::presence::RealmPresence {
                    guid: e.guid,
                    name: ch.name,
                    race: ch.race,
                    class: ch.class,
                    level: u8::try_from(e.level).unwrap_or(u8::MAX),
                    zone_id: e.zone_id,
                    session_online: ch.online,
                    whereabouts: crate::world::presence::Whereabouts::InWorld {
                        away,
                        entity,
                        shard_name: shard_name.clone(),
                    },
                })
            })
            .collect();
        Ok(rows)
    }

    fn zone_name(&self, zone_id: u32) -> String {
        self.0
            .coord()
            .conn
            .db
            .game_area()
            .id()
            .find(&zone_id)
            .map(|a| a.name)
            .unwrap_or_default()
    }

    fn contact_lists(&self, owner_guid: u64) -> Result<(Vec<u64>, Vec<u64>)> {
        let Some(viewer) = self
            .world_view()
            .viewer_of_owner(super::super::world_view::OwnerGuid(owner_guid))
        else {
            return Ok((Vec::new(), Vec::new()));
        };
        Ok((viewer.friend_guids(), viewer.ignored_guids()))
    }

    fn ignored_guids(&self, owner_guid: u64) -> Result<Vec<u64>> {
        let guard = self.0.coord();
        let contacts = guard.contacts.read().unwrap_or_else(|p| p.into_inner());
        Ok(contacts.ignored_by(owner_guid))
    }

    fn character_guid_by_name(&self, name: &str) -> Result<Option<u64>> {
        let guard = self.0.coord();
        let names = guard
            .character_names
            .read()
            .unwrap_or_else(|p| p.into_inner());
        Ok(names.guid_named(name))
    }

    fn add_friend(
        &self,
        actor: Actor,
        target_guid: u64,
        target_race: u8,
    ) -> Result<ContactOutcome> {
        contact_outcome(call_reducer!(
            self.0.call_pipe().conn.reducers,
            "gw_add_friend",
            gw_add_friend_then(self.session_actor(actor), target_guid, target_race)
        ))
    }

    fn del_friend(&self, actor: Actor, target_guid: u64) -> Result<ContactOutcome> {
        contact_outcome(call_reducer!(
            self.0.call_pipe().conn.reducers,
            "gw_del_friend",
            gw_del_friend_then(self.session_actor(actor), target_guid)
        ))
    }

    fn add_ignore(&self, actor: Actor, target_guid: u64) -> Result<ContactOutcome> {
        contact_outcome(call_reducer!(
            self.0.call_pipe().conn.reducers,
            "gw_add_ignore",
            gw_add_ignore_then(self.session_actor(actor), target_guid)
        ))
    }

    fn del_ignore(&self, actor: Actor, target_guid: u64) -> Result<ContactOutcome> {
        contact_outcome(call_reducer!(
            self.0.call_pipe().conn.reducers,
            "gw_del_ignore",
            gw_del_ignore_then(self.session_actor(actor), target_guid)
        ))
    }
}

fn contact_outcome(result: Result<()>) -> Result<ContactOutcome> {
    let Err(error) = result else {
        return Ok(ContactOutcome::Done);
    };
    match classify(&error) {
        DurableFailure::Refusal { reason } => match ContactRefusal::parse_tag(reason) {
            Some(refusal) => Ok(refusal.into()),
            None => Err(error),
        },
        DurableFailure::TransportLoss => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stdb::ReducerCallError;

    #[test]
    fn a_contact_refusal_is_an_outcome_and_a_transport_loss_is_an_error() {
        let refused = ReducerCallError::refused("gw_add_friend", ContactRefusal::ListFull.as_tag());
        assert_eq!(
            contact_outcome(Err(refused.into())).unwrap(),
            ContactOutcome::Refused(ContactRefusal::ListFull)
        );
        let lost = ReducerCallError::transport_lost("gw_add_friend");
        assert!(contact_outcome(Err(lost.into())).is_err());
    }
}
