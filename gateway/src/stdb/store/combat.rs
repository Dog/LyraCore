use anyhow::Result;

use crate::stdb::bindings::*;
use crate::stdb::connection::call_reducer;
use crate::stdb::{classify, Coordinator, DurableFailure};
use crate::world::{Actor, CombatStore, PetRenameOutcome};
use lyracore_shared::pet::PET_NAME_INVALID;

impl CombatStore for Coordinator {
    fn set_target(&self, actor: Actor, target_guid: u64) -> Result<()> {
        let coord = self.0.call_pipe();
        call_reducer!(
            coord.conn.reducers,
            "gw_set_target",
            gw_set_target_then(self.session_actor(actor), target_guid)
        )
    }

    fn pet_command(&self, actor: Actor, data: u32, target_guid: u64) -> Result<()> {
        let coord = self.0.call_pipe();
        call_reducer!(
            coord.conn.reducers,
            "gw_pet_command",
            gw_pet_command_then(self.session_actor(actor), data, target_guid)
        )
    }

    fn pet_rename(&self, actor: Actor, pet_guid: u64, name: &str) -> Result<PetRenameOutcome> {
        let coord = self.0.call_pipe();
        pet_rename_outcome(call_reducer!(
            coord.conn.reducers,
            "gw_pet_rename",
            gw_pet_rename_then(self.session_actor(actor), pet_guid, name.to_string())
        ))
    }

    fn set_sheathed(&self, actor: Actor, state: u8) -> Result<()> {
        let coord = self.0.call_pipe();
        call_reducer!(
            coord.conn.reducers,
            "gw_set_sheathed",
            gw_set_sheathed_then(self.session_actor(actor), state)
        )
    }
}

/// Only PET_NAME_INVALID becomes a client-visible outcome; other failures stay Err.
fn pet_rename_outcome(result: Result<()>) -> Result<PetRenameOutcome> {
    let Err(error) = result else {
        return Ok(PetRenameOutcome::Renamed);
    };
    match classify(&error) {
        DurableFailure::Refusal { reason } if reason == PET_NAME_INVALID => {
            Ok(PetRenameOutcome::NameInvalid)
        }
        _ => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stdb::ReducerCallError;

    #[test]
    fn only_the_bad_name_refusal_becomes_an_outcome() {
        assert_eq!(
            pet_rename_outcome(Ok(())).unwrap(),
            PetRenameOutcome::Renamed
        );
        let bad_name = ReducerCallError::refused("gw_pet_rename", PET_NAME_INVALID);
        assert_eq!(
            pet_rename_outcome(Err(bad_name.into())).unwrap(),
            PetRenameOutcome::NameInvalid
        );
        let not_yours =
            ReducerCallError::refused("gw_pet_rename", "pet rename: 1 has no live pet 9");
        assert!(matches!(
            classify(&pet_rename_outcome(Err(not_yours.into())).unwrap_err()),
            DurableFailure::Refusal { .. }
        ));
        let lost = ReducerCallError::transport_lost("gw_pet_rename");
        assert!(matches!(
            classify(&pet_rename_outcome(Err(lost.into())).unwrap_err()),
            DurableFailure::TransportLoss
        ));
    }
}
