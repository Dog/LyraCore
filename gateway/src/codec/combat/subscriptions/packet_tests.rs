use super::*;
use crate::stdb::bindings::{CombatEvent, SpellCastEvent};
use crate::stdb::world_view::test_support::{queued_job, registered_viewer};
use crate::world::Outbound;
use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use wow_world_messages::vanilla::opcodes::ServerOpcodeMessage;

const BYSTANDER: u64 = 99;

fn shared_packets(out: &[Outbound]) -> Vec<(u16, &Arc<[u8]>)> {
    out.iter()
        .map(|out| match out {
            Outbound::Shared { opcode, body } => (*opcode, body),
            _ => panic!("a notification Relay must share encoded packet bodies"),
        })
        .collect()
}

#[test]
fn swing_bodies_are_shared_after_each_viewers_create_gate_and_the_kill_line_stays_private() {
    let view = WorldView::new(true);
    let row = CombatEvent {
        killing_blow: true,
        ..swing_event()
    };
    let (source, source_rx) = registered_viewer(&view, 0, row.attacker_guid);
    let (peer, peer_rx) = registered_viewer(&view, 0, BYSTANDER);
    let (_, hidden_rx) = registered_viewer(&view, 0, BYSTANDER + 1);
    source.created.lock().unwrap().insert(row.attacker_guid);
    peer.created.lock().unwrap().insert(row.attacker_guid);

    combat_event_appeared(&view, 0, &row);
    assert!(queued_job(&hidden_rx).is_empty());
    let source_out = queued_job(&source_rx);
    let peer_out = queued_job(&peer_rx);
    let source_packets = shared_packets(&source_out);
    let peer_packets = shared_packets(&peer_out);
    assert_eq!(
        source_packets
            .iter()
            .map(|(opcode, _)| *opcode)
            .collect::<Vec<_>>(),
        [0x014a, 0x0144, 0x01f5]
    );
    assert_eq!(
        peer_packets
            .iter()
            .map(|(opcode, _)| *opcode)
            .collect::<Vec<_>>(),
        [0x014a, 0x0144]
    );
    for (source_packet, peer_packet) in source_packets.iter().zip(peer_packets) {
        assert!(Arc::ptr_eq(source_packet.1, peer_packet.1));
    }
}

#[test]
fn a_timed_cast_shares_go_and_damage_bodies_after_the_casters_private_result() {
    let view = WorldView::new(true);
    let row = SpellCastEvent {
        is_completion: true,
        damage: 23,
        ..spell_cast_event(2)
    };
    let (_, source_rx) = registered_viewer(&view, 0, row.caster_guid);
    let (_, peer_rx) = registered_viewer(&view, 0, BYSTANDER);

    cast_event_appeared(&view, 0, &row);
    let source_out = queued_job(&source_rx);
    let peer_out = queued_job(&peer_rx);
    let source_packets = shared_packets(&source_out);
    let peer_packets = shared_packets(&peer_out);
    assert_eq!(
        source_packets
            .iter()
            .map(|(opcode, _)| *opcode)
            .collect::<Vec<_>>(),
        [0x0130, 0x0132, 0x0250]
    );
    assert_eq!(
        peer_packets
            .iter()
            .map(|(opcode, _)| *opcode)
            .collect::<Vec<_>>(),
        [0x0132, 0x0250]
    );
    for (source_packet, peer_packet) in source_packets.iter().skip(1).zip(peer_packets) {
        assert!(Arc::ptr_eq(source_packet.1, peer_packet.1));
    }
}

#[test]
fn a_shared_interruption_keeps_caster_failure_and_observer_teardown_separate() {
    let view = WorldView::new(true);
    let row = interrupted_cast(78, NO_POWER_CODE);
    let (_, source_rx) = registered_viewer(&view, 0, row.caster_guid);
    let (_, peer_rx) = registered_viewer(&view, 0, BYSTANDER);
    let (_, other_rx) = registered_viewer(&view, 0, BYSTANDER + 1);

    cast_event_appeared(&view, 0, &row);
    let source_out = queued_job(&source_rx);
    let peer_out = queued_job(&peer_rx);
    let other_out = queued_job(&other_rx);
    let source_packets = shared_packets(&source_out);
    let peer_packets = shared_packets(&peer_out);
    let other_packets = shared_packets(&other_out);
    assert_eq!(
        source_packets
            .iter()
            .map(|(opcode, _)| *opcode)
            .collect::<Vec<_>>(),
        [0x0133, 0x0130]
    );
    assert_eq!(source_packets[1].1.as_ref(), &[78, 0, 0, 0, 0x02, 0x4d]);
    assert_eq!(
        peer_packets
            .iter()
            .map(|(opcode, _)| *opcode)
            .collect::<Vec<_>>(),
        [0x02a6]
    );
    assert!(Arc::ptr_eq(peer_packets[0].1, other_packets[0].1));
}

#[test]
fn the_instant_caster_skips_repeated_launch_packets_but_shares_the_cooldown() {
    let view = WorldView::new(true);
    let row = SpellCastEvent {
        client_initiated: true,
        miss_info: 2,
        cooldown_ms: 5000,
        ..spell_cast_event(2)
    };
    let (_, source_rx) = registered_viewer(&view, 0, row.caster_guid);
    let (_, peer_rx) = registered_viewer(&view, 0, BYSTANDER);

    cast_event_appeared(&view, 0, &row);
    let source_out = queued_job(&source_rx);
    let peer_out = queued_job(&peer_rx);
    let source_packets = shared_packets(&source_out);
    let peer_packets = shared_packets(&peer_out);
    assert_eq!(
        source_packets
            .iter()
            .map(|(opcode, _)| *opcode)
            .collect::<Vec<_>>(),
        [codec::SMSG_SPELLLOGMISS_OPCODE, 0x0134]
    );
    assert_eq!(
        peer_packets
            .iter()
            .map(|(opcode, _)| *opcode)
            .collect::<Vec<_>>(),
        [0x0131, 0x0132, 0x0134]
    );
    assert!(Arc::ptr_eq(source_packets[1].1, peer_packets[2].1));
}

#[test]
fn spell_impact_viewers_share_the_damage_body() {
    let view = WorldView::new(true);
    let row = SpellImpactEvent {
        id: 1,
        caster_guid: 7,
        target_guid: 11,
        spell_id: 133,
        created_at: spacetimedb_sdk::Timestamp::UNIX_EPOCH,
        damage: 23,
        school: 2,
        is_crit: false,
        resisted: 0,
        absorbed: 0,
        map_id: 0,
        instance_id: 0,
        grid_x: 0,
        grid_y: 0,
    };
    let (_, source_rx) = registered_viewer(&view, 0, row.caster_guid);
    let (_, peer_rx) = registered_viewer(&view, 0, BYSTANDER);
    impact_appeared(&view, 0, &row);
    let source_out = queued_job(&source_rx);
    let peer_out = queued_job(&peer_rx);
    let source_packets = shared_packets(&source_out);
    let peer_packets = shared_packets(&peer_out);
    assert_eq!(source_packets.len(), 1);
    assert_eq!(source_packets[0].0, 0x0250);
    assert_eq!(peer_packets.len(), 1);
    assert!(Arc::ptr_eq(source_packets[0].1, peer_packets[0].1));
}

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
        let out = combat_event_outbound(&created, BYSTANDER, &row);
        let [Outbound::One(ServerOpcodeMessage::SMSG_SPELL_GO(go))] = out.as_slice() else {
            panic!("a ranged launch must emit exactly one SPELL_GO");
        };
        assert_eq!((go.caster.guid(), go.spell), (7, 75));
        assert_eq!((go.hits.len(), go.misses.len()), (hits, misses));
        assert!(combat_event_outbound(&Mutex::new(HashSet::new()), BYSTANDER, &row).is_empty());
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
        miss_info: 0,
        energized: 0,
        power_type: 0,
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
    let out = combat_event_outbound(&created, BYSTANDER, &row);
    assert!(matches!(
        out.as_slice(),
        [Outbound::One(
            ServerOpcodeMessage::SMSG_ATTACKERSTATEUPDATE(_)
        )]
    ));

    row.killing_blow = true;
    let out = combat_event_outbound(&created, BYSTANDER, &row);
    assert!(matches!(
        out.as_slice(),
        [
            Outbound::One(ServerOpcodeMessage::SMSG_ATTACKERSTATEUPDATE(_)),
            Outbound::One(ServerOpcodeMessage::SMSG_ATTACKSTOP(_))
        ]
    ));
}

const HEROIC_STRIKE_CASTER: u64 = 7;

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
        miss_info: 0,
        energized: 0,
        power_type: 0,
    }
}

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
            assert_eq!(body, &vec![78, 0, 0, 0, 0x02, 0x4D]);
        }
        _ => panic!("expected the raw failed cast result second"),
    }
}

#[test]
fn a_bystander_sees_only_the_observer_teardown_of_a_deferred_power_failure() {
    let out = cast_event_outbound(
        HEROIC_STRIKE_CASTER + 1,
        &interrupted_cast(78, NO_POWER_CODE),
    );
    let [Outbound::One(message @ ServerOpcodeMessage::SMSG_SPELL_FAILED_OTHER(_))] = out.as_slice()
    else {
        panic!("expected the observer teardown alone");
    };
    let mut bytes = Vec::new();
    message.write_unencrypted_server(&mut bytes).unwrap();
    // Size 14 (opcode + 12-byte body), opcode 0x02A6, full caster guid, spell 78.
    let mut want = vec![0x00, 0x0E, 0xA6, 0x02];
    want.extend_from_slice(&HEROIC_STRIKE_CASTER.to_le_bytes());
    want.extend_from_slice(&78u32.to_le_bytes());
    assert_eq!(bytes, want);
}

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
    assert_eq!(stealth_action(true, 1), StealthAction::Hide);
    assert_eq!(stealth_action(true, 2), StealthAction::Hide);
    assert_eq!(stealth_action(true, 3), StealthAction::Hide);
    assert_eq!(stealth_action(false, 1), StealthAction::None);
    assert_eq!(stealth_action(false, 0), StealthAction::Reveal);
    assert_eq!(stealth_action(false, 2), StealthAction::None);
    assert_eq!(stealth_action(true, 0), StealthAction::None);
}

#[test]
fn stealth_dedup_composes_with_a_real_hashset_exactly_once() {
    let target = 99u64;
    let mut created: HashSet<u64> = HashSet::from([target]);
    let mut destroys = 0;
    for count in [2, 2] {
        if stealth_action(true, count) == StealthAction::Hide && created.remove(&target) {
            destroys += 1;
        }
    }
    assert_eq!(destroys, 1, "two HIDE decisions must destroy exactly once");
    assert!(!created.contains(&target));
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
