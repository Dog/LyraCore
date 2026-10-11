use super::*;
use crate::stdb::bindings::{CombatEvent, SpellCastEvent};
use crate::world::Outbound;
use std::collections::HashSet;
use std::sync::Mutex;
use wow_world_messages::vanilla::opcodes::ServerOpcodeMessage;

fn swing_event() -> CombatEvent {
    CombatEvent {
        id: 1,
        attacker_guid: 7,
        target_guid: 11,
        damage: 23,
        hit_info: 0,
        killing_blow: false,
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
    }
}

#[test]
fn ranged_launch_relays_one_spell_go_for_a_hit_or_miss() {
    let created = Mutex::new(HashSet::from([7]));
    for (hit_info, hits, misses) in [(0, 1, 0), (2, 0, 1)] {
        let row = CombatEvent {
            damage: 0,
            hit_info,
            ranged_spell_id: 75,
            ammo_display_id: 5996,
            impact_delay_ms: 500,
            ..swing_event()
        };
        let out = combat_event_outbound(&created, &row);
        let [Outbound::One(ServerOpcodeMessage::SMSG_SPELL_GO(go))] = out.as_slice() else {
            panic!("a ranged launch must emit exactly one SPELL_GO");
        };
        assert_eq!((go.caster.guid(), go.spell), (7, 75));
        assert_eq!((go.hits.len(), go.misses.len()), (hits, misses));
        assert!(combat_event_outbound(&Mutex::new(HashSet::new()), &row).is_empty());
    }
}

#[test]
fn ranged_impact_relays_only_its_authoritative_damage() {
    let row = SpellCastEvent {
        id: 1,
        caster_guid: 7,
        spell_id: 75,
        created_at: spacetimedb_sdk::Timestamp::UNIX_EPOCH,
        target_guid: 11,
        cast_time_ms: 0,
        is_completion: false,
        damage: 23,
        school: 0,
        is_crit: false,
        resisted: 0,
        absorbed: 0,
        is_interrupted: false,
        cooldown_ms: 0,
        delay_ms: 0,
        healed: 0,
        is_proc_log: true,
        swing_hit_info: 0,
        client_initiated: false,
        map_id: 0,
        instance_id: 0,
        grid_x: 0,
        grid_y: 0,
        failure_reason: 0,
        kind: 5,
    };
    let out = cast_event_outbound(7, &row);
    let [Outbound::One(ServerOpcodeMessage::SMSG_SPELLNONMELEEDAMAGELOG(log))] = out.as_slice()
    else {
        panic!("an impact must emit exactly one damage log");
    };
    assert_eq!(
        (
            log.attacker.guid(),
            log.target.guid(),
            log.spell,
            log.damage
        ),
        (7, 11, 75, 23)
    );
}

#[test]
fn melee_swing_relays_damage_then_stops_on_a_killing_blow() {
    let created = Mutex::new(HashSet::from([7]));
    let mut row = swing_event();
    let out = combat_event_outbound(&created, &row);
    assert!(matches!(
        out.as_slice(),
        [Outbound::One(
            ServerOpcodeMessage::SMSG_ATTACKERSTATEUPDATE(_)
        )]
    ));

    row.killing_blow = true;
    let out = combat_event_outbound(&created, &row);
    assert!(matches!(
        out.as_slice(),
        [
            Outbound::One(ServerOpcodeMessage::SMSG_ATTACKERSTATEUPDATE(_)),
            Outbound::One(ServerOpcodeMessage::SMSG_ATTACKSTOP(_))
        ]
    ));
}

const HEROIC_STRIKE_CASTER: u64 = 7;

/// The module's `CAST_FAIL_NO_POWER`.
const NO_POWER_CODE: u8 = 1;

fn spell_cast_event(kind: u8) -> SpellCastEvent {
    SpellCastEvent {
        id: 1,
        caster_guid: HEROIC_STRIKE_CASTER,
        spell_id: 133,
        created_at: spacetimedb_sdk::Timestamp::UNIX_EPOCH,
        target_guid: 11,
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
    }
}

/// An interruption signal row for `spell_id` cast by `HEROIC_STRIKE_CASTER`, carrying
/// `failure_reason`. Every other field stays at the module baseline's zero, which is what such a
/// row really looks like on the wire.
fn interrupted_cast(spell_id: u32, failure_reason: u8) -> SpellCastEvent {
    SpellCastEvent {
        spell_id,
        target_guid: 0,
        is_interrupted: true,
        failure_reason,
        ..spell_cast_event(3)
    }
}

#[test]
fn explicit_spell_cast_event_kinds_override_legacy_discriminators() {
    let contradictory = |kind| SpellCastEvent {
        cast_time_ms: 1_500,
        is_completion: true,
        damage: 23,
        is_interrupted: true,
        delay_ms: 500,
        is_proc_log: true,
        kind,
        ..spell_cast_event(kind)
    };

    assert!(matches!(
        cast_event_outbound(HEROIC_STRIKE_CASTER, &contradictory(1)).as_slice(),
        [Outbound::One(ServerOpcodeMessage::SMSG_SPELL_START(_))]
    ));
    assert!(matches!(
        cast_event_outbound(HEROIC_STRIKE_CASTER + 1, &contradictory(2)).as_slice(),
        [
            Outbound::One(ServerOpcodeMessage::SMSG_SPELL_GO(_)),
            Outbound::One(ServerOpcodeMessage::SMSG_SPELLNONMELEEDAMAGELOG(_))
        ]
    ));

    let interrupt = SpellCastEvent {
        kind: 3,
        ..spell_cast_event(3)
    };
    assert!(matches!(
        cast_event_outbound(HEROIC_STRIKE_CASTER, &interrupt).as_slice(),
        [Outbound::One(ServerOpcodeMessage::SMSG_SPELL_FAILURE(_))]
    ));

    let pushback = SpellCastEvent {
        delay_ms: 500,
        kind: 4,
        ..spell_cast_event(4)
    };
    assert!(matches!(
        cast_event_outbound(HEROIC_STRIKE_CASTER, &pushback).as_slice(),
        [Outbound::One(ServerOpcodeMessage::SMSG_SPELL_DELAYED(_))]
    ));

    let proc_log = SpellCastEvent {
        damage: 23,
        kind: 5,
        ..spell_cast_event(5)
    };
    assert!(matches!(
        cast_event_outbound(HEROIC_STRIKE_CASTER, &proc_log).as_slice(),
        [Outbound::One(
            ServerOpcodeMessage::SMSG_SPELLNONMELEEDAMAGELOG(_)
        )]
    ));
}

#[test]
fn legacy_spell_cast_event_rows_keep_the_field_based_decoder() {
    let start = SpellCastEvent {
        cast_time_ms: 1_500,
        ..spell_cast_event(0)
    };
    assert!(matches!(
        cast_event_outbound(HEROIC_STRIKE_CASTER, &start).as_slice(),
        [Outbound::One(ServerOpcodeMessage::SMSG_SPELL_START(_))]
    ));

    let go = spell_cast_event(0);
    assert!(matches!(
        cast_event_outbound(HEROIC_STRIKE_CASTER + 1, &go).as_slice(),
        [
            Outbound::One(ServerOpcodeMessage::SMSG_SPELL_START(_)),
            Outbound::One(ServerOpcodeMessage::SMSG_SPELL_GO(_))
        ]
    ));

    let interrupt = SpellCastEvent {
        is_interrupted: true,
        ..spell_cast_event(0)
    };
    assert!(matches!(
        cast_event_outbound(HEROIC_STRIKE_CASTER, &interrupt).as_slice(),
        [Outbound::One(ServerOpcodeMessage::SMSG_SPELL_FAILURE(_))]
    ));

    let pushback = SpellCastEvent {
        delay_ms: 500,
        ..spell_cast_event(0)
    };
    assert!(matches!(
        cast_event_outbound(HEROIC_STRIKE_CASTER, &pushback).as_slice(),
        [Outbound::One(ServerOpcodeMessage::SMSG_SPELL_DELAYED(_))]
    ));

    let proc_log = SpellCastEvent {
        damage: 23,
        is_proc_log: true,
        ..spell_cast_event(0)
    };
    assert!(matches!(
        cast_event_outbound(HEROIC_STRIKE_CASTER, &proc_log).as_slice(),
        [Outbound::One(
            ServerOpcodeMessage::SMSG_SPELLNONMELEEDAMAGELOG(_)
        )]
    ));

    let old_precedence = SpellCastEvent {
        cast_time_ms: 1_500,
        is_interrupted: true,
        delay_ms: 500,
        is_proc_log: true,
        ..spell_cast_event(0)
    };
    assert!(matches!(
        cast_event_outbound(HEROIC_STRIKE_CASTER, &old_precedence).as_slice(),
        [Outbound::One(ServerOpcodeMessage::SMSG_SPELL_FAILURE(_))]
    ));
}

#[test]
fn unknown_spell_cast_event_kind_emits_no_packet() {
    let row = SpellCastEvent {
        is_interrupted: true,
        delay_ms: 500,
        is_proc_log: true,
        kind: u8::MAX,
        ..spell_cast_event(u8::MAX)
    };
    assert!(cast_event_outbound(HEROIC_STRIKE_CASTER, &row).is_empty());
}

/// A queued strike that could not pay its cost at the swing: the caster gets the cast-bar
/// teardown and then the failed cast result naming the queued spell with the vanilla Not Enough
/// Power reason (0x4D). No SMSG_SPELL_GO — the strike never fired.
#[test]
fn a_deferred_power_failure_relays_the_teardown_then_the_failed_cast_result() {
    let out = cast_event_outbound(HEROIC_STRIKE_CASTER, &interrupted_cast(78, NO_POWER_CODE));

    assert_eq!(out.len(), 2, "expected exactly the two vanilla messages");
    match &out[0] {
        Outbound::One(ServerOpcodeMessage::SMSG_SPELL_FAILURE(m)) => {
            assert_eq!(m.guid.guid(), HEROIC_STRIKE_CASTER);
            assert_eq!(m.spell, 78);
        }
        _ => panic!("expected the interruption teardown first"),
    }
    match &out[1] {
        Outbound::Raw { opcode, body } => {
            assert_eq!(*opcode, 0x0130);
            // spell(u32 LE) + CAST_FAILED(0x02) + SPELL_FAILED_NO_POWER(0x4D).
            assert_eq!(body, &vec![78, 0, 0, 0, 0x02, 0x4D]);
        }
        _ => panic!("expected the raw failed cast result second"),
    }
}

/// The failed cast result is caster-private: a bystander watching the same row gets nothing.
#[test]
fn a_bystander_sees_neither_half_of_a_deferred_power_failure() {
    let out = cast_event_outbound(
        HEROIC_STRIKE_CASTER + 1,
        &interrupted_cast(78, NO_POWER_CODE),
    );
    assert!(out.is_empty(), "both messages are caster-private");
}

/// An interruption with no concrete reason (a damaged mid-cast timed spell) still sends the
/// teardown alone — the cast result belongs to a failure the server can name.
#[test]
fn an_ordinary_interruption_still_sends_the_teardown_alone() {
    let out = cast_event_outbound(HEROIC_STRIKE_CASTER, &interrupted_cast(133, 0));
    assert_eq!(out.len(), 1, "expected the teardown alone");
    assert!(matches!(
        &out[0],
        Outbound::One(ServerOpcodeMessage::SMSG_SPELL_FAILURE(_))
    ));
}

#[test]
fn stealth_both_inserts_hide_both_deletes_reveal_created_dedupes() {
    // A Stealth cast applies TWO A_STEALTH auras and the SDK fires both inserts AFTER applying the
    // delta, so both see post-count 2 -> both HIDE (the per-viewer `created` set makes the 2nd a
    // no-op at the call site). A break removes both -> two deletes seeing post-count 0 -> both REVEAL
    // (created-deduped to one). The decision is purely "stealthed -> Hide, not stealthed -> Reveal".
    assert_eq!(stealth_action(true, 1), StealthAction::Hide);
    assert_eq!(stealth_action(true, 2), StealthAction::Hide);
    assert_eq!(stealth_action(true, 3), StealthAction::Hide);
    assert_eq!(stealth_action(false, 1), StealthAction::None); // still stealthed after one of two deletes
    assert_eq!(stealth_action(false, 0), StealthAction::Reveal);
    assert_eq!(stealth_action(false, 2), StealthAction::None); // over-count delete does not reveal
                                                               // An insert can never leave count 0 (it adds one) -> defensively a no-op.
    assert_eq!(stealth_action(true, 0), StealthAction::None);
}

#[test]
fn stealth_dedup_composes_with_a_real_hashset_exactly_once() {
    // `stealth_visibility` pairs `stealth_action`'s Hide/Reveal decision with `created`'s
    // remove()/insert() return value (only act if the set's membership actually changed) — this
    // composes that SAME discipline over a real `std::collections::HashSet<u64>` (not a mock) to
    // pin the "exactly once" guarantee a Stealth cast's two A_STEALTH inserts/deletes rely on.
    let target = 99u64;
    let mut created: HashSet<u64> = HashSet::from([target]); // starts visible
    let mut destroys = 0;
    // A Stealth cast applies TWO A_STEALTH auras; the SDK fires both inserts AFTER applying the
    // whole delta, so both see post-count 2 -> both decide Hide.
    for count in [2, 2] {
        if stealth_action(true, count) == StealthAction::Hide && created.remove(&target) {
            destroys += 1;
        }
    }
    assert_eq!(destroys, 1, "two HIDE decisions must destroy exactly once");
    assert!(!created.contains(&target));
    // A break removes both auras; both deletes see post-count 0 -> both decide Reveal.
    let mut creates = 0;
    for count in [0, 0] {
        if stealth_action(false, count) == StealthAction::Reveal && created.insert(target) {
            creates += 1;
        }
    }
    assert_eq!(
        creates, 1,
        "two REVEAL decisions must (re)create exactly once"
    );
    assert!(created.contains(&target));
}
