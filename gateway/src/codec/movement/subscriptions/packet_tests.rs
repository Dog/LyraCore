use super::*;
use crate::codec::entity::subscriptions::tests::creature_create_outbound;
use crate::stdb::bindings::CreatureSpline;
use crate::world::Outbound;
use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use wow_world_messages::vanilla::opcodes::ServerOpcodeMessage;
use wow_world_messages::vanilla::Vector3d;

// ---- Cross-map teleport relay --------------------------------------------

#[test]
fn teleport_relay_same_map_is_byte_identical_to_the_pre_224_ack() {
    let out = build_teleport_relay(true, 7, 0, 1.0, 2.0, 3.0, 0.5).unwrap();
    match out {
        Outbound::One(ServerOpcodeMessage::MSG_MOVE_TELEPORT_ACK(m)) => {
            assert_eq!(m.guid.guid(), 7);
            assert_eq!(
                m.info.position,
                wow_world_messages::vanilla::Vector3d {
                    x: 1.0,
                    y: 2.0,
                    z: 3.0
                }
            );
            assert_eq!(m.info.orientation, 0.5);
        }
        _ => panic!("expected a same-map relay to be a single MSG_MOVE_TELEPORT_ACK"),
    }
}

#[test]
fn teleport_relay_cross_map_sends_transfer_pending_then_new_world() {
    let out = build_teleport_relay(false, 7, 36, 10.0, 20.0, 30.0, 1.5).unwrap();
    match out {
        Outbound::Batch(msgs) => {
            assert_eq!(msgs.len(), 2, "TRANSFER_PENDING + NEW_WORLD, nothing else");
            match &msgs[0] {
                ServerOpcodeMessage::SMSG_TRANSFER_PENDING(p) => {
                    assert_eq!(p.map.as_int(), 36);
                    assert!(p.has_transport.is_none());
                }
                _ => panic!("expected SMSG_TRANSFER_PENDING first"),
            }
            match &msgs[1] {
                ServerOpcodeMessage::SMSG_NEW_WORLD(w) => {
                    assert_eq!(w.map.as_int(), 36);
                    assert_eq!(
                        w.position,
                        wow_world_messages::vanilla::Vector3d {
                            x: 10.0,
                            y: 20.0,
                            z: 30.0
                        }
                    );
                    assert_eq!(w.orientation, 1.5);
                }
                _ => panic!("expected SMSG_NEW_WORLD second"),
            }
        }
        _ => {
            panic!("expected a cross-map relay to be a Batch of [TRANSFER_PENDING, NEW_WORLD]")
        }
    }
}

#[test]
fn teleport_relay_rejects_a_map_id_the_client_enum_does_not_know() {
    // `Map` is a closed vanilla enum; an id past its known range must surface as an Err (never
    // silently drop the whole relay or panic).
    assert!(build_teleport_relay(false, 7, u32::MAX, 0.0, 0.0, 0.0, 0.0).is_err());
}

fn creature_spline(guid: u64, start_micros: u64, dur_ms: u32) -> CreatureSpline {
    CreatureSpline {
        guid,
        start_micros,
        dur_ms,
        sx: 10.0,
        sy: 20.0,
        sz: 30.0,
        dx: 30.0,
        dy: 40.0,
        dz: 50.0,
        map_id: 0,
        instance_id: 0,
        grid_x: 0,
        grid_y: 0,
        spline_id: 10,
        run: true,
        cell: lyracore_shared::spatial::grid_cell_id(0, 0),
        facing: false,
        facing_angle: 0.0,
        path: None,
    }
}

#[test]
fn an_empty_retained_path_stops_the_client_instead_of_sending_invalid_geometry() {
    let created = Mutex::new(HashSet::from([99]));
    let mut row = creature_spline(99, 1_000_000, 1_000);
    row.path = Some(crate::stdb::bindings::CreaturePath {
        navigation: crate::stdb::bindings::NavigationInputs {
            imported_revision: None,
            navigation_enabled: false,
            collision_enabled: false,
            coverage_enabled: false,
            static_generation: None,
            coverage_generation: None,
        },
        points: Vec::new(),
    });
    let outbound = creature_leg_outbound(&created, &row);
    let [Outbound::One(ServerOpcodeMessage::SMSG_MONSTER_MOVE(stop))] = outbound.as_slice() else {
        panic!("malformed paths need a stop");
    };
    assert_eq!(stop.duration, 0);
    assert_eq!(stop.splines[0], stop.spline_point);
}

#[test]
fn resident_waypoint_path_resumes_after_the_turn() {
    let guid = 99;
    let created = Mutex::new(HashSet::from([guid]));
    let mut row = creature_spline(guid, 1_000_000, 7_000);
    (row.sx, row.sy, row.sz) = (0.0, 0.0, 0.0);
    (row.dx, row.dy, row.dz) = (3.0, 4.0, 0.0);
    row.path = Some(crate::stdb::bindings::CreaturePath {
        navigation: crate::stdb::bindings::NavigationInputs {
            imported_revision: None,
            navigation_enabled: false,
            collision_enabled: false,
            coverage_enabled: false,
            static_generation: None,
            coverage_generation: None,
        },
        points: vec![
            crate::stdb::bindings::CreaturePathPoint {
                x: 3.0,
                y: 0.0,
                z: 0.0,
            },
            crate::stdb::bindings::CreaturePathPoint {
                x: 3.0,
                y: 4.0,
                z: 0.0,
            },
        ],
    });
    let mut outbound = Vec::new();
    append_resident_creature_after_create(&mut outbound, &created, 0, Some(&row), 4_500_000);
    let [Outbound::Raw { opcode, body }] = outbound.as_slice() else {
        panic!("expected a retained path");
    };
    assert_eq!(*opcode, 0xdd);
    assert_eq!(&body[..2], &[1, 99]);
    let f32_at = |offset| f32::from_le_bytes(body[offset..offset + 4].try_into().unwrap());
    let u32_at = |offset| u32::from_le_bytes(body[offset..offset + 4].try_into().unwrap());
    assert_eq!((f32_at(2), f32_at(6), f32_at(10)), (3.0, 0.5, 0.0));
    assert_eq!(u32_at(23), 3_500);
    assert_eq!(u32_at(27), 1);
    assert_eq!((f32_at(31), f32_at(35), f32_at(39)), (3.0, 4.0, 0.0));
    assert_eq!(body.len(), 43);
}

#[test]
fn resident_creature_replay_follows_create_from_the_current_point() {
    let guid = 99;
    let created = Mutex::new(HashSet::from([guid]));
    let row = creature_spline(guid, 1_000_000, 1_000);
    let mut outbound = creature_create_outbound();

    append_resident_creature_after_create(&mut outbound, &created, 0, Some(&row), 1_250_500);

    let [Outbound::One(ServerOpcodeMessage::SMSG_UPDATE_OBJECT(create)), Outbound::One(ServerOpcodeMessage::SMSG_MONSTER_MOVE(movement))] =
        outbound.as_slice()
    else {
        panic!("a resident creature needs CREATE followed by MONSTER_MOVE");
    };
    let [wow_world_messages::vanilla::Object::CreateObject2 { guid3, .. }] =
        create.objects.as_slice()
    else {
        panic!("the first packet must create the resident creature");
    };
    assert_eq!(guid3.guid(), guid);
    assert_eq!(movement.guid.guid(), guid);
    assert!((movement.spline_point.x - 15.01).abs() < 0.0001);
    assert!((movement.spline_point.y - 25.01).abs() < 0.0001);
    assert!((movement.spline_point.z - 35.01).abs() < 0.0001);
    assert_eq!(
        movement.splines,
        vec![Vector3d {
            x: 30.0,
            y: 40.0,
            z: 50.0,
        }]
    );
    assert_eq!(
        movement.duration, 750,
        "sub-millisecond remainder rounds up so replay cannot move faster than the durable leg"
    );
}

#[test]
fn resident_creature_replay_skips_absent_expired_and_ineligible_legs() {
    let guid = 99;
    let row = creature_spline(guid, 1_000_000, 1_000);
    let mut outbound = Vec::new();

    append_resident_creature_after_create(
        &mut outbound,
        &Mutex::new(HashSet::from([guid])),
        0,
        None,
        1_250_000,
    );
    append_resident_creature_after_create(
        &mut outbound,
        &Mutex::new(HashSet::new()),
        0,
        Some(&row),
        1_250_000,
    );
    append_resident_creature_after_create(
        &mut outbound,
        &Mutex::new(HashSet::from([guid])),
        1,
        Some(&row),
        1_250_000,
    );
    append_resident_creature_after_create(
        &mut outbound,
        &Mutex::new(HashSet::from([guid])),
        0,
        Some(&row),
        2_000_000,
    );

    assert!(outbound.is_empty());
}

#[test]
fn resident_facing_replay_follows_create_even_after_its_timestamp() {
    let guid = 99;
    let created = Mutex::new(HashSet::from([guid]));
    let mut row = creature_spline(guid, 1_000_000, 0);
    row.facing = true;
    row.facing_angle = 2.1;
    let mut outbound = creature_create_outbound();

    append_resident_creature_after_create(&mut outbound, &created, 0, Some(&row), 9_000_000);

    let [Outbound::One(ServerOpcodeMessage::SMSG_UPDATE_OBJECT(_)), Outbound::One(ServerOpcodeMessage::SMSG_MONSTER_MOVE(movement))] =
        outbound.as_slice()
    else {
        panic!("a resident creature needs CREATE followed by its facing state");
    };
    assert_eq!(
        movement.move_type,
        wow_world_messages::vanilla::SMSG_MONSTER_MOVE_MonsterMoveType::FacingAngle { angle: 2.1 }
    );
}

/// a `facing`-flagged `game_creature_spline` row relays as `SMSG_MONSTER_MOVE`'s
/// `FacingAngle` variant, not the `Normal` one every other leg uses — the whole point being that
/// a stationary creature otherwise has no wire-level way to tell the client its heading changed.
#[test]
fn a_facing_leg_relays_as_the_facing_angle_variant() {
    let created = Arc::new(Mutex::new(HashSet::from([42u64])));
    let row = CreatureSpline {
        guid: 42,
        start_micros: 0,
        dur_ms: 0,
        sx: 10.0,
        sy: 20.0,
        sz: 30.0,
        dx: 10.0,
        dy: 20.0,
        dz: 30.0,
        map_id: 0,
        instance_id: 0,
        grid_x: 0,
        grid_y: 0,
        spline_id: 7,
        run: false,
        cell: lyracore_shared::spatial::grid_cell_id(0, 0),
        facing: true,
        facing_angle: 2.1,
        path: None,
    };
    let out = creature_leg_outbound(&created, &row);
    assert_eq!(out.len(), 1);
    let Outbound::One(ServerOpcodeMessage::SMSG_MONSTER_MOVE(msg)) = &out[0] else {
        panic!("expected SMSG_MONSTER_MOVE");
    };
    assert_eq!(
        msg.move_type,
        wow_world_messages::vanilla::SMSG_MONSTER_MOVE_MonsterMoveType::FacingAngle { angle: 2.1 },
        "facing leg must use the FacingAngle variant, not Normal"
    );
    assert_eq!(msg.duration, 0);

    // A non-facing leg keeps the Normal variant unchanged.
    let mut normal_row = row.clone();
    normal_row.facing = false;
    normal_row.dur_ms = 500;
    let out = creature_leg_outbound(&created, &normal_row);
    let Outbound::One(ServerOpcodeMessage::SMSG_MONSTER_MOVE(msg)) = &out[0] else {
        panic!("expected SMSG_MONSTER_MOVE");
    };
    assert_eq!(
        msg.move_type,
        wow_world_messages::vanilla::SMSG_MONSTER_MOVE_MonsterMoveType::Normal
    );
}
