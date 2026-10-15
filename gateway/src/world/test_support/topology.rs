use super::*;

#[derive(Default)]
pub(crate) struct TopologyState {
    pub(crate) shard: String,
    pub(crate) home: Option<std::sync::Arc<WorldFake>>,
    pub(crate) home_after_flip: Option<std::sync::Arc<WorldFake>>,
    pub(crate) location_shard: Option<(u32, u64, std::sync::Arc<WorldFake>)>,
    pub(crate) home_shard_calls: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    pub(crate) calls: std::sync::Arc<std::sync::Mutex<Vec<(String, String)>>>,
    pub(crate) realm: Option<std::sync::Arc<WorldFake>>,
    pub(crate) peers: std::sync::Mutex<Vec<std::sync::Arc<WorldFake>>>,
    pub(crate) world_shard_set_error: Option<String>,
    pub(crate) realm_error: std::sync::Mutex<Option<String>>,
    pub(crate) settle_error: Option<String>,
    pub(crate) settle_ok_calls: usize,
    pub(crate) settle_calls: std::sync::atomic::AtomicUsize,
    pub(crate) bound_sessions: std::sync::Mutex<Vec<u64>>,
    pub(crate) viewer_present_at_settle: std::sync::Mutex<Vec<bool>>,
}

impl ShardRoutingStore for WorldFake {
    fn shard_name(&self) -> &str {
        &self.topology.shard
    }

    fn settle_home_shard(
        &self,
        character_guid: u64,
    ) -> Result<Option<std::sync::Arc<dyn WorldStore>>> {
        if let Some(view) = &self.session.relay_view {
            self.topology.viewer_present_at_settle.lock().unwrap().push(
                view.viewer_of_owner(crate::stdb::world_view::OwnerGuid(character_guid))
                    .is_some(),
            );
        }
        if let Some(e) = &self.topology.settle_error {
            let nth = self
                .topology
                .settle_calls
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if nth >= self.topology.settle_ok_calls {
                return Err(anyhow!("{e}"));
            }
        }
        Ok(self.home_shard(character_guid))
    }

    fn bind_shard_session(&self, account_id: u64, _session_key: &[u8; 40]) -> Result<()> {
        self.rec("bind_shard_session");
        self.topology
            .bound_sessions
            .lock()
            .unwrap()
            .push(account_id);
        Ok(())
    }

    fn shard_for_location(
        &self,
        map_id: u32,
        instance_id: u64,
    ) -> Option<std::sync::Arc<dyn WorldStore>> {
        let (m, i, shard) = self.topology.location_shard.as_ref()?;
        (*m == map_id && *i == instance_id).then(|| shard.clone() as std::sync::Arc<dyn WorldStore>)
    }

    fn realm_store(&self) -> Result<Option<std::sync::Arc<dyn WorldStore>>> {
        if let Some(error) = self.topology.realm_error.lock().unwrap().as_ref() {
            return Err(anyhow!(error.clone()));
        }
        Ok(self
            .topology
            .realm
            .clone()
            .map(|r| r as std::sync::Arc<dyn WorldStore>))
    }

    fn world_stores(&self) -> Vec<std::sync::Arc<dyn WorldStore>> {
        self.topology
            .peers
            .lock()
            .unwrap()
            .iter()
            .map(|p| p.clone() as std::sync::Arc<dyn WorldStore>)
            .collect()
    }

    fn party_command_worlds(&self) -> Result<Vec<std::sync::Arc<dyn WorldStore>>> {
        if let Some(error) = &self.topology.world_shard_set_error {
            return Err(anyhow!(error.clone()));
        }
        Ok(self.world_stores())
    }
}

impl WorldFake {
    pub(crate) fn rec(&self, what: &str) {
        self.topology
            .calls
            .lock()
            .unwrap()
            .push((self.topology.shard.clone(), what.to_string()));
    }

    pub(crate) fn home_shard(
        &self,
        _character_guid: u64,
    ) -> Option<std::sync::Arc<dyn WorldStore>> {
        let nth = self
            .topology
            .home_shard_calls
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let resolved = match (&self.topology.home_after_flip, nth) {
            (Some(flipped), n) if n >= 1 => Some(flipped.clone()),
            _ => self.topology.home.clone(),
        };
        resolved.map(|h| h as std::sync::Arc<dyn WorldStore>)
    }
}
