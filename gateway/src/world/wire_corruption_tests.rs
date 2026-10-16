use super::*;

fn movement_burst_over_a_real_cipher(n: usize) {
    let store = std::sync::Arc::new(WorldFake {
        session: SessionState {
            username: "TESTER".into(),
            session: Some(WorldSession {
                account_id: 42,
                session_key: K,
            }),
            ..Default::default()
        },
        ..Default::default()
    });
    let (mut client, server_end) = world_session_socket_pair();
    let server_store = store.clone();
    let server = std::thread::spawn(move || {
        let mut s = server_end;
        let (_conn, encrypt) = world_handshake(&mut s, server_store.clone())
            .unwrap()
            .expect("handshake should succeed");
        (s, encrypt)
    });
    let (_c_enc, mut c_dec) = client_handshake(&mut client, "TESTER", K);
    let (s, encrypt) = server
        .join()
        .expect("server handshake thread must not panic");

    let (tx, writer) = start_writer(s, encrypt);

    // Read concurrently with the writer; joining first deadlocks once the socket buffer fills.
    let reader = std::thread::spawn(move || {
        let mut monster_moves = 0usize;
        let mut heartbeats = 0usize;
        for _ in 0..(2 * n) {
            match ServerOpcodeMessage::read_encrypted(&mut client, &mut c_dec) {
                Ok(ServerOpcodeMessage::SMSG_MONSTER_MOVE(_)) => monster_moves += 1,
                Ok(ServerOpcodeMessage::MSG_MOVE_HEARTBEAT(_)) => heartbeats += 1,
                Ok(other) => panic!(
                    "n={n}: decoded an unexpected opcode after {monster_moves} monster-moves + \
                     {heartbeats} heartbeats — this IS a stream desync: {other}"
                ),
                Err(e) => panic!(
                    "n={n}: decode failed after {monster_moves} monster-moves + {heartbeats} \
                     heartbeats — exactly the crash shape this file guards against (a desynced \
                     header decoding to garbage): {e}"
                ),
            }
        }
        (monster_moves, heartbeats)
    });

    for i in 0..n {
        while tx.depth() >= 128 {
            std::thread::sleep(Duration::from_millis(1));
        }
        let guid = 0x1000_0000_0000_0000u64 + i as u64;
        let start = wow_world_messages::vanilla::Vector3d {
            x: -8938.0 + i as f32,
            y: -131.0,
            z: 83.5,
        };
        let dest = wow_world_messages::vanilla::Vector3d {
            x: -8900.0 + i as f32,
            y: -100.0,
            z: 82.4,
        };
        let m = codec::build_monster_move(guid, start, dest, 500, i as u32 + 1, i % 2 == 0);
        tx.send(Outbound::One(ServerOpcodeMessage::SMSG_MONSTER_MOVE(
            Box::new(m),
        )))
        .expect("writer thread must still be alive mid-burst");

        let info = MovementInfo {
            flags: wow_world_messages::vanilla::MovementInfo_MovementFlags::empty(),
            timestamp: i as u32,
            position: wow_world_messages::vanilla::Vector3d {
                x: 100.0 + i as f32,
                y: 0.0,
                z: 0.0,
            },
            orientation: 0.0,
            fall_time: 0.0,
        };
        let info_bytes = codec::movement_info_to_bytes(&info)
            .expect("a well-formed MovementInfo must serialize");
        let (opcode, body) = codec::build_movement_relay_raw(
            lyracore_shared::opcodes::movement::MSG_MOVE_HEARTBEAT,
            guid,
            &info_bytes,
        )
        .expect("a valid MovementInfo carrier must always produce a relay frame");
        tx.send(Outbound::Raw { opcode, body })
            .expect("writer thread must still be alive mid-burst");
    }
    drop(tx);
    writer
        .join()
        .expect("writer thread must not panic under a movement burst")
        .expect("movement frames must reach the client");
    let (monster_moves, heartbeats) = reader.join().expect("reader thread must not panic");

    assert_eq!(
        monster_moves, n,
        "n={n}: lost or gained a typed SMSG_MONSTER_MOVE frame"
    );
    assert_eq!(
        heartbeats, n,
        "n={n}: lost or gained a raw peer-motion relay frame"
    );
}

#[test]
fn writer_thread_survives_movement_bursts_from_100_to_500_movers_without_desync() {
    for n in [100usize, 150, 300, 500] {
        movement_burst_over_a_real_cipher(n);
    }
}

#[test]
fn writer_trace_ring_caps_at_capacity_and_drops_oldest() {
    let mut trace = WriterTrace::new();
    for i in 0..(WriterTrace::CAPACITY + 5) {
        trace.push(i as u16, 0, 0);
    }
    assert_eq!(trace.ring.len(), WriterTrace::CAPACITY);
    assert_eq!(trace.ring.front().unwrap().opcode, 5);
    assert_eq!(
        trace.ring.back().unwrap().opcode,
        (WriterTrace::CAPACITY + 4) as u16
    );
}

#[test]
fn writer_trace_dump_writes_a_file_with_the_traced_frames() {
    let mut trace = WriterTrace::new();
    trace.push(0x00EE, 31, 0xDEAD_BEEF_0000_0001);
    trace.push(0x0130, 6, 0xDEAD_BEEF_0000_0002);
    const ACCOUNT: u64 = 0x0FFF_FFFF_F209_0000;
    let dir = std::env::temp_dir().join(format!(
        "lyracore-writer-trace-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir(&dir).unwrap();
    trace.dump_to(&dir, ACCOUNT, "test-induced dump, not a real session end");

    let path = dir.join(format!("{ACCOUNT}.txt"));
    let contents = std::fs::read_to_string(&path).expect("dump must write a readable file");
    assert!(
        contents.contains("opcode=0x00EE"),
        "missing first frame: {contents}"
    );
    assert!(
        contents.contains("opcode=0x0130"),
        "missing second frame: {contents}"
    );
    assert!(
        contents.contains("test-induced dump"),
        "missing the end reason: {contents}"
    );
    std::fs::remove_file(&path).unwrap();
    std::fs::remove_dir(dir).unwrap();
}

#[test]
fn oversized_raw_body_ends_the_session_instead_of_wrapping_the_size_field() {
    let store = std::sync::Arc::new(WorldFake {
        session: SessionState {
            username: "TESTER".into(),
            session: Some(WorldSession {
                account_id: 42,
                session_key: K,
            }),
            ..Default::default()
        },
        ..Default::default()
    });
    let (mut client, server_end) = world_session_socket_pair();
    let server_store = store.clone();
    let server = std::thread::spawn(move || {
        let mut s = server_end;
        let (_conn, encrypt) = world_handshake(&mut s, server_store.clone())
            .unwrap()
            .expect("handshake should succeed");
        (s, encrypt)
    });
    let (_c_enc, mut c_dec) = client_handshake(&mut client, "TESTER", K);
    let (s, encrypt) = server
        .join()
        .expect("server handshake thread must not panic");

    let (tx, writer) = start_writer(s, encrypt);

    let oversized_body = vec![0xABu8; u16::MAX as usize];
    tx.send(Outbound::Raw {
        opcode: 0x00A9,
        body: oversized_body,
    })
    .unwrap();
    drop(tx);

    writer
        .join()
        .expect("the writer thread must not panic on an oversized raw body")
        .expect_err("oversized bodies must end the writer");

    match ServerOpcodeMessage::read_encrypted(&mut client, &mut c_dec) {
        Err(_) => {}
        Ok(m) => panic!(
            "expected a clean EOF (nothing sent for the oversized frame), but decoded {m} — the \
             size field wrapped and the client just parsed a corrupt frame as if it were real"
        ),
    }
}

fn combat_cast_burst_over_a_real_cipher(n: usize) {
    let store = std::sync::Arc::new(WorldFake {
        session: SessionState {
            username: "TESTER".into(),
            session: Some(WorldSession {
                account_id: 42,
                session_key: K,
            }),
            ..Default::default()
        },
        ..Default::default()
    });
    let (mut client, server_end) = world_session_socket_pair();
    let server_store = store.clone();
    let server = std::thread::spawn(move || {
        let mut s = server_end;
        let (_conn, encrypt) = world_handshake(&mut s, server_store.clone())
            .unwrap()
            .expect("handshake should succeed");
        (s, encrypt)
    });
    let (_c_enc, mut c_dec) = client_handshake(&mut client, "TESTER", K);
    let (s, encrypt) = server
        .join()
        .expect("server handshake thread must not panic");

    let (tx, writer) = start_writer(s, encrypt);

    const SPELL_ID: u32 = 133;
    const BOSS_GUID: u64 = 0xF130_0000_0000_0001;
    const FRAMES_PER_CASTER: usize = 6;

    // Read concurrently with the writer to avoid deadlock on a full socket buffer.
    let reader = std::thread::spawn(move || {
        let mut starts = 0usize;
        let mut goes = 0usize;
        let mut damage_logs = 0usize;
        let mut cooldowns = 0usize;
        let mut swings = 0usize;
        let mut heartbeats = 0usize;
        for _ in 0..(FRAMES_PER_CASTER * n) {
            match ServerOpcodeMessage::read_encrypted(&mut client, &mut c_dec) {
                Ok(ServerOpcodeMessage::SMSG_SPELL_START(_)) => starts += 1,
                Ok(ServerOpcodeMessage::SMSG_SPELL_GO(_)) => goes += 1,
                Ok(ServerOpcodeMessage::SMSG_SPELLNONMELEEDAMAGELOG(_)) => damage_logs += 1,
                Ok(ServerOpcodeMessage::SMSG_SPELL_COOLDOWN(_)) => cooldowns += 1,
                Ok(ServerOpcodeMessage::SMSG_ATTACKERSTATEUPDATE(_)) => swings += 1,
                Ok(ServerOpcodeMessage::MSG_MOVE_HEARTBEAT(_)) => heartbeats += 1,
                Ok(other) => panic!(
                    "n={n}: decoded an unexpected opcode after {starts} starts + {goes} goes + \
                     {damage_logs} damage-logs + {cooldowns} cooldowns + {swings} swings + \
                     {heartbeats} heartbeats — this IS a stream desync, on the SPELL/COMBAT plane: \
                     {other}"
                ),
                Err(e) => panic!(
                    "n={n}: decode failed after {starts} starts + {goes} goes + {damage_logs} \
                     damage-logs + {cooldowns} cooldowns + {swings} swings + {heartbeats} \
                     heartbeats — exactly the crash shape this file guards against, on the \
                     SPELL/COMBAT plane: {e}"
                ),
            }
        }
        (starts, goes, damage_logs, cooldowns, swings, heartbeats)
    });

    for i in 0..n {
        while tx.depth() >= 128 {
            std::thread::sleep(Duration::from_millis(1));
        }
        let caster = 0x1000_0000_0000_0000u64 + i as u64;
        let damage = 40 + (i as u32 % 60);

        let start = codec::build_spell_start(caster, SPELL_ID, 0, 0, None);
        tx.send(Outbound::One(ServerOpcodeMessage::SMSG_SPELL_START(
            Box::new(start),
        )))
        .expect("writer thread must still be alive mid-burst");

        let go = codec::build_spell_go(caster, SPELL_ID, BOSS_GUID, None);
        tx.send(Outbound::One(ServerOpcodeMessage::SMSG_SPELL_GO(Box::new(
            go,
        ))))
        .expect("writer thread must still be alive mid-burst");

        let log = codec::build_spell_non_melee_damage_log(
            BOSS_GUID,
            caster,
            SPELL_ID,
            damage,
            2,
            i % 5 == 0,
            0,
            0,
        );
        tx.send(Outbound::One(
            ServerOpcodeMessage::SMSG_SPELLNONMELEEDAMAGELOG(Box::new(log)),
        ))
        .expect("writer thread must still be alive mid-burst");

        let cd = codec::build_spell_cooldown(caster, SPELL_ID, 1500);
        tx.send(Outbound::One(ServerOpcodeMessage::SMSG_SPELL_COOLDOWN(
            Box::new(cd),
        )))
        .expect("writer thread must still be alive mid-burst");

        let swing = codec::build_attacker_state_update(BOSS_GUID, caster, damage, 0, 0, 0);
        tx.send(Outbound::One(
            ServerOpcodeMessage::SMSG_ATTACKERSTATEUPDATE(Box::new(swing)),
        ))
        .expect("writer thread must still be alive mid-burst");

        let info = MovementInfo {
            flags: wow_world_messages::vanilla::MovementInfo_MovementFlags::empty(),
            timestamp: i as u32,
            position: wow_world_messages::vanilla::Vector3d {
                x: -8938.0 + i as f32,
                y: -131.0,
                z: 83.5,
            },
            orientation: 0.0,
            fall_time: 0.0,
        };
        let info_bytes = codec::movement_info_to_bytes(&info)
            .expect("a well-formed MovementInfo must serialize");
        let (opcode, body) = codec::build_movement_relay_raw(
            lyracore_shared::opcodes::movement::MSG_MOVE_HEARTBEAT,
            caster,
            &info_bytes,
        )
        .expect("a valid MovementInfo carrier must always produce a relay frame");
        tx.send(Outbound::Raw { opcode, body })
            .expect("writer thread must still be alive mid-burst");
    }
    drop(tx);
    writer
        .join()
        .expect("writer thread must not panic under a combat+cast burst")
        .expect("combat frames must reach the client");
    let (starts, goes, damage_logs, cooldowns, swings, heartbeats) =
        reader.join().expect("reader thread must not panic");

    assert_eq!(starts, n, "n={n}: lost or gained a SMSG_SPELL_START frame");
    assert_eq!(goes, n, "n={n}: lost or gained a SMSG_SPELL_GO frame");
    assert_eq!(
        damage_logs, n,
        "n={n}: lost or gained a SMSG_SPELLNONMELEEDAMAGELOG frame"
    );
    assert_eq!(
        cooldowns, n,
        "n={n}: lost or gained a SMSG_SPELL_COOLDOWN frame"
    );
    assert_eq!(
        swings, n,
        "n={n}: lost or gained a SMSG_ATTACKERSTATEUPDATE frame"
    );
    assert_eq!(
        heartbeats, n,
        "n={n}: lost or gained a peer-motion relay frame"
    );
}

#[test]
fn writer_thread_survives_combat_cast_bursts_from_100_to_300_casters_without_desync() {
    for n in [100usize, 150, 200, 300] {
        combat_cast_burst_over_a_real_cipher(n);
    }
}
