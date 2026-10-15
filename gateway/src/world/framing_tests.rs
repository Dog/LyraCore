use super::*;
use std::os::unix::net::UnixStream;

fn framing_store() -> std::sync::Arc<WorldFake> {
    std::sync::Arc::new(WorldFake {
        session: SessionState {
            username: "TESTER".into(),
            session: Some(WorldSession {
                account_id: 42,
                session_key: K,
            }),
            ..Default::default()
        },
        ..Default::default()
    })
}

fn framing_session() -> (
    UnixStream,
    EncrypterHalf,
    DecrypterHalf,
    std::thread::JoinHandle<Result<()>>,
) {
    let store = framing_store();
    let (mut client, server_end) = world_session_socket_pair();
    let server = std::thread::spawn(move || run_world_session(server_end, store.clone()));
    let (c_enc, c_dec) = client_handshake(&mut client, "TESTER", K);
    (client, c_enc, c_dec, server)
}

#[test]
fn a_clean_disconnect_with_no_frame_pending_ends_the_session_without_an_error() {
    let (client, _c_enc, _c_dec, server) = framing_session();
    drop(client);
    server
        .join()
        .unwrap()
        .expect("an EOF at a frame boundary is a normal disconnect, not a framing error");
}

#[test]
fn a_header_truncated_mid_read_is_a_disconnect_not_a_decoded_frame() {
    let (mut client, _c_enc, _c_dec, server) = framing_session();
    client.write_all(&[0xAA, 0xBB, 0xCC]).unwrap();
    drop(client);
    server.join().unwrap().expect(
        "half a header is a severed connection, not a frame — it must end the session cleanly \
         rather than decode into a size/opcode pair and hang on the body read",
    );
}

#[test]
fn a_body_shorter_than_its_declared_size_ends_the_session_with_an_error() {
    let (mut client, mut c_enc, _c_dec, server) = framing_session();
    c_enc
        .write_encrypted_client_header(&mut client, 4 + 64, 0x0037)
        .unwrap();
    client.write_all(&[0u8; 8]).unwrap();
    drop(client);

    let err = server
        .join()
        .unwrap()
        .expect_err("a body that never arrives must end the session, never block on it");
    let text = format!("{err:#}");
    assert!(
        text.contains("world read error (body)"),
        "a truncated body must be reported as a BODY read error so an operator can tell it from a \
         normal disconnect; got: {text}"
    );
}

#[test]
fn a_maximum_size_header_with_no_body_ends_the_session_instead_of_waiting_for_64_kib() {
    let (mut client, mut c_enc, _c_dec, server) = framing_session();
    c_enc
        .write_encrypted_client_header(&mut client, u16::MAX, 0x0037)
        .unwrap();
    drop(client);

    let err = server.join().unwrap().expect_err(
        "a header claiming the largest body a u16 can express, with nothing behind it, must end \
         the session",
    );
    assert!(
        format!("{err:#}").contains("world read error (body)"),
        "{err:#}"
    );
}

#[test]
fn a_size_field_below_the_opcode_width_never_underflows_the_body_length() {
    for size in 0u16..=3 {
        let (mut client, mut c_enc, _c_dec, server) = framing_session();
        c_enc
            .write_encrypted_client_header(&mut client, size, 0x0037)
            .unwrap();
        drop(client);

        let _resolved = server.join().unwrap_or_else(|_| {
            panic!(
                "size={size} (below the 4-byte opcode width) must not underflow `body_len` — the \
                 session thread died instead of returning"
            )
        });
    }
}

#[test]
fn an_unsupported_opcode_ends_the_session_rather_than_desyncing_the_keystream() {
    let (mut client, mut c_enc, _c_dec, server) = framing_session();
    c_enc
        .write_encrypted_client_header(&mut client, 4, 0xDEAD)
        .unwrap();
    drop(client);

    let err = server
        .join()
        .unwrap()
        .expect_err("an opcode with no parser must end the session");
    assert!(format!("{err:#}").contains("world read error"), "{err:#}");
}

#[test]
fn an_auth_session_after_auth_is_refused_before_the_typed_decoder_sees_it() {
    let (mut client, mut c_enc, _c_dec, server) = framing_session();
    let mut body = Vec::new();
    body.extend_from_slice(&[0u8; 8]);
    body.extend_from_slice(b"TESTER\0");
    body.extend_from_slice(&[0u8; 24]);
    body.extend_from_slice(&u32::MAX.to_le_bytes());
    body.extend_from_slice(&[0xFF; 32]);
    c_enc
        .write_encrypted_client_header(
            &mut client,
            (body.len() + 4) as u16,
            CMSG_AUTH_SESSION_OPCODE,
        )
        .unwrap();
    client.write_all(&body).unwrap();
    drop(client);

    let err = server
        .join()
        .unwrap()
        .expect_err("a post-auth CMSG_AUTH_SESSION must end the session");
    assert!(
        format!("{err:#}").contains("CMSG_AUTH_SESSION after auth"),
        "{err:#}"
    );
}

#[test]
fn a_known_opcode_with_an_undecodable_body_is_session_fatal_too() {
    let (mut client, mut c_enc, _c_dec, server) = framing_session();
    c_enc
        .write_encrypted_client_header(&mut client, 4 + 1, 0x012E)
        .unwrap();
    client.write_all(&[0x01]).unwrap();
    drop(client);

    let err = server
        .join()
        .unwrap()
        .expect_err("a known opcode whose body will not decode must still end the session");
    assert!(format!("{err:#}").contains("world read error"), "{err:#}");
}

#[test]
fn a_session_ending_in_a_framing_error_still_gives_its_seat_back() {
    let store = framing_store();
    let queue = std::sync::Arc::new(LoginQueue::new(1, 0));

    let (mut client, server_end) = world_session_socket_pair();
    let server_queue = queue.clone();
    let server = std::thread::spawn(move || {
        run_world_session_with_queue(server_end, store.clone(), &server_queue)
    });
    let (mut c_enc, _c_dec) = client_handshake(&mut client, "TESTER", K);
    assert_eq!(
        queue.active(),
        1,
        "precondition: the admitted session holds the only seat"
    );

    c_enc
        .write_encrypted_client_header(&mut client, 4, 0xDEAD)
        .unwrap();
    drop(client);
    server
        .join()
        .unwrap()
        .expect_err("the malformed frame must end the session with an error");

    assert_eq!(
        queue.active(),
        0,
        "the seat must be released on the ERROR path too — otherwise every malformed frame \
         permanently shrinks the realm's capacity by one"
    );
    assert_eq!(
        queue.request(),
        Admission::Admitted,
        "and the freed seat must be immediately usable by the next login"
    );
}

#[test]
fn a_session_that_dies_in_world_still_deletes_the_players_entity() {
    let store = std::sync::Arc::new(quest_store());
    let queue = std::sync::Arc::new(LoginQueue::unlimited());

    let (mut client, server_end) = world_session_socket_pair();
    let server_store = store.clone();
    let server_queue = queue.clone();
    let server = std::thread::spawn(move || {
        run_world_session_with_queue(server_end, server_store.clone(), &server_queue)
    });
    let (mut c_enc, mut c_dec) = client_handshake(&mut client, "TESTER", K);

    CMSG_PLAYER_LOGIN { guid: Guid::new(1) }
        .write_encrypted_client(&mut client, &mut c_enc)
        .unwrap();
    for _ in 0..10 {
        let _ = ServerOpcodeMessage::read_encrypted(&mut client, &mut c_dec);
    }
    assert!(
        !store
            .session
            .logout_called
            .load(std::sync::atomic::Ordering::SeqCst),
        "precondition: the player is in the world and has not logged out"
    );

    c_enc
        .write_encrypted_client_header(&mut client, 4, 0xDEAD)
        .unwrap();
    drop(client);
    server
        .join()
        .unwrap()
        .expect_err("the malformed frame must end the session with an error");

    assert!(
        store
            .session
            .logout_called
            .load(std::sync::atomic::Ordering::SeqCst),
        "an in-world session that ends on an error must still run `logout` — otherwise the \
         character stays spawned in the world with nobody driving it, and the player cannot relog \
         onto it"
    );
}

#[test]
fn a_malformed_addon_envelope_is_dropped_and_the_session_keeps_serving() {
    let store = framing_store();
    let (mut client, server_end) = world_session_socket_pair();
    let server = std::thread::spawn(move || run_world_session(server_end, store.clone()));
    let (mut c_enc, mut c_dec) = client_handshake(&mut client, "TESTER", K);

    let mut body = Vec::new();
    body.extend_from_slice(&0u32.to_le_bytes());
    body.extend_from_slice(&codec::addon::LANG_ADDON.to_le_bytes());
    body.extend_from_slice(b"NOTSTC|garbage\0");
    c_enc
        .write_encrypted_client_header(
            &mut client,
            4 + body.len() as u16,
            codec::addon::CMSG_MESSAGECHAT_OPCODE,
        )
        .unwrap();
    client.write_all(&body).unwrap();

    CMSG_CHAR_ENUM {}
        .write_encrypted_client(&mut client, &mut c_enc)
        .unwrap();
    let reply = ServerOpcodeMessage::read_encrypted(&mut client, &mut c_dec)
        .expect("the session must still answer after a malformed addon envelope");
    assert!(
        matches!(reply, ServerOpcodeMessage::SMSG_CHAR_ENUM(_)),
        "expected the char list, got {reply} — a foreign addon frame must never be session-fatal"
    );

    drop(client);
    server
        .join()
        .unwrap()
        .expect("a dropped addon frame must leave the session ending cleanly");
}

#[test]
fn an_authenticated_stc_order_uses_the_logged_in_actor_and_keeps_the_session_live() {
    let store = std::sync::Arc::new({
        let base = tester_store(42);
        WorldFake {
            session: SessionState {
                login_entity: Some(warrior_entity()),
                ..base.session
            },
            ..base
        }
    });
    let (mut client, mut c_enc, mut c_dec, server) = enter_world(store.clone(), 1);

    for text in [
        "STC\tv1|example.order|same|1/1|follow|77\0",
        "STC\tv1||example.order||changed||1/1||follow||77\0",
    ] {
        let mut body = Vec::new();
        body.extend_from_slice(&1u32.to_le_bytes());
        body.extend_from_slice(&codec::addon::LANG_ADDON.to_le_bytes());
        body.extend_from_slice(text.as_bytes());
        c_enc
            .write_encrypted_client_header(
                &mut client,
                4 + body.len() as u16,
                codec::addon::CMSG_MESSAGECHAT_OPCODE,
            )
            .unwrap();
        client.write_all(&body).unwrap();
    }

    CMSG_CHAR_ENUM {}
        .write_encrypted_client(&mut client, &mut c_enc)
        .unwrap();
    assert!(matches!(
        ServerOpcodeMessage::read_encrypted(&mut client, &mut c_dec).unwrap(),
        ServerOpcodeMessage::SMSG_CHAR_ENUM(_)
    ));
    assert_eq!(
        *store.session.client_commands.lock().unwrap(),
        vec![
            (1, "example.order".into(), "follow|77".into()),
            (1, "example.order".into(), "follow|77".into()),
        ]
    );
    drop(client);
    server.join().unwrap();
}
