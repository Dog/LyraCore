use anyhow::Result;
use lyracore_shared::item::ItemRefusal;

use crate::stdb::bindings::*;
use crate::stdb::connection::{call_reducer, classify, DurableFailure};
use crate::stdb::Coordinator;
use crate::world::{Actor, ItemActionResult, ItemActionStore};

impl ItemActionStore for Coordinator {
    fn equip_item(&self, actor: Actor, from_slot: u8) -> Result<ItemActionResult> {
        let coord = self.0.call_pipe();
        item_action(call_reducer!(
            coord.conn.reducers,
            "gw_equip_item",
            gw_equip_item_then(self.session_actor(actor), from_slot)
        ))
    }

    fn unequip_item(&self, actor: Actor, from_slot: u8) -> Result<ItemActionResult> {
        let coord = self.0.call_pipe();
        item_action(call_reducer!(
            coord.conn.reducers,
            "gw_unequip_item",
            gw_unequip_item_then(self.session_actor(actor), from_slot)
        ))
    }

    fn move_item(&self, actor: Actor, from_slot: u8, to_slot: u8) -> Result<ItemActionResult> {
        let coord = self.0.call_pipe();
        item_action(call_reducer!(
            coord.conn.reducers,
            "gw_move_item",
            gw_move_item_then(self.session_actor(actor), from_slot, to_slot)
        ))
    }

    fn use_item(&self, actor: Actor, slot: u8) -> Result<ItemActionResult> {
        let coord = self.0.call_pipe();
        item_action(call_reducer!(
            coord.conn.reducers,
            "gw_use_item",
            gw_use_item_then(self.session_actor(actor), slot)
        ))
    }

    fn split_item(
        &self,
        actor: Actor,
        from_slot: u8,
        to_slot: u8,
        count: u32,
    ) -> Result<ItemActionResult> {
        let coord = self.0.call_pipe();
        item_action(call_reducer!(
            coord.conn.reducers,
            "gw_split_item",
            gw_split_item_then(self.session_actor(actor), from_slot, to_slot, count)
        ))
    }

    fn destroy_item(&self, actor: Actor, slot: u8, count: u32) -> Result<ItemActionResult> {
        let coord = self.0.call_pipe();
        item_action(call_reducer!(
            coord.conn.reducers,
            "gw_destroy_item",
            gw_destroy_item_then(self.session_actor(actor), slot, count)
        ))
    }
}

fn item_action(result: Result<()>) -> Result<ItemActionResult> {
    match result {
        Ok(()) => Ok(ItemActionResult::Done),
        Err(error) => match classify(&error) {
            DurableFailure::Refusal { reason } => match ItemRefusal::parse_tag(reason) {
                Some(refusal) => Ok(refusal.into()),
                None => Err(error),
            },
            DurableFailure::TransportLoss => Err(error),
        },
    }
}

#[cfg(test)]
mod item_reducer_tests {
    use super::*;
    use crate::stdb::connection::ReducerCallError;
    use anyhow::anyhow;

    #[test]
    fn only_a_rejected_reducer_carries_a_typed_refusal() {
        for refusal in ItemRefusal::ALL {
            let rejected = Err(anyhow::Error::from(ReducerCallError::refused(
                "gw_move_item",
                refusal.as_tag(),
            ))
            .context("move phase"));
            assert_eq!(
                item_action(rejected).unwrap(),
                ItemActionResult::Refused(refusal)
            );
        }

        assert_eq!(item_action(Ok(())).unwrap(), ItemActionResult::Done);

        let not_refusals = [
            anyhow::Error::from(ReducerCallError::fatal(
                "gw_move_item reducer timed out after 10s".to_string(),
            )),
            anyhow::Error::from(ReducerCallError::transport_lost("gw_use_item")),
            anyhow::Error::from(ReducerCallError::refused("gw_equip_item", "operator only")),
            anyhow!(
                "wrapped text that mentions {}",
                ItemRefusal::Internal.as_tag()
            ),
        ];
        for error in not_refusals {
            let text = format!("{error:#}");
            assert!(item_action(Err(error)).is_err(), "{text}");
        }
    }
}
