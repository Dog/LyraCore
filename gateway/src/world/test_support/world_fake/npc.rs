use super::super::*;
use crate::stdb::ReducerCallError;

#[derive(Default)]
pub(crate) struct NpcState {
    pub(crate) innkeeper: bool,
    pub(crate) home_bound: std::sync::atomic::AtomicBool,
    pub(crate) gossip_opts: Vec<codec::GossipOptionView>,
    pub(crate) gossip_selects: std::sync::Mutex<Vec<(u32, u32)>>,
    pub(crate) npc_text_view: Option<codec::NpcTextView>,
    pub(crate) gameobject_type: Option<u8>,
    pub(crate) inspect_error: Option<fn() -> anyhow::Error>,
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

    fn gossip_options(&self, _npc_guid: u64) -> Result<Vec<codec::GossipOptionView>> {
        Ok(self.npc.gossip_opts.clone())
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
        _actor: Actor,
        _npc_guid: u64,
        option_id: u32,
        option_row_id: u32,
    ) -> Result<()> {
        self.npc
            .gossip_selects
            .lock()
            .unwrap()
            .push((option_id, option_row_id));
        Ok(())
    }
}
