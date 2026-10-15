use super::*;
use lyracore_shared::opcodes::movement as m;
use std::io::Write;
use wow_world_messages::vanilla::{
    MSG_MOVE_HEARTBEAT_Client, MSG_MOVE_SET_PITCH_Client, MovementInfo, MovementInfo_MovementFlags,
    Vector3d, CMSG_FORCE_MOVE_UNROOT_ACK,
};

fn in_world() -> (
    std::sync::Arc<WorldFake>,
    UnixStream,
    EncrypterHalf,
    std::thread::JoinHandle<Result<()>>,
) {
    let (store, client, c_enc, _, server) = in_world_with(None);
    (store, client, c_enc, server)
}

fn in_world_with(
    relay_view: Option<std::sync::Arc<crate::stdb::world_view::WorldView>>,
) -> (
    std::sync::Arc<WorldFake>,
    UnixStream,
    EncrypterHalf,
    DecrypterHalf,
    std::thread::JoinHandle<Result<()>>,
) {
    let store = std::sync::Arc::new({
        let base = tester_store(7);
        WorldFake {
            session: SessionState {
                login_entity: Some(warrior_entity()),
                relay_view,
                ..base.session
            },
            ..base
        }
    });
    let (mut client, server_end) = world_session_socket_pair();
    let server_store = store.clone();
    let server = std::thread::spawn(move || run_world_session(server_end, server_store));
    let (mut c_enc, mut c_dec) = client_handshake(&mut client, "TESTER", K);
    CMSG_PLAYER_LOGIN { guid: Guid::new(1) }
        .write_encrypted_client(&mut client, &mut c_enc)
        .unwrap();
    drain_world_entry(&mut client, &mut c_dec);
    (store, client, c_enc, c_dec, server)
}

fn send_raw(client: &mut UnixStream, c_enc: &mut EncrypterHalf, opcode: u32, body: &[u8]) {
    c_enc
        .write_encrypted_client_header(&mut *client, 4 + body.len() as u16, opcode)
        .unwrap();
    client.write_all(body).unwrap();
}

/// Build-5875 transport pose with pitch, which the library cannot decode.
fn swimming_on_transport(timestamp: u32) -> Vec<u8> {
    let mut body = 0x0220_0000u32.to_le_bytes().to_vec(); // ON_TRANSPORT | SWIMMING
    body.extend_from_slice(&timestamp.to_le_bytes());
    for value in [-8950.0f32, -130.0, 83.0, 1.5] {
        body.extend_from_slice(&value.to_le_bytes());
    }
    body.extend_from_slice(&0xF120_0000_0000_0012u64.to_le_bytes());
    for value in [1.0f32, -2.5, 3.25, 0.5, 0.25] {
        body.extend_from_slice(&value.to_le_bytes()); // relative pose, then pitch
    }
    body.extend_from_slice(&0u32.to_le_bytes()); // fall time
    body
}

fn standing(timestamp: u32) -> MovementInfo {
    MovementInfo {
        flags: MovementInfo_MovementFlags::empty(),
        timestamp,
        position: Vector3d {
            x: -8950.0,
            y: -130.0,
            z: 83.0,
        },
        orientation: 1.5,
        fall_time: 0.0,
    }
}

fn recorded_opcodes(store: &WorldFake) -> Vec<u32> {
    store
        .session
        .moves
        .lock()
        .unwrap()
        .iter()
        .map(|(opcode, ..)| *opcode)
        .collect()
}

#[test]
fn pitch_changes_reach_the_movement_relay_even_on_a_transport() {
    let (store, mut client, mut c_enc, server) = in_world();
    send_raw(
        &mut client,
        &mut c_enc,
        m::MSG_MOVE_START_PITCH_UP,
        &swimming_on_transport(10),
    );
    MSG_MOVE_SET_PITCH_Client { info: standing(20) }
        .write_encrypted_client(&mut client, &mut c_enc)
        .unwrap();
    drop(client);
    server.join().unwrap().unwrap();
    assert_eq!(
        recorded_opcodes(&store),
        [m::MSG_MOVE_START_PITCH_UP, m::MSG_MOVE_SET_PITCH]
    );
}

#[test]
fn acknowledged_root_and_unroot_relay_the_acknowledged_pose() {
    let (store, mut client, mut c_enc, server) = in_world();
    let mut root_ack = 1u64.to_le_bytes().to_vec();
    root_ack.extend_from_slice(&0u32.to_le_bytes()); // movement counter
    root_ack.extend_from_slice(&swimming_on_transport(10));
    send_raw(&mut client, &mut c_enc, 0x00E9, &root_ack);
    CMSG_FORCE_MOVE_UNROOT_ACK {
        guid: Guid::new(1),
        movement_counter: 1,
        info: standing(20),
    }
    .write_encrypted_client(&mut client, &mut c_enc)
    .unwrap();
    drop(client);
    server.join().unwrap().unwrap();
    assert_eq!(
        recorded_opcodes(&store),
        [m::MSG_MOVE_ROOT, m::MSG_MOVE_UNROOT]
    );
    let moves = store.session.moves.lock().unwrap();
    assert_eq!(moves[0].5, 10, "the relay carries the acknowledged pose");
    assert_eq!(
        store.session.move_bodies.lock().unwrap()[0],
        swimming_on_transport(10),
        "the stored body keeps the build-5875 transport tail"
    );
}

fn root_ack(timestamp: u32) -> Vec<u8> {
    let mut ack = 1u64.to_le_bytes().to_vec();
    ack.extend_from_slice(&0u32.to_le_bytes()); // movement counter
    ack.extend_from_slice(&swimming_on_transport(timestamp));
    ack
}

#[test]
fn an_acknowledged_pose_waits_for_the_client_to_apply_a_teleport() {
    let view = std::sync::Arc::new(crate::stdb::world_view::WorldView::new(true));
    let (store, mut client, mut c_enc, mut c_dec, server) = in_world_with(Some(view.clone()));
    crate::codec::movement::subscriptions::teleport_appeared(
        &view,
        0,
        &crate::stdb::bindings::TeleportEvent {
            id: 1,
            recipient_identity: spacetimedb_sdk::Identity::from_byte_array([0; 32]),
            mover_guid: 1,
            map_id: 0,
            x: -9000.0,
            y: -100.0,
            z: 80.0,
            orientation: 0.0,
            created_micros: 0,
            cross_map: false,
        },
    );
    loop {
        if let ServerOpcodeMessage::MSG_MOVE_TELEPORT_ACK(_) =
            ServerOpcodeMessage::read_encrypted(&mut client, &mut c_dec).unwrap()
        {
            break;
        }
    }
    send_raw(&mut client, &mut c_enc, 0x00E9, &root_ack(10));
    wow_world_messages::vanilla::MSG_MOVE_TELEPORT_ACK_Client {
        guid: Guid::new(1),
        movement_counter: 0,
        time: std::time::Duration::from_millis(15),
    }
    .write_encrypted_client(&mut client, &mut c_enc)
    .unwrap();
    send_raw(&mut client, &mut c_enc, 0x00E9, &root_ack(20));
    drop(client);
    server.join().unwrap().unwrap();
    let moves = store.session.moves.lock().unwrap();
    assert_eq!(
        moves.len(),
        1,
        "only the acknowledgement after the teleport is relayed"
    );
    assert_eq!((moves[0].0, moves[0].5), (m::MSG_MOVE_ROOT, 20));
}

#[test]
fn an_acknowledgement_for_another_mover_records_nothing() {
    let (store, mut client, mut c_enc, server) = in_world();
    CMSG_FORCE_MOVE_UNROOT_ACK {
        guid: Guid::new(2),
        movement_counter: 1,
        info: standing(20),
    }
    .write_encrypted_client(&mut client, &mut c_enc)
    .unwrap();
    drop(client);
    server.join().unwrap().unwrap();
    assert!(recorded_opcodes(&store).is_empty());
}

#[test]
fn cheat_and_game_master_movement_requests_leave_the_position_unchanged() {
    let (store, mut client, mut c_enc, server) = in_world();
    let world_teleport = {
        // Time, an unknown map, a position and a facing: the library would refuse the map.
        let mut body = 0u32.to_le_bytes().to_vec();
        body.extend_from_slice(&0xFFFFu32.to_le_bytes());
        body.extend_from_slice(&[0; 16]);
        body
    };
    let requests: [(u32, Vec<u8>); 8] = [
        (0x0008, world_teleport),
        (0x0009, b"Bob\0".to_vec()),
        (0x00C6, swimming_on_transport(1)),
        (0x00D9, swimming_on_transport(1)),
        (0x00E1, vec![0; 16]),
        (0x02AD, swimming_on_transport(1)),
        (0x02CA, swimming_on_transport(1)),
        (0x02F8, vec![0; 13]),
    ];
    for (opcode, body) in &requests {
        send_raw(&mut client, &mut c_enc, *opcode, body);
    }
    MSG_MOVE_HEARTBEAT_Client { info: standing(30) }
        .write_encrypted_client(&mut client, &mut c_enc)
        .unwrap();
    drop(client);
    server
        .join()
        .unwrap()
        .expect("an ignored request must leave the World Session open");
    assert_eq!(recorded_opcodes(&store), [m::MSG_MOVE_HEARTBEAT]);
}
