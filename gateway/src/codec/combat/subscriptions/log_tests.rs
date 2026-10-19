use super::*;
use crate::stdb::bindings::{MeleeAttack, SpellCastEvent};
use crate::world::Outbound;
use std::sync::Mutex;

const CASTER: u64 = 7;
const TARGET: u64 = 11;
const WOLF: u64 = 0xF130_00C7_3800_0001;

/// Big-endian size (body plus opcode), little-endian opcode, then the body.
fn frame(out: &Outbound) -> Vec<u8> {
    match out {
        Outbound::One(message) => {
            let mut bytes = Vec::new();
            message.write_unencrypted_server(&mut bytes).unwrap();
            bytes
        }
        Outbound::Raw { opcode, body } => {
            let mut bytes = ((body.len() + 2) as u16).to_be_bytes().to_vec();
            bytes.extend_from_slice(&opcode.to_le_bytes());
            bytes.extend_from_slice(body);
            bytes
        }
        _ => panic!("expected a single message"),
    }
}

fn frames(out: &[Outbound]) -> Vec<Vec<u8>> {
    out.iter().map(frame).collect()
}

fn header(size: u16, opcode: u16) -> Vec<u8> {
    let mut bytes = size.to_be_bytes().to_vec();
    bytes.extend_from_slice(&opcode.to_le_bytes());
    bytes
}

fn u32s(values: &[u32]) -> Vec<u8> {
    values
        .iter()
        .flat_map(|value| value.to_le_bytes())
        .collect()
}

fn engagement(attacker_guid: u64, swing_error: u8) -> MeleeAttack {
    MeleeAttack {
        attacker_guid,
        target_guid: TARGET,
        last_swing_ms: 0,
        ranged_spell_id: 0,
        last_offhand_swing_ms: 0,
        rout_ends_ms: 0,
        pursuit_ends_ms: 0,
        leash_x: 0.0,
        leash_y: 0.0,
        swing_error,
    }
}

fn cast_row(kind: u8) -> SpellCastEvent {
    SpellCastEvent {
        id: 1,
        caster_guid: CASTER,
        spell_id: 172,
        created_at: spacetimedb_sdk::Timestamp::UNIX_EPOCH,
        target_guid: TARGET,
        cast_time_ms: 0,
        is_completion: false,
        damage: 0,
        school: 0,
        is_crit: false,
        resisted: 0,
        absorbed: 0,
        is_interrupted: false,
        cooldown_ms: 0,
        delay_ms: 0,
        healed: 0,
        is_proc_log: false,
        swing_hit_info: 0,
        client_initiated: false,
        map_id: 0,
        instance_id: 0,
        grid_x: 0,
        grid_y: 0,
        failure_reason: 0,
        kind,
        miss_info: 0,
        energized: 0,
        power_type: 0,
    }
}

const PERIODIC_LOG: u8 = 6;
const GO: u8 = 2;

/// Packed target 11, packed caster 7, spell, count 1.
fn periodic_head(spell_id: u32) -> Vec<u8> {
    let mut body = vec![0x01, TARGET as u8, 0x01, CASTER as u8];
    body.extend(u32s(&[spell_id, 1]));
    body
}

#[test]
fn a_swing_error_change_sends_its_message_once_and_a_cleared_error_sends_nothing() {
    let none = engagement(CASTER, 0);
    let far = engagement(CASTER, 1);
    let behind = engagement(CASTER, 2);
    let sent =
        |old: &MeleeAttack, row: &MeleeAttack| swing_error_outbound(old, row).map(|o| frame(&o));
    // Empty bodies: size 2 is the opcode alone.
    assert_eq!(sent(&none, &far), Some(header(2, 0x0145)));
    assert_eq!(sent(&far, &behind), Some(header(2, 0x0146)));
    assert_eq!(sent(&far, &far), None, "an unchanged error is not repeated");
    assert_eq!(sent(&behind, &none), None, "a landed swing clears silently");
}

#[test]
fn a_creature_engagement_plays_the_aggro_reaction_before_the_attack_start() {
    let created = Mutex::new(rustc_hash::FxHashSet::from_iter([WOLF, CASTER]));
    let out = frames(&melee_engage_outbound(&created, &engagement(WOLF, 0), true));
    let mut reaction = header(14, 0x013C);
    reaction.extend_from_slice(&WOLF.to_le_bytes());
    reaction.extend(u32s(&[2])); // AI_REACTION_HOSTILE
    assert_eq!(out.len(), 2);
    assert_eq!(out[0], reaction);
    assert_eq!(
        &out[1][2..4],
        &0x0143u16.to_le_bytes(),
        "then SMSG_ATTACKSTART"
    );

    let player = frames(&melee_engage_outbound(
        &created,
        &engagement(CASTER, 0),
        true,
    ));
    assert_eq!(player.len(), 1);
    assert_eq!(&player[0][2..4], &0x0143u16.to_le_bytes());
}

#[test]
fn a_replayed_creature_engagement_restores_the_stance_without_the_aggro_sound() {
    let created = Mutex::new(rustc_hash::FxHashSet::from_iter([WOLF]));
    let out = frames(&melee_engage_outbound(
        &created,
        &engagement(WOLF, 0),
        false,
    ));
    assert_eq!(out.len(), 1, "only the attack start replays");
    assert_eq!(&out[0][2..4], &0x0143u16.to_le_bytes());
}

#[test]
fn the_caster_of_a_synced_instant_learns_its_miss_from_the_miss_log() {
    let row = SpellCastEvent {
        miss_info: 2,
        client_initiated: true,
        ..cast_row(GO)
    };
    let out = frames(&cast_event_outbound(CASTER, &row));
    // Spell, full caster, flag 0, count 1, full target, SPELL_MISS_RESIST (2).
    let mut body = u32s(&[172]);
    body.extend_from_slice(&CASTER.to_le_bytes());
    body.push(0);
    body.extend(u32s(&[1]));
    body.extend_from_slice(&TARGET.to_le_bytes());
    body.push(2);
    let mut want = header(body.len() as u16 + 2, 0x024B);
    want.extend(body);
    assert_eq!(out, vec![want]);

    let observer = frames(&cast_event_outbound(TARGET, &row));
    assert!(observer
        .iter()
        .all(|frame| frame[2..4] != 0x024Bu16.to_le_bytes()));
    assert!(observer
        .iter()
        .any(|frame| frame[2..4] == 0x0132u16.to_le_bytes()));
}

#[test]
fn a_periodic_damage_tick_logs_damage_school_absorb_and_resist_as_words() {
    let row = SpellCastEvent {
        damage: 9,
        school: 5, // shadow
        resisted: 3,
        ..cast_row(PERIODIC_LOG)
    };
    let mut body = periodic_head(172);
    body.extend(u32s(&[3, 9, 5, 0, 3])); // SPELL_AURA_PERIODIC_DAMAGE, damage, school, absorb, resist
    let mut want = header(body.len() as u16 + 2, 0x024E);
    want.extend(body);
    assert_eq!(frames(&cast_event_outbound(TARGET, &row)), vec![want]);
}

#[test]
fn periodic_heal_and_energize_ticks_log_their_own_aura_types() {
    let heal = SpellCastEvent {
        healed: 42,
        ..cast_row(PERIODIC_LOG)
    };
    let mut body = periodic_head(172);
    body.extend(u32s(&[8, 42])); // SPELL_AURA_PERIODIC_HEAL, amount
    let mut want = header(body.len() as u16 + 2, 0x024E);
    want.extend(body);
    assert_eq!(frames(&cast_event_outbound(TARGET, &heal)), vec![want]);

    let energize = SpellCastEvent {
        energized: 10,
        power_type: 1, // rage
        ..cast_row(PERIODIC_LOG)
    };
    let mut body = periodic_head(172);
    body.extend(u32s(&[24, 1, 10])); // SPELL_AURA_PERIODIC_ENERGIZE, power type, amount
    let mut want = header(body.len() as u16 + 2, 0x024E);
    want.extend(body);
    assert_eq!(frames(&cast_event_outbound(TARGET, &energize)), vec![want]);

    assert!(
        cast_event_outbound(TARGET, &cast_row(PERIODIC_LOG)).is_empty(),
        "a tick with no outcome logs nothing"
    );
}

#[test]
fn a_resisted_spell_lists_its_target_as_resisted_in_the_go() {
    let row = SpellCastEvent {
        miss_info: 2,
        school: 5,
        ..cast_row(GO)
    };
    let out = frames(&cast_event_outbound(TARGET, &row));
    // GO: packed item and caster, spell, flags, 0 hits, 1 miss (full guid, RESIST), unit target.
    let mut body = vec![0x01, 0x07, 0x01, 0x07];
    body.extend(u32s(&[172]));
    body.extend_from_slice(&0x0100u16.to_le_bytes());
    body.extend_from_slice(&[0, 1]);
    body.extend_from_slice(&TARGET.to_le_bytes());
    body.push(2);
    body.extend_from_slice(&0x0002u16.to_le_bytes());
    body.extend_from_slice(&[0x01, 0x0B]);
    let mut go = header(body.len() as u16 + 2, 0x0132);
    go.extend(body);
    assert_eq!(out.len(), 2);
    assert_eq!(out[1], go);
}

#[test]
fn a_spell_that_gives_power_sends_the_energize_log_after_the_go() {
    let row = SpellCastEvent {
        energized: 100,
        power_type: 1,
        ..cast_row(GO)
    };
    let out = frames(&cast_event_outbound(TARGET, &row));
    let mut body = vec![0x01, 0x0B, 0x01, 0x07]; // packed target 11, packed caster 7
    body.extend(u32s(&[172, 1, 100])); // spell, rage, amount
    let mut want = header(body.len() as u16 + 2, 0x0151);
    want.extend(body);
    assert_eq!(out.last(), Some(&want));
}

#[test]
fn only_the_killing_player_gets_the_party_kill_line() {
    let created = Mutex::new(rustc_hash::FxHashSet::from_iter([CASTER]));
    let kill = crate::stdb::bindings::CombatEvent {
        id: 1,
        attacker_guid: CASTER,
        target_guid: WOLF,
        damage: 30,
        hit_info: 0,
        killing_blow: true,
        created_at: spacetimedb_sdk::Timestamp::UNIX_EPOCH,
        blocked_amount: 0,
        ranged_spell_id: 0,
        ammo_display_id: 0,
        spell_swing: false,
        impact_delay_ms: 0,
        map_id: 0,
        instance_id: 0,
        grid_x: 0,
        grid_y: 0,
    };
    let killer = frames(&combat_event_outbound(&created, CASTER, &kill));
    let mut line = header(18, 0x01F5);
    line.extend_from_slice(&CASTER.to_le_bytes());
    line.extend_from_slice(&WOLF.to_le_bytes());
    assert_eq!(killer.len(), 3, "swing, attack stop, then the kill line");
    assert_eq!(killer[2], line);

    let bystander = combat_event_outbound(&created, TARGET, &kill);
    assert_eq!(
        bystander.len(),
        2,
        "a bystander sees the swing and the stop only"
    );
}
