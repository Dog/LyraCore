pub mod taxonomy;

pub use taxonomy::SPELL_ATTR_CHANNELED;

// Periodic triggers preserve legacy channel behavior when the header has no flag.
pub fn is_channel_aura(effect_kind: u8, cast_flags: u32) -> bool {
    effect_kind == taxonomy::A_PERIODIC_TRIGGER || cast_flags & SPELL_ATTR_CHANNELED != 0
}
