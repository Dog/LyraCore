use super::*;
use crate::codec::combat::subscriptions::tests::aura;
use crate::codec::entity::subscriptions::tests::player_entity;
use lyracore_shared::spell::taxonomy::{
    A_CONTROL, A_MOD_SPEED, M_FEAR, M_POLY, M_ROOT, M_STUN, SPEED_MOVE, SPEED_SWIM,
};

const SELF: u64 = 1;
const CREATURE: u64 = 0xF130_0000_0000_0042;
const PEER: u64 = 2;

fn raw(out: &Outbound) -> (u16, &[u8]) {
    match out {
        Outbound::Raw { opcode, body } => (*opcode, body),
        _ => panic!("expected a raw packet"),
    }
}

fn stun(target: u64) -> Aura {
    aura(target, A_CONTROL, M_STUN, 0, 0, i64::MAX)
}

#[test]
fn a_stun_roots_its_own_client_until_the_last_immobilizer_ends() {
    let stun = stun(SELF);
    let root = aura(SELF, A_CONTROL, M_ROOT, 0, 0, i64::MAX);
    let out = aura_movement_outbound(None, std::slice::from_ref(&stun), &[&stun], SELF, SELF);
    assert_eq!(raw(&out[0]), (0x00E8, [0x01, 0x01, 0, 0, 0, 0].as_slice()));
    let out = aura_movement_outbound(None, std::slice::from_ref(&root), &[&stun], SELF, SELF);
    assert_eq!(raw(&out[0]).0, 0x00E8);
    let out = aura_movement_outbound(None, &[], &[&root], SELF, SELF);
    assert_eq!(raw(&out[0]).0, 0x00EA);
}

#[test]
fn a_dead_body_stays_rooted_when_its_stun_ends() {
    let stun = stun(SELF);
    let body = WorldEntity {
        dead: true,
        ..player_entity()
    };
    let out = aura_movement_outbound(Some(&body), &[], &[&stun], SELF, SELF);
    assert_eq!(raw(&out[0]).0, 0x00E8);
}

#[test]
fn fear_is_not_an_immobilizer() {
    let fear = aura(SELF, A_CONTROL, M_FEAR, 0, 0, i64::MAX);
    assert!(
        aura_movement_outbound(None, std::slice::from_ref(&fear), &[&fear], SELF, SELF).is_empty()
    );
}

#[test]
fn observers_see_a_creature_root_as_a_spline_root_and_a_player_root_not_at_all() {
    let creature_stun = stun(CREATURE);
    let out = aura_movement_outbound(
        None,
        std::slice::from_ref(&creature_stun),
        &[&creature_stun],
        CREATURE,
        SELF,
    );
    assert_eq!(raw(&out[0]).0, 0x031A);
    let out = aura_movement_outbound(None, &[], &[&creature_stun], CREATURE, SELF);
    assert_eq!(raw(&out[0]).0, 0x0304);
    let peer_stun = stun(PEER);
    assert!(aura_movement_outbound(
        None,
        std::slice::from_ref(&peer_stun),
        &[&peer_stun],
        PEER,
        SELF
    )
    .is_empty());
}

// A 40% slow: run 7.0 becomes 4.2 and run back 4.5 becomes 2.7.
#[test]
fn a_slow_changes_backward_running_for_the_owner_and_observers() {
    let slow = aura(SELF, A_MOD_SPEED, i32::from(SPEED_MOVE), -40, 0, i64::MAX);
    let out = aura_movement_outbound(None, std::slice::from_ref(&slow), &[&slow], SELF, SELF);
    match &out[..] {
        [Outbound::One(ServerOpcodeMessage::SMSG_FORCE_RUN_BACK_SPEED_CHANGE(change))] => {
            assert!((change.speed - 2.7).abs() < 1e-5);
        }
        _ => panic!("expected one run-back change"),
    }
    let peer_slow = aura(PEER, A_MOD_SPEED, i32::from(SPEED_MOVE), -40, 0, i64::MAX);
    let out = aura_movement_outbound(
        None,
        std::slice::from_ref(&peer_slow),
        &[&peer_slow],
        PEER,
        SELF,
    );
    match &out[..] {
        [Outbound::One(ServerOpcodeMessage::SMSG_SPLINE_SET_RUN_SPEED(run)), Outbound::One(ServerOpcodeMessage::SMSG_SPLINE_SET_RUN_BACK_SPEED(back))] =>
        {
            assert_eq!((run.guid.guid(), back.guid.guid()), (PEER, PEER));
            assert!((run.speed - 4.2).abs() < 1e-5);
            assert!((back.speed - 2.7).abs() < 1e-5);
        }
        _ => panic!("expected run and run-back spline speeds"),
    }
}

// Sprint's +70% gives 11.9 and leaves backward running alone.
#[test]
fn observers_see_bonus_speeds_in_spline_form() {
    let sprint = aura(PEER, A_MOD_SPEED, i32::from(SPEED_MOVE), 70, 0, i64::MAX);
    let out = aura_movement_outbound(None, std::slice::from_ref(&sprint), &[&sprint], PEER, SELF);
    match &out[..] {
        [Outbound::One(ServerOpcodeMessage::SMSG_SPLINE_SET_RUN_SPEED(run))] => {
            assert!((run.speed - 11.9).abs() < 1e-4);
        }
        _ => panic!("expected one run spline speed"),
    }
    let swim = aura(PEER, A_MOD_SPEED, i32::from(SPEED_SWIM), 50, 0, i64::MAX);
    let out = aura_movement_outbound(None, &[], &[&swim], PEER, SELF);
    match &out[..] {
        [Outbound::One(ServerOpcodeMessage::SMSG_SPLINE_SET_SWIM_SPEED(speed))] => {
            assert!((speed.speed - lyracore_shared::constants::speeds::SWIM).abs() < 1e-5);
        }
        _ => panic!("expected the base swim speed after the bonus ends"),
    }
    let own_sprint = aura(SELF, A_MOD_SPEED, i32::from(SPEED_MOVE), 70, 0, i64::MAX);
    assert!(aura_movement_outbound(None, &[], &[&own_sprint], SELF, SELF).is_empty());
}

#[test]
fn death_roots_release_unroots_with_water_walk_and_resurrection_lands() {
    const GHOST: u32 = lyracore_shared::constants::player_flags::GHOST;
    let alive = player_entity();
    let body = WorldEntity {
        dead: true,
        ..alive.clone()
    };
    let ghost = WorldEntity {
        player_flags: GHOST,
        ..body.clone()
    };
    let out = self_entity_movement_outbound(&alive, &body, &[]);
    assert_eq!(out.len(), 1);
    assert_eq!(raw(&out[0]).0, 0x00E8);
    let out = self_entity_movement_outbound(&body, &ghost, &[]);
    assert_eq!(raw(&out[0]).0, 0x00EA);
    assert!(matches!(
        out[1],
        Outbound::One(ServerOpcodeMessage::SMSG_MOVE_WATER_WALK(_))
    ));
    let out = self_entity_movement_outbound(&ghost, &alive, &[]);
    assert_eq!(raw(&out[0]).0, 0x00EA);
    assert!(matches!(
        out[1],
        Outbound::One(ServerOpcodeMessage::SMSG_MOVE_LAND_WALK(_))
    ));
    assert!(self_entity_movement_outbound(&alive, &alive, &[]).is_empty());
}

#[test]
fn world_entry_restores_only_the_controls_that_differ_from_the_default() {
    let alive = player_entity();
    assert!(world_entry_movement_outbound(&alive, &[]).is_empty());
    let out = world_entry_movement_outbound(&alive, &[stun(SELF)]);
    assert_eq!(raw(&out[0]).0, 0x00E8);
    let ghost = WorldEntity {
        dead: true,
        player_flags: lyracore_shared::constants::player_flags::GHOST,
        ..alive
    };
    assert!(matches!(
        world_entry_movement_outbound(&ghost, &[])[..],
        [Outbound::One(ServerOpcodeMessage::SMSG_MOVE_WATER_WALK(_))]
    ));
}

// A 40% slow leaves 4.5 * 0.6 = 2.7 yards per second of backward running.
#[test]
fn world_entry_restores_a_slowed_backward_speed() {
    let slow = aura(SELF, A_MOD_SPEED, i32::from(SPEED_MOVE), -40, 0, i64::MAX);
    match &world_entry_movement_outbound(&player_entity(), &[slow])[..] {
        [Outbound::One(ServerOpcodeMessage::SMSG_FORCE_RUN_BACK_SPEED_CHANGE(change))] => {
            assert!((change.speed - 2.7).abs() < 1e-5);
        }
        _ => panic!("expected one run-back change"),
    }
}

#[test]
fn polymorph_roots_the_owner_and_a_creature() {
    let polymorph = aura(SELF, A_CONTROL, M_POLY, 0, 0, i64::MAX);
    let out = aura_movement_outbound(
        None,
        std::slice::from_ref(&polymorph),
        &[&polymorph],
        SELF,
        SELF,
    );
    assert_eq!(raw(&out[0]).0, 0x00E8);
    let sheep = aura(CREATURE, A_CONTROL, M_POLY, 0, 0, i64::MAX);
    let out = aura_movement_outbound(
        None,
        std::slice::from_ref(&sheep),
        &[&sheep],
        CREATURE,
        SELF,
    );
    assert_eq!(raw(&out[0]).0, 0x031A);
}
