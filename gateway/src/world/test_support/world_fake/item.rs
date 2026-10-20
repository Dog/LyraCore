use super::super::*;

impl ItemActionStore for WorldFake {
    fn item_name(&self, entry: u32) -> Result<Option<String>> {
        Ok(self
            .cast
            .item_templates
            .iter()
            .find(|template| template.entry == entry)
            .map(|template| template.name.clone()))
    }

    fn readable_item(&self, _owner_guid: u64, _slot: u8) -> Result<ReadableItem> {
        Ok(ReadableItem::Missing)
    }

    fn item_guid(&self, _owner_guid: u64, slot: u8) -> Option<u64> {
        let state = self.benilla_gameplay.as_ref()?.lock().unwrap();
        state
            .inventory
            .iter()
            .find(|item| item.slot == slot)
            .map(|item| item.guid)
    }

    fn destroy_item(&self, _actor: Actor, _slot: u8, _count: u32) -> Result<ItemActionResult> {
        Ok(lyracore_shared::item::ItemRefusal::ItemNotFound.into())
    }

    fn split_item(
        &self,
        _actor: Actor,
        _from_slot: u8,
        _to_slot: u8,
        _count: u32,
    ) -> Result<ItemActionResult> {
        Ok(lyracore_shared::item::ItemRefusal::ItemNotFound.into())
    }

    fn equip_item(&self, _actor: Actor, _from_slot: u8) -> Result<ItemActionResult> {
        Ok(ItemActionResult::Done)
    }

    fn unequip_item(&self, _actor: Actor, _from_slot: u8) -> Result<ItemActionResult> {
        Ok(ItemActionResult::Done)
    }

    fn move_item(&self, _actor: Actor, from_slot: u8, to_slot: u8) -> Result<ItemActionResult> {
        if let Some(state) = &self.benilla_gameplay {
            let mut state = state.lock().unwrap();
            for item in &mut state.inventory {
                if item.slot == from_slot {
                    item.slot = to_slot;
                } else if item.slot == to_slot {
                    item.slot = from_slot;
                }
            }
        }
        Ok(ItemActionResult::Done)
    }

    fn use_item(&self, _actor: Actor, _slot: u8) -> Result<ItemActionResult> {
        Ok(ItemActionResult::Done)
    }
}
