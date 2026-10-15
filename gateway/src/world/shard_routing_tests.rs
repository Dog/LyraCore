use super::*;

pub(super) type ShardCallLog = std::sync::Arc<std::sync::Mutex<Vec<(String, String)>>>;

pub(super) fn sharded_stores() -> (std::sync::Arc<WorldFake>, ShardCallLog) {
    sharded_stores_with_home_entity(true)
}

fn sharded_stores_with_home_entity(
    home_entity_in_world: bool,
) -> (std::sync::Arc<WorldFake>, ShardCallLog) {
    let calls: ShardCallLog = Default::default();
    let mut ported = warrior_entity();
    ported.map_id = 1;
    let home = std::sync::Arc::new(WorldFake {
        session: SessionState {
            entity_in_world: home_entity_in_world,
            login_entity: Some(warrior_entity()),
            worldport_entity: Some(ported),
            ..Default::default()
        },
        topology: TopologyState {
            shard: "instances".into(),
            calls: calls.clone(),
            ..Default::default()
        },
        auction: AuctionState {
            auction_interaction: Some(imported_auction_interaction()),
        },
        ..Default::default()
    });
    let world = std::sync::Arc::new(WorldFake {
        topology: TopologyState {
            shard: "world".into(),
            calls: calls.clone(),
            home: Some(home),
            ..Default::default()
        },
        session: SessionState {
            username: "TESTER".into(),
            session: Some(WorldSession {
                account_id: 7,
                session_key: K,
            }),
            login_entity: Some(warrior_entity()),
            ..Default::default()
        },
        characters: vec![codec::CharacterView {
            guid: 1,
            name: "Tester".into(),
            race: 1,
            class: 1,
            level: 1,
            ..Default::default()
        }],
        ..Default::default()
    });
    (world, calls)
}

#[test]
fn auctioneer_lookup_uses_the_sessions_pinned_home_shard() {
    let (store, calls) = sharded_stores();
    let (mut client, mut c_enc, mut c_dec, server) = enter_world(store, 1);

    MSG_AUCTION_HELLO_Client {
        auctioneer: Guid::new(42),
    }
    .write_encrypted_client(&mut client, &mut c_enc)
    .unwrap();
    assert!(matches!(
        ServerOpcodeMessage::read_encrypted(&mut client, &mut c_dec).unwrap(),
        ServerOpcodeMessage::MSG_AUCTION_HELLO(_)
    ));

    drop(client);
    server.join().unwrap();
    let log = calls.lock().unwrap();
    assert!(
        log.iter()
            .any(|(shard, call)| shard == "instances" && call == "auction_interaction"),
        "the named lookup must run on the pinned home shard: {log:?}"
    );
    assert!(
        !log.iter()
            .any(|(shard, call)| shard == "world" && call == "auction_interaction"),
        "auction lookup escaped to the default shard: {log:?}"
    );
}

fn heartbeat(timestamp: u32) -> wow_world_messages::vanilla::MSG_MOVE_HEARTBEAT_Client {
    use wow_world_messages::vanilla::{MovementInfo, MovementInfo_MovementFlags, Vector3d};
    wow_world_messages::vanilla::MSG_MOVE_HEARTBEAT_Client {
        info: MovementInfo {
            flags: MovementInfo_MovementFlags::empty(),
            timestamp,
            position: Vector3d {
                x: -8950.0,
                y: -130.0,
                z: 83.0,
            },
            orientation: 1.5,
            fall_time: 0.0,
        },
    }
}

pub(super) fn drive_routed_session(
    store: std::sync::Arc<WorldFake>,
    calls: ShardCallLog,
) -> Vec<(String, String)> {
    let (mut client, server_end) = world_session_socket_pair();
    let server_store = store;
    let server = std::thread::spawn(move || {
        run_world_session(server_end, server_store.clone()).unwrap();
    });
    let (mut c_enc, mut c_dec) = client_handshake(&mut client, "TESTER", K);

    CMSG_CHAR_ENUM {}
        .write_encrypted_client(&mut client, &mut c_enc)
        .unwrap();
    ServerOpcodeMessage::read_encrypted(&mut client, &mut c_dec).unwrap();

    CMSG_PLAYER_LOGIN { guid: Guid::new(1) }
        .write_encrypted_client(&mut client, &mut c_enc)
        .unwrap();
    drain_world_entry(&mut client, &mut c_dec);

    heartbeat(100)
        .write_encrypted_client(&mut client, &mut c_enc)
        .unwrap();
    CMSG_ATTACKSWING {
        guid: Guid::new(90),
    }
    .write_encrypted_client(&mut client, &mut c_enc)
    .unwrap();
    let _ = ServerOpcodeMessage::read_encrypted(&mut client, &mut c_dec);

    drop(client);
    server.join().unwrap();
    let log = calls.lock().unwrap().clone();
    log
}

#[test]
fn every_player_scoped_call_after_login_targets_the_home_shard_only() {
    let (store, calls) = sharded_stores();
    let log = drive_routed_session(store, calls);

    assert_eq!(
        log.first().map(|(s, c)| (s.as_str(), c.as_str())),
        Some(("world", "characters")),
        "character select is realm-scoped and must stay on the default database: {log:?}"
    );
    let after_login = &log[1..];
    assert!(
        after_login.iter().all(|(shard, _)| shard == "instances"),
        "no call may target a shard other than the player's home shard: {log:?}"
    );
    for expected in [
        "player_login",
        "subscribe_player_events",
        "movement_update",
        "start_attack",
        "logout",
    ] {
        assert!(
            log.iter()
                .any(|(shard, call)| shard == "instances" && call == expected),
            "{expected} must have run on the home shard: {log:?}"
        );
    }
}

#[test]
fn a_single_entry_shard_map_never_routes_and_keeps_every_call_on_the_one_database() {
    let (store, calls) = sharded_stores();
    let single = std::sync::Arc::new(WorldFake {
        session: SessionState {
            entity_in_world: true,
            username: "TESTER".into(),
            session: Some(WorldSession {
                account_id: 7,
                session_key: K,
            }),
            login_entity: Some(warrior_entity()),
            ..Default::default()
        },
        topology: TopologyState {
            shard: "world".into(),
            calls: calls.clone(),
            home: None,
            ..Default::default()
        },
        characters: store.characters.clone(),
        ..Default::default()
    });
    let log = drive_routed_session(single, calls);

    assert!(
        log.iter().all(|(shard, _)| shard == "world"),
        "an unrouted session must never leave its own database: {log:?}"
    );
    for expected in [
        "characters",
        "player_login",
        "subscribe_player_events",
        "movement_update",
        "logout",
    ] {
        assert!(
            log.iter().any(|(_, call)| call == expected),
            "{expected} missing: {log:?}"
        );
    }
}

#[test]
fn a_routing_flip_re_routes_the_next_entrant_and_leaves_the_resident_alone() {
    let calls: ShardCallLog = Default::default();
    let resolutions: std::sync::Arc<std::sync::atomic::AtomicUsize> = Default::default();
    let instances = std::sync::Arc::new(WorldFake {
        session: SessionState {
            entity_in_world: true,
            login_entity: Some(warrior_entity()),
            ..Default::default()
        },
        topology: TopologyState {
            shard: "instances".into(),
            calls: calls.clone(),
            home_shard_calls: resolutions.clone(),
            ..Default::default()
        },
        ..Default::default()
    });
    let pool_b = std::sync::Arc::new(WorldFake {
        session: SessionState {
            entity_in_world: true,
            login_entity: Some(warrior_entity()),
            ..Default::default()
        },
        topology: TopologyState {
            shard: "pool-b".into(),
            calls: calls.clone(),
            home_shard_calls: resolutions.clone(),
            ..Default::default()
        },
        ..Default::default()
    });
    let world = std::sync::Arc::new(WorldFake {
        topology: TopologyState {
            shard: "world".into(),
            calls: calls.clone(),
            home_shard_calls: resolutions.clone(),
            home: Some(instances),
            home_after_flip: Some(pool_b),
            ..Default::default()
        },
        session: SessionState {
            username: "TESTER".into(),
            session: Some(WorldSession {
                account_id: 7,
                session_key: K,
            }),
            login_entity: Some(warrior_entity()),
            ..Default::default()
        },
        characters: vec![codec::CharacterView {
            guid: 1,
            name: "Tester".into(),
            race: 1,
            class: 1,
            level: 1,
            ..Default::default()
        }],
        ..Default::default()
    });

    let resident = drive_routed_session(world.clone(), calls.clone());
    assert!(
        resident
            .iter()
            .skip(1)
            .all(|(shard, _)| shard == "instances"),
        "the resident's whole session must stay on the shard it entered on: {resident:?}"
    );
    assert!(
        !resident.iter().any(|(shard, _)| shard == "pool-b"),
        "no resident call may follow the flip — this ticket re-routes NEW ENTRANTS ONLY: {resident:?}"
    );
    assert_eq!(
        resolutions.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "routing is resolved exactly once per world entry, on ANY handle — nothing may \
         re-resolve a live session mid-flight"
    );

    calls.lock().unwrap().clear();
    let entrant = drive_routed_session(world, calls.clone());
    assert!(
        entrant.iter().skip(1).all(|(shard, _)| shard == "pool-b"),
        "the next entrant must land on the re-homed shard: {entrant:?}"
    );
    for expected in [
        "player_login",
        "subscribe_player_events",
        "movement_update",
        "logout",
    ] {
        assert!(
            entrant
                .iter()
                .any(|(shard, call)| shard == "pool-b" && call == expected),
            "{expected} must have run on the flipped shard: {entrant:?}"
        );
    }
    assert_eq!(
        resolutions.load(std::sync::atomic::Ordering::SeqCst),
        2,
        "two world entries, two resolutions — no more, and none from either session's own traffic"
    );
}

#[test]
fn a_world_port_keeps_the_pin_when_the_home_shard_still_owns_the_new_map() {
    let (store, calls) = sharded_stores_with_home_entity(false);
    let (mut client, server_end) = world_session_socket_pair();
    let server_store = store.clone();
    let server = std::thread::spawn(move || {
        run_world_session(server_end, server_store.clone()).unwrap();
    });
    let (mut c_enc, mut c_dec) = client_handshake(&mut client, "TESTER", K);
    CMSG_PLAYER_LOGIN { guid: Guid::new(1) }
        .write_encrypted_client(&mut client, &mut c_enc)
        .unwrap();
    drain_world_entry(&mut client, &mut c_dec);
    MSG_MOVE_WORLDPORT_ACK {}
        .write_encrypted_client(&mut client, &mut c_enc)
        .unwrap();
    drain_world_entry(&mut client, &mut c_dec);
    drop(client);
    server.join().unwrap();

    let log = calls.lock().unwrap().clone();
    assert!(
        log.iter().all(|(shard, _)| shard == "instances"),
        "the world-port re-entry must stay on the pinned home shard: {log:?}"
    );
    assert_eq!(
        log.iter().filter(|(_, c)| c == "player_login").count(),
        2,
        "login + world-port re-entry both ran on the home shard: {log:?}"
    );
    assert_eq!(
        log.iter()
            .filter(|(_, c)| c == "subscribe_player_events")
            .count(),
        2,
        "the re-entry re-subscribed on the home shard, not the default one: {log:?}"
    );
}

#[test]
fn a_spurious_worldport_ack_is_ignored_on_a_session_pinned_off_the_default_shard() {
    let calls: ShardCallLog = Default::default();
    let home = std::sync::Arc::new(WorldFake {
        topology: TopologyState {
            shard: "instances".into(),
            calls: calls.clone(),
            ..Default::default()
        },
        session: SessionState {
            login_entity: Some(warrior_entity()),
            entity_in_world: true,
            ..Default::default()
        },
        ..Default::default()
    });
    let world = std::sync::Arc::new(WorldFake {
        topology: TopologyState {
            shard: "world".into(),
            calls: calls.clone(),
            home: Some(home.clone()),
            ..Default::default()
        },
        session: SessionState {
            username: "TESTER".into(),
            session: Some(WorldSession {
                account_id: 7,
                session_key: K,
            }),
            login_entity: Some(warrior_entity()),
            entity_in_world: false,
            ..Default::default()
        },
        characters: vec![codec::CharacterView {
            guid: 1,
            name: "Tester".into(),
            race: 1,
            class: 1,
            level: 1,
            ..Default::default()
        }],
        ..Default::default()
    });

    let (mut client, server_end) = world_session_socket_pair();
    let server_store = world.clone();
    let server = std::thread::spawn(move || {
        run_world_session(server_end, server_store.clone()).unwrap();
    });
    let (mut c_enc, mut c_dec) = client_handshake(&mut client, "TESTER", K);
    CMSG_PLAYER_LOGIN { guid: Guid::new(1) }
        .write_encrypted_client(&mut client, &mut c_enc)
        .unwrap();
    drain_world_entry(&mut client, &mut c_dec);

    MSG_MOVE_WORLDPORT_ACK {}
        .write_encrypted_client(&mut client, &mut c_enc)
        .unwrap();
    drop(client);
    server.join().unwrap();

    assert_eq!(
        home.session
            .login_calls
            .load(std::sync::atomic::Ordering::SeqCst),
        1,
        "a spurious ack on a pinned session must be ignored — the world entry ran again"
    );
    assert_eq!(
        home.session.subscribed.lock().unwrap().len(),
        1,
        "a spurious ack must not tear down and re-register the session's subscriptions"
    );
}

#[test]
fn a_spurious_worldport_ack_is_ignored_on_the_default_shard() {
    let store = std::sync::Arc::new({
        let base = tester_store(7);
        WorldFake {
            session: SessionState {
                login_entity: Some(warrior_entity()),
                entity_in_world: true,
                ..base.session
            },
            ..base
        }
    });
    let (mut client, server_end) = world_session_socket_pair();
    let server_store = store.clone();
    let server = std::thread::spawn(move || {
        run_world_session(server_end, server_store.clone()).unwrap();
    });
    let (mut c_enc, mut c_dec) = client_handshake(&mut client, "TESTER", K);
    CMSG_PLAYER_LOGIN { guid: Guid::new(1) }
        .write_encrypted_client(&mut client, &mut c_enc)
        .unwrap();
    drain_world_entry(&mut client, &mut c_dec);
    MSG_MOVE_WORLDPORT_ACK {}
        .write_encrypted_client(&mut client, &mut c_enc)
        .unwrap();
    drop(client);
    server.join().unwrap();

    assert_eq!(
        store
            .session
            .login_calls
            .load(std::sync::atomic::Ordering::SeqCst),
        1,
        "a spurious ack with the entity live must be ignored"
    );
}

#[test]
fn a_logout_to_character_select_releases_the_home_shard_pin() {
    let (store, calls) = sharded_stores();
    let (mut client, server_end) = world_session_socket_pair();
    let server_store = store.clone();
    let server = std::thread::spawn(move || {
        run_world_session(server_end, server_store.clone()).unwrap();
    });
    let (mut c_enc, mut c_dec) = client_handshake(&mut client, "TESTER", K);

    CMSG_PLAYER_LOGIN { guid: Guid::new(1) }
        .write_encrypted_client(&mut client, &mut c_enc)
        .unwrap();
    drain_world_entry(&mut client, &mut c_dec);
    CMSG_LOGOUT_REQUEST {}
        .write_encrypted_client(&mut client, &mut c_enc)
        .unwrap();
    ServerOpcodeMessage::read_encrypted(&mut client, &mut c_dec).unwrap();
    ServerOpcodeMessage::read_encrypted(&mut client, &mut c_dec).unwrap();

    CMSG_CHAR_ENUM {}
        .write_encrypted_client(&mut client, &mut c_enc)
        .unwrap();
    let enumerated = match ServerOpcodeMessage::read_encrypted(&mut client, &mut c_dec).unwrap() {
        ServerOpcodeMessage::SMSG_CHAR_ENUM(e) => e.characters.len(),
        other => panic!("expected SMSG_CHAR_ENUM, got {other}"),
    };
    drop(client);
    server.join().unwrap();

    let log = calls.lock().unwrap().clone();
    assert_eq!(
        log.iter()
            .filter(|(s, c)| c == "characters" && s == "world")
            .count(),
        1,
        "the post-logout character enum must run on the realm/default database: {log:?}"
    );
    assert_eq!(
        enumerated, 1,
        "the player must still see their characters after a logout"
    );
}

#[test]
fn a_freshly_created_characters_first_login_transfers_off_the_default_shard() {
    let (store, calls) = sharded_stores();

    let (mut client, server_end) = world_session_socket_pair();
    let server_store = store.clone();
    let server = std::thread::spawn(move || {
        run_world_session(server_end, server_store.clone()).unwrap();
    });
    let (mut c_enc, mut c_dec) = client_handshake(&mut client, "TESTER", K);

    CMSG_CHAR_CREATE {
        name: "Newbie".into(),
        race: Race::Orc,
        class: Class::Warrior,
        gender: Gender::Male,
        skin_color: 0,
        face: 0,
        hair_style: 0,
        hair_color: 0,
        facial_hair: 0,
    }
    .write_encrypted_client(&mut client, &mut c_enc)
    .unwrap();
    match ServerOpcodeMessage::read_encrypted(&mut client, &mut c_dec).unwrap() {
        ServerOpcodeMessage::SMSG_CHAR_CREATE(m) => {
            assert_eq!(m.result, WorldResult::CharCreateSuccess)
        }
        other => panic!("expected SMSG_CHAR_CREATE, got {other}"),
    }

    CMSG_CHAR_ENUM {}
        .write_encrypted_client(&mut client, &mut c_enc)
        .unwrap();
    let new_guid = match ServerOpcodeMessage::read_encrypted(&mut client, &mut c_dec).unwrap() {
        ServerOpcodeMessage::SMSG_CHAR_ENUM(e) => e
            .characters
            .iter()
            .find(|c| c.name == "Newbie")
            .unwrap_or_else(|| {
                panic!(
                    "the freshly created character never appeared in the char enum — without \
                     this, the login below cannot possibly be testing the character CREATE just \
                     produced. characters: {:?}",
                    e.characters
                )
            })
            .guid
            .guid(),
        other => panic!("expected SMSG_CHAR_ENUM, got {other}"),
    };

    CMSG_PLAYER_LOGIN {
        guid: Guid::new(new_guid),
    }
    .write_encrypted_client(&mut client, &mut c_enc)
    .unwrap();
    drain_world_entry(&mut client, &mut c_dec);
    drop(client);
    server.join().unwrap();

    let log = calls.lock().unwrap().clone();
    for expected in ["player_login", "subscribe_player_events", "logout"] {
        assert!(
            log.iter()
                .any(|(shard, call)| shard == "instances" && call == expected),
            "the freshly created character's FIRST login must drive the transfer onto its start \
             map's owning shard, exactly like every later world entry — {expected} never ran on \
             `instances`: {log:?}"
        );
    }
    assert!(
        !log.iter().any(|(shard, call)| shard == "world"
            && (call == "player_login" || call == "subscribe_player_events")),
        "the first login must not stay on the default shard once routing resolves it elsewhere: \
         {log:?}"
    );
}
