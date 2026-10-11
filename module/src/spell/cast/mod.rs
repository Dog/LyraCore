//! Targeting + effect dispatch + the cast core (split of the old flat 2,999-line `cast.rs`):
//! - `targeting` — `select_targets` / `aura_apply` / `apply_effect` and the per-kind effect handlers.
//! - `resolve`   — the cast core: `resolve_cast_at` (+ its extracted `check_cast_gates` gate sweep) /
//!   `resolve_cast` / `begin_cast` / the passive-apply path.
//!
//! The cast-bar teardown family (`pushback_cast` / `interrupt_cast` / `break_channel` /
//! `interrupt_cast_and_lock`) moved OUT of this module entirely, into `control.rs` — it already called
//! back into `pushback_cast` (the on-damage chokepoint), so co-locating it with the rest of the
//! crowd-control model removes a cross-module callback instead of adding one.
//!
//! Resolve and targeting share operations through the named exports here and in `spell::mod`.
//! Resolve calls targeting's effect dispatch, and triggered effects call back into resolve.

pub(crate) mod resolve;
mod targeting;

#[cfg(test)]
pub(crate) use resolve::passive_applies_effect_kind;
#[cfg(feature = "debug_reducers")]
pub(crate) use resolve::resolve_cast;
pub(crate) use resolve::{
    apply_spell_auras, begin_cast, begin_cast_with_admission, cast_triggered,
    check_cast_start_gates, check_pending_cast_los, clear_dead_callback_cast_admission,
    resolve_cast_at, resolve_cast_at_typed, start_creature_spell, CreatureSpellCasterAdmission,
    CreatureSpellStart, CreatureSpellStartMode, CreatureSpellTarget,
};
#[cfg(feature = "debug_reducers")]
pub(crate) use targeting::blink_forward;
pub(crate) use targeting::{
    apply_direct_heal, apply_effect, aura_apply, dynamic_object_guid, faction_target_violation,
    recipe_reagents, revive_amount, select_targets, ItemGrantOutcome,
};
