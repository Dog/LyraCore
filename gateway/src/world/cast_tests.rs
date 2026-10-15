use super::family::{ProtocolFamily, ProtocolSession};
use super::handlers::InMemoryCasts;
use super::*;
use std::sync::Mutex;

const OP_CAST_RESULT: u16 = 0x0130;

fn run(
    store: &InMemoryCasts,
    session: &mut ProtocolSession,
    msg: impl Into<ClientOpcodeMessage>,
) -> Vec<Outbound> {
    handlers::Cast::handle(store, session, msg.into().into())
        .unwrap()
        .outbound
}

fn repeating(session: &ProtocolSession) -> bool {
    matches!(&session.state, WorldState::InWorld(world) if world.ranged_repeat)
}

fn unit_targets(guid: u64) -> SpellCastTargets {
    SpellCastTargets {
        target_flags: SpellCastTargets_SpellCastTargetFlags::new_unit(
            SpellCastTargets_SpellCastTargetFlags_Unit {
                unit_target: Guid::new(guid),
            },
        ),
    }
}

#[test]
fn instant_cast_sends_start_then_raw_cast_result_ok_then_go_and_threads_the_target() {
    let store = InMemoryCasts::instant();
    let sent = run(
        &store,
        &mut ProtocolSession::in_world(7, 1),
        CMSG_CAST_SPELL {
            spell: 100,
            targets: unit_targets(77),
        },
    );

    let mut want = 100u32.to_le_bytes().to_vec();
    want.push(0x00); // SPELL_RESULT_STATUS_OKAY has no trailing reason byte.
    match sent.as_slice() {
        [Outbound::One(ServerOpcodeMessage::SMSG_SPELL_START(_)), Outbound::Raw {
            opcode: OP_CAST_RESULT,
            body,
        }, Outbound::One(ServerOpcodeMessage::SMSG_SPELL_GO(go))] => {
            assert_eq!(
                *body, want,
                "CAST_RESULT body is spell_id(u32 LE) + OKAY(0x00)"
            );
            let hits: Vec<u64> = go.hits.iter().map(|g| g.guid()).collect();
            assert_eq!(hits, [77], "the client's unit target is the GO's hit");
        }
        _ => panic!(
            "expected START, raw CAST_RESULT, GO; got {} packets",
            sent.len()
        ),
    }
}

#[test]
fn auto_shot_intercept_starts_the_ranged_attack_instead_of_casting() {
    for spell in [75u32, 5019] {
        let store = InMemoryCasts {
            ranged_auto_repeat: vec![75, 5019],
            ..InMemoryCasts::instant()
        };
        let mut caster = ProtocolSession::in_world(7, 1);
        let sent = run(
            &store,
            &mut caster,
            CMSG_CAST_SPELL {
                spell,
                targets: unit_targets(88),
            },
        );
        match sent.as_slice() {
            [Outbound::One(ServerOpcodeMessage::SMSG_SPELL_START(start))] => {
                assert_eq!(start.spell, spell, "spell id in START");
                assert_eq!(
                    start.timer, 0,
                    "spell {spell}: START timer must be 0 (the 0.5s wind-up is an attack-timer, not a cast bar)"
                );
                let target = start
                    .targets
                    .target_flags
                    .get_unit()
                    .map(|u| u.unit_target.guid());
                assert_eq!(
                    target,
                    Some(88),
                    "spell {spell}: START names the unit target"
                );
            }
            _ => panic!(
                "spell {spell}: expected the activation START alone, got {} packets",
                sent.len()
            ),
        }
        assert!(
            repeating(&caster),
            "spell {spell}: the session arms the loop"
        );
        assert_eq!(
            store.engaged.lock().unwrap().as_slice(),
            &[1],
            "spell {spell}: the durable loop engages the caster"
        );
    }
}

#[test]
fn a_cast_before_entering_the_world_makes_no_request_and_does_not_fail() {
    let store = InMemoryCasts::instant();
    let sent = run(
        &store,
        &mut ProtocolSession::new(7, "TESTER".into()),
        CMSG_CAST_SPELL {
            spell: 100,
            targets: unit_targets(77),
        },
    );
    assert!(matches!(
        sent.as_slice(),
        [Outbound::Raw {
            opcode: OP_CAST_RESULT,
            ..
        }]
    ));
    assert!(store.casts.lock().unwrap().is_empty());
}

#[test]
fn cancelling_auto_repeat_still_tears_the_ranged_loop_down_through_stop_attack() {
    let store = InMemoryCasts {
        ranged_auto_repeat: vec![75],
        ..InMemoryCasts::instant()
    };
    let mut caster = ProtocolSession::in_world(7, 1);
    run(
        &store,
        &mut caster,
        CMSG_CAST_SPELL {
            spell: 75,
            targets: unit_targets(88),
        },
    );
    assert_eq!(store.engaged.lock().unwrap().as_slice(), &[1]);

    let sent = run(&store, &mut caster, CMSG_CANCEL_AUTO_REPEAT_SPELL {});
    assert!(sent.is_empty());
    assert!(!repeating(&caster));
    assert!(store.engaged.lock().unwrap().is_empty());
}

#[test]
fn cancel_aura_removes_the_aura_the_wire_spell_id_names() {
    let store = InMemoryCasts {
        auras: Mutex::new(vec![5555, 6000]),
        ..Default::default()
    };
    let sent = run(
        &store,
        &mut ProtocolSession::in_world(7, 1),
        CMSG_CANCEL_AURA { id: 5555 },
    );
    assert!(sent.is_empty());
    assert_eq!(store.auras.lock().unwrap().as_slice(), &[6000]);
}

#[test]
fn cancel_cast_drops_the_pending_cast_of_the_caller_only() {
    let store = InMemoryCasts {
        pending_casts: Mutex::new(vec![1, 2]),
        ..Default::default()
    };
    let sent = run(
        &store,
        &mut ProtocolSession::in_world(7, 1),
        CMSG_CANCEL_CAST { id: 133 },
    );
    assert!(sent.is_empty());
    assert_eq!(store.pending_casts.lock().unwrap().as_slice(), &[2]);
}
