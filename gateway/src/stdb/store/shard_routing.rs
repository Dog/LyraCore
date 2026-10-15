use anyhow::Result;

use crate::stdb::Coordinator;
use crate::world::{ShardRoutingStore, WorldStore};

impl ShardRoutingStore for Coordinator {
    fn shard_name(&self) -> &str {
        self.shard_name()
    }

    fn settle_home_shard(
        &self,
        character_guid: u64,
    ) -> Result<Option<std::sync::Arc<dyn WorldStore>>> {
        if !self.is_sharded() {
            return Ok(None);
        }
        let Some(holder) = crate::realm_core::locate_home_shard(self, character_guid) else {
            return Ok(None);
        };
        // Resume the Escrow's recorded destination, which outranks the Character's stored location.
        let escrow = holder.escrow_row(character_guid);
        let (map_id, instance_id) = escrow
            .as_ref()
            .map(|r| (r.dest_map_id, r.dest_instance_id))
            .or_else(|| holder.character_location(character_guid))
            .ok_or_else(|| anyhow::anyhow!("character {character_guid} vanished mid-routing"))?;
        let owner = if escrow.is_none() {
            self.instance_shard_for(map_id, instance_id, holder.shard_name())
        } else {
            self.shard_for(map_id, instance_id)
        }
        .unwrap_or_else(|| self.clone());
        log::info!(
            "settle {character_guid}: holder={} owner={} escrow={} ({map_id}/{instance_id})",
            holder.shard_name(),
            owner.shard_name(),
            escrow.is_some()
        );
        crate::world::transfer::settle_transfer(&holder, &owner, character_guid)?;
        Ok(Some(
            std::sync::Arc::new(owner) as std::sync::Arc<dyn WorldStore>
        ))
    }

    fn shard_for_location(
        &self,
        map_id: u32,
        instance_id: u64,
    ) -> Option<std::sync::Arc<dyn WorldStore>> {
        let shard = self.instance_shard_for(map_id, instance_id, Coordinator::shard_name(self))?;
        Some(std::sync::Arc::new(shard) as std::sync::Arc<dyn WorldStore>)
    }

    fn bind_shard_session(&self, account_id: u64, session_key: &[u8; 40]) -> Result<()> {
        let identity = self.bound_identity(account_id)?;
        self.establish_session(account_id, session_key, identity)
    }

    fn realm_store(&self) -> Result<Option<std::sync::Arc<dyn WorldStore>>> {
        if !self.is_sharded() && self.shard_map().realm_core_db().is_none() {
            return Ok(None);
        }
        self.realm_core()
            .map(|realm| Some(std::sync::Arc::new(realm) as std::sync::Arc<dyn WorldStore>))
    }

    fn world_stores(&self) -> Vec<std::sync::Arc<dyn WorldStore>> {
        if !self.is_sharded() {
            return Vec::new();
        }
        self.all_shards()
            .into_iter()
            .map(|c| std::sync::Arc::new(c) as std::sync::Arc<dyn WorldStore>)
            .collect()
    }

    fn party_command_worlds(&self) -> Result<Vec<std::sync::Arc<dyn WorldStore>>> {
        if !self.is_sharded() {
            return Ok(Vec::new());
        }
        Ok(self
            .configured_world_shards()?
            .into_iter()
            .map(|coordinator| std::sync::Arc::new(coordinator) as std::sync::Arc<dyn WorldStore>)
            .collect())
    }
}
