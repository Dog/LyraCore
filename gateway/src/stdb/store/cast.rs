use lyracore_shared::spell::taxonomy::{
    E_DISENCHANT, E_ENCHANT_ITEM, E_FISH, E_NEXT_SWING, E_OPEN_LOCK, E_PERSISTENT_AREA,
    SPELL_ATTR_RANGED_AUTO_REPEAT,
};

use anyhow::Result;
use spacetimedb_sdk::Table;

use crate::codec;
use crate::stdb::bindings::*;
use crate::stdb::connection::call_reducer;
use crate::stdb::views::item_template_view;
use crate::stdb::Coordinator;
use crate::world::{Actor, CastStore, EnchantRoute};

impl CastStore for Coordinator {
    fn spell_cast_time(&self, spell_id: u32) -> Option<u32> {
        self.0
            .coord()
            .conn
            .db
            .game_spell()
            .spell_id()
            .find(&spell_id)
            .map(|s| s.cast_time_ms)
    }

    fn enchant_route(&self, spell_id: u32) -> Option<EnchantRoute> {
        self.0
            .coord()
            .conn
            .db
            .game_spell_effect()
            .iter()
            .filter(|e| e.spell_id == spell_id)
            .find_map(|e| match e.kind {
                E_ENCHANT_ITEM => Some(EnchantRoute::Enchant(e.p_0 as u32)),
                E_DISENCHANT => Some(EnchantRoute::Disenchant),
                _ => None,
            })
    }

    fn spell_is_ground_area(&self, spell_id: u32) -> bool {
        self.0
            .coord()
            .conn
            .db
            .game_spell_effect()
            .iter()
            .any(|e| e.spell_id == spell_id && e.kind == E_PERSISTENT_AREA)
    }

    fn spell_is_fishing(&self, spell_id: u32) -> bool {
        self.0
            .coord()
            .conn
            .db
            .game_spell_effect()
            .iter()
            .any(|e| e.spell_id == spell_id && e.kind == E_FISH)
    }

    fn spell_is_open_lock(&self, spell_id: u32) -> bool {
        self.0
            .coord()
            .conn
            .db
            .game_spell_effect()
            .iter()
            .any(|e| e.spell_id == spell_id && e.kind == E_OPEN_LOCK)
    }

    fn spell_queues_next_swing(&self, spell_id: u32) -> bool {
        let guard = self.0.coord();
        let queues = guard
            .conn
            .db
            .game_spell_effect()
            .iter()
            .any(|e| e.spell_id == spell_id && e.kind == E_NEXT_SWING);
        queues
    }

    fn spell_is_ranged_auto_repeat(&self, spell_id: u32) -> bool {
        let guard = self.0.coord();
        guard
            .conn
            .db
            .game_spell()
            .spell_id()
            .find(&spell_id)
            .is_some_and(|s| s.cast_flags & SPELL_ATTR_RANGED_AUTO_REPEAT != 0)
    }

    fn item_template(&self, entry: u32) -> Result<Option<crate::codec::ItemTemplateView>> {
        Ok(self
            .0
            .coord()
            .conn
            .db
            .game_item_template()
            .entry()
            .find(&entry)
            .map(item_template_view))
    }

    fn start_ranged_attack(&self, actor: Actor, target_guid: u64, spell_id: u32) -> Result<()> {
        let coord = self.0.call_pipe();
        call_reducer!(
            coord.conn.reducers,
            "gw_ranged_attack",
            gw_ranged_attack_then(self.session_actor(actor), target_guid, spell_id)
        )
    }

    fn cast_spell(&self, actor: Actor, spell_id: u32, target_guid: u64) -> Result<()> {
        let coord = self.0.call_pipe();
        call_reducer!(
            coord.conn.reducers,
            "gw_cast_spell",
            gw_cast_spell_then(self.session_actor(actor), spell_id, target_guid)
        )
    }

    fn cast_item_target(&self, actor: Actor, spell_id: u32, slot: u8) -> Result<()> {
        let coord = self.0.call_pipe();
        call_reducer!(
            coord.conn.reducers,
            "gw_cast_item_target",
            gw_cast_item_target_then(self.session_actor(actor), spell_id, slot)
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn cast_spell_at(
        &self,
        actor: Actor,
        spell_id: u32,
        target_guid: u64,
        x: f32,
        y: f32,
        z: f32,
    ) -> Result<()> {
        let coord = self.0.call_pipe();
        call_reducer!(
            coord.conn.reducers,
            "gw_cast_spell_at",
            gw_cast_spell_at_then(self.session_actor(actor), spell_id, target_guid, x, y, z)
        )
    }

    fn cancel_aura(&self, actor: Actor, spell_id: u32) -> Result<()> {
        let coord = self.0.call_pipe();
        call_reducer!(
            coord.conn.reducers,
            "gw_cancel_aura",
            gw_cancel_aura_then(self.session_actor(actor), spell_id)
        )
    }

    fn cancel_cast(&self, actor: Actor) -> Result<()> {
        let coord = self.0.call_pipe();
        call_reducer!(
            coord.conn.reducers,
            "gw_cancel_cast",
            gw_cancel_cast_then(self.session_actor(actor))
        )
    }

    fn disenchant_item(&self, actor: Actor, slot: u8) -> Result<()> {
        let coord = self.0.call_pipe();
        call_reducer!(
            coord.conn.reducers,
            "gw_disenchant",
            gw_disenchant_then(self.session_actor(actor), slot)
        )
    }

    fn enchant_item_on_slot(&self, actor: Actor, slot: u8, enchant_id: u32) -> Result<()> {
        let coord = self.0.call_pipe();
        call_reducer!(
            coord.conn.reducers,
            "gw_enchant_item",
            gw_enchant_item_then(self.session_actor(actor), slot, enchant_id)
        )
    }

    fn fish(&self, actor: Actor) -> Result<()> {
        let coord = self.0.call_pipe();
        call_reducer!(
            coord.conn.reducers,
            "gw_fish",
            gw_fish_then(self.session_actor(actor))
        )
    }

    fn pick_lock(&self, actor: Actor, go_guid: u64) -> Result<()> {
        let coord = self.0.call_pipe();
        call_reducer!(
            coord.conn.reducers,
            "gw_pick_lock",
            gw_pick_lock_then(self.session_actor(actor), go_guid)
        )
    }

    fn item_slot_by_guid(&self, owner_guid: u64, item_guid: u64) -> Option<u8> {
        Coordinator::item_slot_by_guid(self, owner_guid, item_guid)
    }

    fn player_items(&self, owner_guid: u64) -> Result<Vec<codec::ItemInstanceView>> {
        Coordinator::player_items(self, owner_guid)
    }
}
