//! Combat — melee auto-attack, **bidirectional** with **per-unit attack speed**: each unit swings on
//! its own `base_attack_time_ms`, and a creature that is attacked **retaliates** and swings back at its
//! own speed. Each swing writes a `game_combat_event` row for the gateway to relay as
//! `SMSG_ATTACKERSTATEUPDATE` (swing animation + damage text); the health drop flows through the
//! `game_world_entity` `on_update` → VALUES relay. A creature target dies at 0 HP (DESTROY + respawn);
//! a **player** target also dies at 0 HP (health=0 + `dead` → the death/release/ghost corpse-run, see
//! `world::repop`). [entity]/[event]/[server]
//!
//! Submodules (split along this file's own comment banners, on top of the shared damage
//! pipeline):
//!   - `tables` — the ctx-free hit-table/damage/regen math (pure formulas, exhaustive tests; unchanged
//!     by the split — it is the model file for the module).
//!   - `folds` — effective-value folds over aura/gear (dodge/parry/block/crit/miss/swing-time/armor/
//!     strength/agility), seal-on-swing + queued-strike, the react-window, equipped-weapon/ammo
//!     lookups, and the `roll_swing`/`roll_ranged_swing` family.
//!   - `death` — `kill_creature`/`kill_player` (the two death chokepoints) + the shared damage
//!     pipeline (`fold_incoming_damage`/`final_damage`/`apply_hit`) every damage resolver routes
//!     through.
//!   - `engage` — the engagement model: enter/disengage/queries over `game_melee_attack`, the
//!     engagement tables, and the start/stop-attack reducers.
//!   - `swing` — the tick passes (`tick_melee`'s leash/aggro/resolve) + the resolvers that fire a hit
//!     (`fire_melee_swing`/`resolve_offhand_swing`/`fire_ranged_shot`/`ranged_impact`).
//!
//! Named exports below hold the operations and tables shared with other Module files.

pub(crate) mod death;
mod engage;
mod folds;
mod swing;
mod tables;

pub(crate) use death::{
    apply_hit, clear_lethal_damage_floor, clear_stale_lethal_damage_floor, commit_death_prevention,
    final_damage, fold_incoming_damage, force_creature_death, is_lethal, kill_creature,
    kill_player, lethal_floor_protects, set_lethal_damage_floor, Hit, HitSource,
    CORPSE_DECAY_MICROS, DRAIN_SOUL_SPELL_ID, RESPAWN_MICROS, SOUL_SHARD_ENTRY,
};

#[cfg(test)]
pub(crate) use engage::COMBAT_DROP_MS;
pub(crate) use engage::{
    apply_start_attack, apply_start_ranged_attack, arm_creature_engagement, break_own_attacks,
    disengage, enter_combat, is_engaged, is_hostile_target, may_harm, may_help,
    melee_combatant_guids, refresh_leash, request_attack, stop_attack_for, stop_duel_combat,
    validate_attack_target,
};
pub use engage::{
    game_combat_event, game_melee_attack, game_melee_schedule, game_ranged_impact_schedule,
    AttackStart, CombatEvent, MeleeAttack, MeleeSchedule, RangedImpactSchedule,
};
pub(crate) use folds::{
    arm_react, aura_attack_power_bonus, clear_queued_strike, effective_agility, effective_crit_bp,
    effective_move_speed, effective_strength, effective_swing_time, equipped_offhand_weapon_damage,
    equipped_ranged_weapon, find_ammo, queued_strike_on_swing, react_window_active,
    retaliate_on_hit, roll_money, roll_ranged_swing, roll_swing, roll_swing_with_range,
    seal_holy_on_swing, swing_range_ctx, weapon_strike_damage, wear_weapon, ReactKind,
};
#[cfg(feature = "debug_reducers")]
pub(crate) use folds::{
    effective_armor, effective_block_bp, effective_block_value, effective_dodge_bp,
    effective_miss_bp, effective_parry_bp,
};

pub use swing::{ranged_impact, tick_melee};
pub(crate) use tables::{
    agility_crit_bp, apply_offhand_penalty, armor_mitigation_pct, attack_outcome, block_chance_bp,
    crit_bp_with_bonus, crushing_chance_bp, dodge_chance_bp, effective_stat, glancing_chance_bp,
    is_rage_user, is_translating, melee_attack_power_for, miss_bp_with_bonus, miss_chance_bp,
    move_speed_with_pct, parry_chance_bp, player_swing_range_ap, rage_from_damage,
    ranged_sheet_values, regen_entity_health, regen_entity_health_in_combat, regen_entity_power,
    resist_mitigation_pct, scale_swing_for_rank, swing_time_with_haste, weapon_swing_range_ap,
    AttackTable, RegenWindow, AUTO_SHOT_PROJECTILE_SPEED, COMBAT_BACKSTOP_SQ, CREATURE_MELEE_MAX,
    CREATURE_MELEE_MIN, CRIT_MULT, CRUSH_DAMAGE_PCT, GLANCING_DAMAGE_PCT, HIT_BLOCK, HIT_CRIT,
    HIT_CRUSHING, HIT_DODGE, HIT_GLANCING, HIT_MISS, HIT_NORMAL, HIT_PARRY, LEASH_RADIUS_SQ,
    MELEE_LEEWAY_WINDOW_MS, MELEE_RANGE_LEEWAY_CREATURE_SQ, MELEE_RANGE_LEEWAY_SQ, MELEE_RANGE_SQ,
    MOVE_FLAG_FORWARD, MOVE_MASK_MOVING, PURSUIT_WINDOW_MS, RANGED_INITIAL_SHOT_MS,
    RANGED_RANGE_SQ, WAND_PROJECTILE_SPEED,
};
#[cfg(feature = "debug_reducers")]
pub(crate) use tables::{regen_health_in_combat, swing_profile};

pub use tables::DEFAULT_ATTACK_TIME_MS;
