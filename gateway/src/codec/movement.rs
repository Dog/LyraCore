use super::*;
use wow_world_messages::vanilla::SMSG_FORCE_SWIM_SPEED_CHANGE;

pub fn relayed_move_opcode(msg: &ClientOpcodeMessage) -> Option<u32> {
    use movement_opcodes as m;
    Some(match msg {
        ClientOpcodeMessage::MSG_MOVE_START_FORWARD(_) => m::MSG_MOVE_START_FORWARD,
        ClientOpcodeMessage::MSG_MOVE_START_BACKWARD(_) => m::MSG_MOVE_START_BACKWARD,
        ClientOpcodeMessage::MSG_MOVE_STOP(_) => m::MSG_MOVE_STOP,
        ClientOpcodeMessage::MSG_MOVE_START_STRAFE_LEFT(_) => m::MSG_MOVE_START_STRAFE_LEFT,
        ClientOpcodeMessage::MSG_MOVE_START_STRAFE_RIGHT(_) => m::MSG_MOVE_START_STRAFE_RIGHT,
        ClientOpcodeMessage::MSG_MOVE_STOP_STRAFE(_) => m::MSG_MOVE_STOP_STRAFE,
        ClientOpcodeMessage::MSG_MOVE_JUMP(_) => m::MSG_MOVE_JUMP,
        ClientOpcodeMessage::MSG_MOVE_START_TURN_LEFT(_) => m::MSG_MOVE_START_TURN_LEFT,
        ClientOpcodeMessage::MSG_MOVE_START_TURN_RIGHT(_) => m::MSG_MOVE_START_TURN_RIGHT,
        ClientOpcodeMessage::MSG_MOVE_STOP_TURN(_) => m::MSG_MOVE_STOP_TURN,
        ClientOpcodeMessage::MSG_MOVE_SET_RUN_MODE(_) => m::MSG_MOVE_SET_RUN_MODE,
        ClientOpcodeMessage::MSG_MOVE_SET_WALK_MODE(_) => m::MSG_MOVE_SET_WALK_MODE,
        ClientOpcodeMessage::MSG_MOVE_FALL_LAND(_) => m::MSG_MOVE_FALL_LAND,
        ClientOpcodeMessage::MSG_MOVE_START_SWIM(_) => m::MSG_MOVE_START_SWIM,
        ClientOpcodeMessage::MSG_MOVE_STOP_SWIM(_) => m::MSG_MOVE_STOP_SWIM,
        ClientOpcodeMessage::MSG_MOVE_SET_FACING(_) => m::MSG_MOVE_SET_FACING,
        ClientOpcodeMessage::MSG_MOVE_HEARTBEAT(_) => m::MSG_MOVE_HEARTBEAT,
        _ => return None,
    })
}

/// The typed transport layout belongs to later clients; use the build-5875 codec.
#[cfg_attr(not(test), allow(dead_code))]
pub fn build_movement_relay(
    opcode: u32,
    mover_guid: u64,
    info: MovementInfo,
) -> Option<ServerOpcodeMessage> {
    use movement_opcodes as m;
    let guid = Guid::new(mover_guid);
    Some(match opcode {
        m::MSG_MOVE_START_FORWARD => {
            ServerOpcodeMessage::MSG_MOVE_START_FORWARD(Box::new(MSG_MOVE_START_FORWARD_Server {
                guid,
                info,
            }))
        }
        m::MSG_MOVE_START_BACKWARD => {
            ServerOpcodeMessage::MSG_MOVE_START_BACKWARD(Box::new(MSG_MOVE_START_BACKWARD_Server {
                guid,
                info,
            }))
        }
        m::MSG_MOVE_STOP => {
            ServerOpcodeMessage::MSG_MOVE_STOP(Box::new(MSG_MOVE_STOP_Server { guid, info }))
        }
        m::MSG_MOVE_START_STRAFE_LEFT => ServerOpcodeMessage::MSG_MOVE_START_STRAFE_LEFT(Box::new(
            MSG_MOVE_START_STRAFE_LEFT_Server { guid, info },
        )),
        m::MSG_MOVE_START_STRAFE_RIGHT => ServerOpcodeMessage::MSG_MOVE_START_STRAFE_RIGHT(
            Box::new(MSG_MOVE_START_STRAFE_RIGHT_Server { guid, info }),
        ),
        m::MSG_MOVE_STOP_STRAFE => {
            ServerOpcodeMessage::MSG_MOVE_STOP_STRAFE(Box::new(MSG_MOVE_STOP_STRAFE_Server {
                guid,
                info,
            }))
        }
        m::MSG_MOVE_JUMP => {
            ServerOpcodeMessage::MSG_MOVE_JUMP(Box::new(MSG_MOVE_JUMP_Server { guid, info }))
        }
        m::MSG_MOVE_START_TURN_LEFT => ServerOpcodeMessage::MSG_MOVE_START_TURN_LEFT(Box::new(
            MSG_MOVE_START_TURN_LEFT_Server { guid, info },
        )),
        m::MSG_MOVE_START_TURN_RIGHT => ServerOpcodeMessage::MSG_MOVE_START_TURN_RIGHT(Box::new(
            MSG_MOVE_START_TURN_RIGHT_Server { guid, info },
        )),
        m::MSG_MOVE_STOP_TURN => {
            ServerOpcodeMessage::MSG_MOVE_STOP_TURN(Box::new(MSG_MOVE_STOP_TURN_Server {
                guid,
                info,
            }))
        }
        m::MSG_MOVE_SET_RUN_MODE => {
            ServerOpcodeMessage::MSG_MOVE_SET_RUN_MODE(Box::new(MSG_MOVE_SET_RUN_MODE_Server {
                guid,
                info,
            }))
        }
        m::MSG_MOVE_SET_WALK_MODE => {
            ServerOpcodeMessage::MSG_MOVE_SET_WALK_MODE(Box::new(MSG_MOVE_SET_WALK_MODE_Server {
                guid,
                info,
            }))
        }
        m::MSG_MOVE_FALL_LAND => {
            ServerOpcodeMessage::MSG_MOVE_FALL_LAND(Box::new(MSG_MOVE_FALL_LAND_Server {
                guid,
                info,
            }))
        }
        m::MSG_MOVE_START_SWIM => {
            ServerOpcodeMessage::MSG_MOVE_START_SWIM(Box::new(MSG_MOVE_START_SWIM_Server {
                guid,
                info,
            }))
        }
        m::MSG_MOVE_STOP_SWIM => {
            ServerOpcodeMessage::MSG_MOVE_STOP_SWIM(Box::new(MSG_MOVE_STOP_SWIM_Server {
                guid,
                info,
            }))
        }
        m::MSG_MOVE_SET_FACING => {
            ServerOpcodeMessage::MSG_MOVE_SET_FACING(Box::new(MSG_MOVE_SET_FACING_Server {
                guid,
                info,
            }))
        }
        m::MSG_MOVE_HEARTBEAT => {
            ServerOpcodeMessage::MSG_MOVE_HEARTBEAT(Box::new(MSG_MOVE_HEARTBEAT_Server {
                guid,
                info,
            }))
        }
        _ => return None,
    })
}

// Fixed MovementInfo fields require 28 bytes before any optional tails.
const MIN_MOVEMENT_INFO_LEN: usize = 4 + 4 + 12 + 4 + 4;

pub fn build_movement_relay_raw(
    opcode: u32,
    mover_guid: u64,
    info_bytes: &[u8],
) -> Option<(u16, Vec<u8>)> {
    if !movement_opcodes::is_slice_move(opcode) || info_bytes.len() < MIN_MOVEMENT_INFO_LEN {
        return None;
    }
    // 9 = the largest packed guid, including its mask byte.
    let mut body = Vec::with_capacity(9 + info_bytes.len());
    super::values::write_packed_guid_u64(&mut body, mover_guid);
    body.extend_from_slice(info_bytes);
    Some((opcode as u16, body))
}

pub fn build_teleport_ack(
    mover_guid: u64,
    counter: u32,
    x: f32,
    y: f32,
    z: f32,
    o: f32,
) -> MSG_MOVE_TELEPORT_ACK_Server {
    MSG_MOVE_TELEPORT_ACK_Server {
        guid: Guid::new(mover_guid),
        movement_counter: counter,
        info: MovementInfo {
            flags: MovementInfo_MovementFlags::empty(),
            timestamp: 0,
            position: Vector3d { x, y, z },
            orientation: o,
            fall_time: 0.0,
        },
    }
}

/// Queue TRANSFER_PENDING and NEW_WORLD together to keep loading and arrival ordered.
pub fn build_cross_map_teleport(
    map_id: u32,
    x: f32,
    y: f32,
    z: f32,
    o: f32,
) -> Result<[ServerOpcodeMessage; 2]> {
    let map = Map::try_from(map_id).map_err(|_| anyhow!("invalid map {map_id}"))?;
    Ok([
        ServerOpcodeMessage::SMSG_TRANSFER_PENDING(Box::new(SMSG_TRANSFER_PENDING {
            map,
            has_transport: None,
        })),
        ServerOpcodeMessage::SMSG_NEW_WORLD(Box::new(SMSG_NEW_WORLD {
            map,
            position: Vector3d { x, y, z },
            orientation: o,
        })),
    ])
}

pub fn build_transfer_aborted(
    map_id: u32,
    reason: TransferAbortReason,
) -> Result<SMSG_TRANSFER_ABORTED> {
    Ok(SMSG_TRANSFER_ABORTED {
        map: Map::try_from(map_id).map_err(|_| anyhow!("invalid map {map_id}"))?,
        reason,
        argument: 0,
    })
}

pub fn build_force_run_speed(guid: u64, speed: f32) -> SMSG_FORCE_RUN_SPEED_CHANGE {
    SMSG_FORCE_RUN_SPEED_CHANGE {
        guid: Guid::new(guid),
        move_event: 0,
        speed,
    }
}

pub fn build_force_swim_speed(guid: u64, speed: f32) -> SMSG_FORCE_SWIM_SPEED_CHANGE {
    SMSG_FORCE_SWIM_SPEED_CHANGE {
        guid: Guid::new(guid),
        move_event: 0,
        speed,
    }
}

pub fn build_monster_move(
    mover_guid: u64,
    start: Vector3d,
    dest: Vector3d,
    duration_ms: u32,
    spline_id: u32,
    run: bool,
) -> SMSG_MONSTER_MOVE {
    SMSG_MONSTER_MOVE {
        guid: Guid::new(mover_guid),
        spline_point: start,
        spline_id,
        move_type: SMSG_MONSTER_MOVE_MonsterMoveType::Normal,
        spline_flags: SplineFlag::new(if run { 0x100 } else { 0 }),
        duration: duration_ms,
        splines: vec![dest],
    }
}

// Linear paths send the destination first, then signed destination-relative points.
pub fn build_ground_path_raw(
    mover_guid: u64,
    start: Vector3d,
    points: &[(f32, f32, f32)],
    duration_ms: u32,
    spline_id: u32,
    run: bool,
) -> Option<(u16, Vec<u8>)> {
    use lyracore_shared::movement_path;
    let &destination = points.last()?;
    if points.len() > movement_path::MAX_POINTS
        || [start.x, start.y, start.z].iter().any(|v| !v.is_finite())
        || points
            .iter()
            .any(|p| !p.0.is_finite() || !p.1.is_finite() || !p.2.is_finite())
    {
        return None;
    }
    let mut body = Vec::with_capacity(46 + points.len() * 4);
    super::values::write_packed_guid_u64(&mut body, mover_guid);
    for value in [start.x, start.y, start.z] {
        body.extend_from_slice(&value.to_le_bytes());
    }
    body.extend_from_slice(&spline_id.to_le_bytes());
    body.push(0);
    body.extend_from_slice(&(if run { 0x100u32 } else { 0 }).to_le_bytes());
    body.extend_from_slice(&duration_ms.to_le_bytes());
    body.extend_from_slice(&(points.len() as u32).to_le_bytes());
    for value in [destination.0, destination.1, destination.2] {
        body.extend_from_slice(&value.to_le_bytes());
    }
    for &point in &points[..points.len() - 1] {
        if movement_path::distance(destination, point).powi(2) < 0.5 {
            return None;
        }
        body.extend_from_slice(&movement_path::packed_offset(destination, point)?.to_le_bytes());
    }
    Some((0x00DD, body))
}

/// Taxi Catmull-Rom paths need absolute points; gtker always applies linear PackXYZ.
pub fn build_taxi_move_raw(
    passenger_guid: u64,
    start: Vector3d,
    points: Vec<Vector3d>,
    duration_ms: u32,
    spline_id: u32,
) -> Option<(u16, Vec<u8>)> {
    if points.is_empty()
        || points
            .iter()
            .chain([&start])
            .any(|p| !p.x.is_finite() || !p.y.is_finite() || !p.z.is_finite())
    {
        return None;
    }
    let mut body = Vec::with_capacity(34 + points.len() * 12);
    super::values::write_packed_guid_u64(&mut body, passenger_guid);
    for value in [start.x, start.y, start.z] {
        body.extend_from_slice(&value.to_le_bytes());
    }
    body.extend_from_slice(&spline_id.to_le_bytes());
    body.push(0);
    body.extend_from_slice(&0x0000_0300u32.to_le_bytes());
    body.extend_from_slice(&duration_ms.to_le_bytes());
    body.extend_from_slice(&(points.len() as u32).to_le_bytes());
    for point in points {
        for value in [point.x, point.y, point.z] {
            body.extend_from_slice(&value.to_le_bytes());
        }
    }
    Some((0x00DD, body))
}

pub fn build_monster_move_facing(
    mover_guid: u64,
    pos: Vector3d,
    angle_rad: f32,
    spline_id: u32,
) -> SMSG_MONSTER_MOVE {
    SMSG_MONSTER_MOVE {
        guid: Guid::new(mover_guid),
        spline_point: pos,
        spline_id,
        move_type: SMSG_MONSTER_MOVE_MonsterMoveType::FacingAngle { angle: angle_rad },
        spline_flags: SplineFlag::new(0),
        duration: 0,
        splines: vec![pos],
    }
}

#[cfg(test)]
mod ground_path_tests {
    use super::*;

    #[test]
    fn vanilla_ground_path_carries_destination_relative_signed_offsets() {
        let (opcode, body) = build_ground_path_raw(
            1,
            Vector3d {
                x: 0.0,
                y: 0.0,
                z: 0.0,
            },
            &[(9.0, 21.0, 29.5), (10.0, 20.0, 30.0)],
            2_000,
            7,
            true,
        )
        .unwrap();
        assert_eq!(opcode, 0xdd);
        assert_eq!(
            body,
            vec![
                1, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 7, 0, 0, 0, 0, 0, 1, 0, 0, 0xd0, 7, 0, 0,
                2, 0, 0, 0, 0, 0, 0x20, 0x41, 0, 0, 0xa0, 0x41, 0, 0, 0xf0, 0x41, 4, 0xe0, 0xbf, 0,
            ]
        );
    }

    #[test]
    fn ground_path_refuses_unrepresentable_intermediate_points() {
        let start = Vector3d {
            x: 0.0,
            y: 0.0,
            z: 0.0,
        };
        for points in [
            [(300.0, 0.0, 0.0), (0.0, 0.0, 0.0)],
            [(0.25, 0.0, 0.0), (0.0, 0.0, 0.0)],
            [(f32::NAN, 0.0, 0.0), (0.0, 0.0, 0.0)],
        ] {
            assert!(build_ground_path_raw(1, start, &points, 1000, 1, true).is_none());
        }
    }
}

pub(crate) mod subscriptions;
