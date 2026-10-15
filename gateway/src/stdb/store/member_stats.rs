use anyhow::Result;
use spacetimedb_sdk::Table;

use crate::stdb::bindings::*;
use crate::stdb::Coordinator;
use crate::world::{MemberPresence, MemberStatsStore};

impl MemberStatsStore for Coordinator {
    fn group_mates(&self, self_guid: u64) -> Result<Vec<u64>> {
        let authority = if self.is_sharded() {
            self.realm_core()?
        } else {
            self.clone()
        };
        let roster = authority
            .0
            .coord()
            .party_memberships
            .read()
            .unwrap()
            .bounded_member_rows(self_guid, lyracore_shared::group::RAID_MAX_MEMBERS)?;
        Ok(roster
            .map(|(_, rows)| {
                rows.into_iter()
                    .map(|(_, guid)| guid)
                    .filter(|guid| *guid != self_guid)
                    .collect()
            })
            .unwrap_or_default())
    }

    fn member_presence(&self, guid: u64) -> Result<MemberPresence> {
        use crate::world::presence::Whereabouts;
        Ok(
            match crate::world::presence::of(self, guid)?.map(|presence| presence.whereabouts) {
                Some(Whereabouts::InWorld {
                    entity, shard_name, ..
                }) => {
                    let entity = self.with_member_shard_stats(&shard_name, guid, entity);
                    MemberPresence::Live(Box::new(crate::codec::MemberStats::from_entity(&entity)))
                }
                Some(Whereabouts::InTransit) => MemberPresence::InTransit,
                Some(Whereabouts::Offline) | None => MemberPresence::Offline,
            },
        )
    }
}

impl Coordinator {
    fn with_member_shard_stats(
        &self,
        shard_name: &str,
        guid: u64,
        entity: crate::codec::MemberEntity,
    ) -> crate::codec::MemberEntity {
        let Some(shard) = self
            .all_shards()
            .iter()
            .position(|coord| coord.shard_name() == shard_name)
        else {
            return entity;
        };
        crate::codec::MemberEntity {
            auras: self.member_aura_slots(shard, guid),
            pet: self.member_pet(shard, guid),
            ..entity
        }
    }

    fn member_aura_slots(
        &self,
        shard: crate::stdb::world_index::ShardId,
        owner_guid: u64,
    ) -> Vec<crate::codec::MemberAuraSlot> {
        self.world_view()
            .auras
            .on_target(shard, owner_guid)
            .into_iter()
            .map(|aura| crate::codec::MemberAuraSlot {
                slot: aura.slot,
                spell_id: aura.spell_id,
            })
            .collect()
    }

    fn member_pet(
        &self,
        shard: crate::stdb::world_index::ShardId,
        owner_guid: u64,
    ) -> Option<crate::codec::MemberPetEntity> {
        let live = self.0.coord();
        let db = &live.conn.db;
        let pet_guid = lyracore_shared::pet::pet_guid_for(owner_guid);
        let pet = db
            .game_world_entity()
            .guid()
            .find(&pet_guid)
            .filter(|pet| pet.owner_guid == owner_guid)?;
        let name = db
            .game_hunter_pet_protocol()
            .iter()
            .find(|hunter| hunter.live_pet_guid == pet.guid)
            .map(|hunter| hunter.name)
            .or_else(|| {
                db.game_creature_template()
                    .entry()
                    .find(&pet.entry)
                    .map(|template| template.name)
            })
            .unwrap_or_default();
        Some(crate::codec::MemberPetEntity {
            guid: pet.guid,
            name,
            display_id: pet.display_id,
            health: pet.health,
            max_health: pet.max_health,
            power: pet.power,
            max_power: pet.max_power,
            unit_bytes_0: pet.unit_bytes_0,
            auras: self.member_aura_slots(shard, pet.guid),
        })
    }
}
