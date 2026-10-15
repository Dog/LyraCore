use super::super::*;
use crate::stdb::ReducerCallError;

#[derive(Default)]
pub(crate) struct CombatState {
    pub(crate) set_target_transport_lost: bool,
}

impl CombatStore for WorldFake {
    fn set_target(&self, _actor: Actor, target_guid: u64) -> Result<()> {
        if self.combat.set_target_transport_lost {
            return Err(ReducerCallError::transport_lost("gw_set_target").into());
        }
        if let Some(state) = &self.benilla_gameplay {
            state.lock().unwrap().selected = target_guid;
        }
        Ok(())
    }

    fn pet_command(&self, _actor: Actor, _data: u32, _target_guid: u64) -> Result<()> {
        Ok(())
    }

    fn pet_rename(
        &self,
        _actor: Actor,
        _pet_guid: u64,
        _name: &str,
    ) -> Result<crate::world::handlers::PetRenameOutcome> {
        Ok(crate::world::handlers::PetRenameOutcome::Renamed)
    }

    fn set_sheathed(&self, _actor: Actor, _state: u8) -> Result<()> {
        Ok(())
    }
}
