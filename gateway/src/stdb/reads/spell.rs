use lyracore_shared::spell::taxonomy::SPELL_ATTR_PASSIVE;

use super::super::bindings::*;

pub(in crate::stdb) fn spell_ranks_stack_in_book(db: &RemoteTables, spell_id: u32) -> bool {
    const POWER_MANA: u8 = 0;
    match db.game_spell().spell_id().find(&spell_id) {
        Some(h) => h.attributes & SPELL_ATTR_PASSIVE == 0 && h.power_type == POWER_MANA,
        None => true,
    }
}
