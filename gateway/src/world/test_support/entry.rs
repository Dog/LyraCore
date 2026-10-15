use super::*;
use std::os::unix::net::UnixStream;
use wow_world_messages::vanilla::{ClientMessage, CMSG_AUTH_SESSION, CMSG_PLAYER_LOGIN};
use wow_world_messages::Guid;

const WORLD_ENTRY_FRAME_LIMIT: usize = 64;

pub(crate) fn drain_world_entry<S: Read>(
    client: &mut S,
    dec: &mut DecrypterHalf,
) -> Vec<ServerOpcodeMessage> {
    let mut frames = Vec::new();
    while frames.len() < WORLD_ENTRY_FRAME_LIMIT {
        let message = ServerOpcodeMessage::read_encrypted(&mut *client, dec)
            .unwrap_or_else(|e| panic!("world entry frame {} did not arrive: {e}", frames.len()));
        let ends_batch = matches!(message, ServerOpcodeMessage::SMSG_WEATHER(_));
        frames.push(message);
        if ends_batch {
            let world_states = ServerOpcodeMessage::read_encrypted(&mut *client, dec)
                .unwrap_or_else(|e| panic!("world states did not follow the batch: {e}"));
            assert!(
                matches!(world_states, ServerOpcodeMessage::SMSG_INIT_WORLD_STATES(_)),
                "expected SMSG_INIT_WORLD_STATES after the batch, got {world_states}"
            );
            frames.push(world_states);
            return frames;
        }
    }
    panic!("no SMSG_WEATHER within {WORLD_ENTRY_FRAME_LIMIT} frames: the world entry batch never ended");
}

pub(crate) const WORLD_SESSION_READ_DEADLINE: std::time::Duration =
    std::time::Duration::from_secs(5);

pub(crate) fn world_session_socket_pair() -> (UnixStream, UnixStream) {
    let (client, server) = UnixStream::pair().expect("world-session socket pair must be created");
    client
        .set_read_timeout(Some(WORLD_SESSION_READ_DEADLINE))
        .expect("world-session client read deadline must be configured");
    (client, server)
}

pub(crate) const K: [u8; 40] = [
    0x2E, 0xFE, 0xE7, 0xB0, 0xC1, 0x77, 0xEB, 0xBD, 0xFF, 0x66, 0x76, 0xC5, 0x6E, 0xFC, 0x23, 0x39,
    0xBE, 0x9C, 0xAD, 0x14, 0xBF, 0x8B, 0x54, 0xBB, 0x5A, 0x86, 0xFB, 0xF8, 0x1F, 0x6D, 0x42, 0x4A,
    0xA2, 0x3C, 0xC9, 0xA3, 0x14, 0x9F, 0xB1, 0x75,
];

pub(crate) fn ns(s: &str) -> NormalizedString {
    NormalizedString::new(s).unwrap()
}

pub(crate) fn drive_auth<S: Read + Write>(
    client: &mut S,
    username: &str,
    key: [u8; 40],
) -> (EncrypterHalf, DecrypterHalf) {
    let server_seed = match ServerOpcodeMessage::read_unencrypted(&mut *client).unwrap() {
        ServerOpcodeMessage::SMSG_AUTH_CHALLENGE(c) => c.server_seed,
        other => panic!("expected SMSG_AUTH_CHALLENGE, got {other}"),
    };
    let client_seed = ProofSeed::new();
    let client_seed_value = client_seed.seed();
    let (client_proof, crypto) =
        client_seed.into_client_header_crypto(&ns(username), key, server_seed);
    let (enc, dec) = crypto.split();

    auth_session(username, client_seed_value, client_proof)
        .write_unencrypted_client(&mut *client)
        .unwrap();

    (enc, dec)
}

pub(crate) fn client_handshake<S: Read + Write>(
    client: &mut S,
    username: &str,
    key: [u8; 40],
) -> (EncrypterHalf, DecrypterHalf) {
    let (enc, mut dec) = drive_auth(client, username, key);

    match ServerOpcodeMessage::read_encrypted(&mut *client, &mut dec).unwrap() {
        ServerOpcodeMessage::SMSG_AUTH_RESPONSE(r) => {
            assert!(matches!(*r, SMSG_AUTH_RESPONSE::AuthOk { .. }));
        }
        other => panic!("expected encrypted SMSG_AUTH_RESPONSE, got {other}"),
    }
    (enc, dec)
}

pub(crate) fn auth_session(
    username: &str,
    client_seed: u32,
    client_proof: [u8; 20],
) -> CMSG_AUTH_SESSION {
    CMSG_AUTH_SESSION {
        build: 5875,
        server_id: 1,
        username: username.to_string(),
        client_seed,
        client_proof,
        addon_info: vec![],
    }
}

pub(crate) fn tester_store(account_id: u64) -> WorldFake {
    WorldFake {
        trainer: TrainerState {
            talent_reset_quote: Some(10_000),
            ..Default::default()
        },
        session: SessionState {
            entity_in_world: true,
            username: "TESTER".into(),
            session: Some(WorldSession {
                account_id,
                session_key: K,
            }),
            ..Default::default()
        },
        ..Default::default()
    }
}

pub(crate) fn warrior_entity() -> codec::EntityView {
    codec::EntityView {
        watched_faction_index: None,
        guid: 1,
        map_id: 0,
        instance_id: 0,
        zone_id: 12,
        x: -8949.95,
        y: -132.493,
        z: 83.5312,
        orientation: 0.0,
        last_move_ms: 0,
        movement_flags: 0,
        run_speed_mult_bp: 10_000,
        type_mask: 0x19,
        entry: 0,
        scale_x: 1.0,
        health: 60,
        max_health: 60,
        power: 0,
        max_power: 1000,
        level: 1,
        faction_template: 1,
        target_guid: 0,
        unit_bytes_0: 1 | (1 << 8) | (1 << 24),
        display_id: 49,
        native_display_id: 49,
        unit_flags: 0,
        mount_display_id: 0,
        base_attack_time_ms: 2000,
        dynamic_flags: 0,
        player_bytes: 0,
        player_bytes_2: 0,
        player_bytes_3: 0,
        player_flags: 0,
        xp: 0,
        next_level_xp: 0,
        money: 0,
        unit_bytes_1: 0,
        unit_bytes_2: 0,
        strength: 23,
        agility: 20,
        stamina: 22,
        intellect: 20,
        spirit: 21,
        sheet_ap_base: 29,
        sheet_ap_mods: 0,
        sheet_dmg_min: 5,
        sheet_dmg_max: 7,
        sheet_ranged_ap: 0,
        sheet_ranged_dmg_min: 0,
        sheet_ranged_dmg_max: 0,
        npc_flags: 0,
        owner_guid: 0,
        effective_armor: 40,
        magic_resistances: [0; 6],
        home_map: 0,
        home_zone: 0,
        home_x: 0.0,
        home_y: 0.0,
        home_z: 0.0,
        guild_id: 0,
        guild_rank: 0,
    }
}

pub(crate) fn quest_store() -> WorldFake {
    let base = tester_store(7);
    WorldFake {
        session: SessionState {
            login_entity: Some(warrior_entity()),
            ..base.session
        },
        ..base
    }
}

pub(crate) fn enter_world(
    store: std::sync::Arc<WorldFake>,
    guid: u64,
) -> (
    UnixStream,
    EncrypterHalf,
    DecrypterHalf,
    std::thread::JoinHandle<()>,
) {
    let (mut client, server_end) = world_session_socket_pair();
    let server_store = store;
    let server = std::thread::spawn(move || {
        run_world_session(server_end, server_store.clone()).unwrap();
    });
    let (mut c_enc, mut c_dec) = client_handshake(&mut client, "TESTER", K);
    CMSG_PLAYER_LOGIN {
        guid: Guid::new(guid),
    }
    .write_encrypted_client(&mut client, &mut c_enc)
    .unwrap();
    drain_world_entry(&mut client, &mut c_dec);
    (client, c_enc, c_dec, server)
}
