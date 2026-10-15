use super::*;

#[test]
fn messagechat_say_and_yell_route_to_chat_types_0_and_1() {
    let store = std::sync::Arc::new(quest_store());
    let (mut client, mut c_enc, _c_dec, server) = enter_world(store.clone(), 1);
    CMSG_MESSAGECHAT {
        chat_type: CMSG_MESSAGECHAT_ChatType::Say,
        language: Language::Universal,
        message: "hi".into(),
    }
    .write_encrypted_client(&mut client, &mut c_enc)
    .unwrap();
    CMSG_MESSAGECHAT {
        chat_type: CMSG_MESSAGECHAT_ChatType::Yell,
        language: Language::Universal,
        message: "HEY".into(),
    }
    .write_encrypted_client(&mut client, &mut c_enc)
    .unwrap();
    drop(client);
    server.join().unwrap();
    assert_eq!(
        store.speech.chats.lock().unwrap().as_slice(),
        &[(0, 0, "hi".to_string()), (1, 0, "HEY".to_string())],
        "Say → type 0, Yell → type 1, language threaded"
    );
}

#[test]
fn messagechat_emote_routes_to_chat_type_3() {
    let store = std::sync::Arc::new(quest_store());
    let (mut client, mut c_enc, _c_dec, server) = enter_world(store.clone(), 1);
    CMSG_MESSAGECHAT {
        chat_type: CMSG_MESSAGECHAT_ChatType::Emote,
        language: Language::Common,
        message: "waves wildly.".into(),
    }
    .write_encrypted_client(&mut client, &mut c_enc)
    .unwrap();
    drop(client);
    server.join().unwrap();
    assert_eq!(
        store.speech.chats.lock().unwrap().as_slice(),
        &[(3, 7, "waves wildly.".to_string())],
        "/e → type 3, language threaded to the Module (which stores it as Universal)"
    );
}

#[test]
fn messagechat_dot_say_diverts_to_gm_command_never_touching_chat() {
    let store = std::sync::Arc::new(quest_store());
    let (mut client, mut c_enc, mut c_dec, server) = enter_world(store.clone(), 1);
    CMSG_MESSAGECHAT {
        chat_type: CMSG_MESSAGECHAT_ChatType::Say,
        language: Language::Universal,
        message: ".heal".into(),
    }
    .write_encrypted_client(&mut client, &mut c_enc)
    .unwrap();
    CMSG_QUESTGIVER_STATUS_QUERY {
        guid: Guid::new(50),
    }
    .write_encrypted_client(&mut client, &mut c_enc)
    .unwrap();
    match ServerOpcodeMessage::read_encrypted(&mut client, &mut c_dec).unwrap() {
        ServerOpcodeMessage::SMSG_QUESTGIVER_STATUS(_) => {}
        other => panic!("expected the sentinel (no reply on gm_command success), got {other}"),
    }
    drop(client);
    server.join().unwrap();
    assert_eq!(
        store.speech.gm_commands.lock().unwrap().as_slice(),
        &[("TESTER".to_string(), ".heal".to_string())],
        "the proof-validated Account name and raw dot-command must reach the Store together"
    );
    assert!(
        store.speech.chats.lock().unwrap().is_empty(),
        "a dot-command must NEVER reach send_chat"
    );
}

#[test]
fn messagechat_non_dot_say_is_byte_identical_to_before_223() {
    let store = std::sync::Arc::new(quest_store());
    let (mut client, mut c_enc, _c_dec, server) = enter_world(store.clone(), 1);
    CMSG_MESSAGECHAT {
        chat_type: CMSG_MESSAGECHAT_ChatType::Say,
        language: Language::Universal,
        message: "hi".into(),
    }
    .write_encrypted_client(&mut client, &mut c_enc)
    .unwrap();
    drop(client);
    server.join().unwrap();
    assert_eq!(
        store.speech.chats.lock().unwrap().as_slice(),
        &[(0u8, 0u8, "hi".to_string())]
    );
    assert!(
        store.speech.gm_commands.lock().unwrap().is_empty(),
        "a plain Say line must never reach gm_command"
    );
}

#[test]
fn messagechat_dot_say_error_relays_a_system_chat_line_to_the_sender_only() {
    let mut s = quest_store();
    s.speech.gm_command_error = Some(|| {
        crate::stdb::ReducerCallError::refused("gw_gm_command", "permission denied").into()
    });
    let store = std::sync::Arc::new(s);
    let (mut client, mut c_enc, mut c_dec, server) = enter_world(store.clone(), 1);
    CMSG_MESSAGECHAT {
        chat_type: CMSG_MESSAGECHAT_ChatType::Say,
        language: Language::Universal,
        message: ".god".into(),
    }
    .write_encrypted_client(&mut client, &mut c_enc)
    .unwrap();
    match ServerOpcodeMessage::read_encrypted(&mut client, &mut c_dec).unwrap() {
        ServerOpcodeMessage::SMSG_MESSAGECHAT(m) => {
            assert_eq!(m.message, "permission denied");
            assert!(
                matches!(
                    m.chat_type,
                    wow_world_messages::vanilla::SMSG_MESSAGECHAT_ChatType::System { .. }
                ),
                "expected a System chat line, got {:?}",
                m.chat_type
            );
        }
        other => panic!("expected SMSG_MESSAGECHAT System, got {other}"),
    }
    drop(client);
    server.join().unwrap();
    assert!(store.speech.chats.lock().unwrap().is_empty());
}

#[test]
fn messagechat_dot_say_transport_loss_ends_the_world_session() {
    let mut s = quest_store();
    s.speech.gm_command_error =
        Some(|| crate::stdb::ReducerCallError::transport_lost("gw_gm_command").into());
    let store = std::sync::Arc::new(s);
    let (mut client, server_end) = world_session_socket_pair();
    let server = std::thread::spawn(move || run_world_session(server_end, store));
    let (mut c_enc, mut c_dec) = client_handshake(&mut client, "TESTER", K);
    CMSG_PLAYER_LOGIN { guid: Guid::new(1) }
        .write_encrypted_client(&mut client, &mut c_enc)
        .unwrap();
    drain_world_entry(&mut client, &mut c_dec);
    CMSG_MESSAGECHAT {
        chat_type: CMSG_MESSAGECHAT_ChatType::Say,
        language: Language::Universal,
        message: ".god".into(),
    }
    .write_encrypted_client(&mut client, &mut c_enc)
    .unwrap();
    while let Ok(message) = ServerOpcodeMessage::read_encrypted(&mut client, &mut c_dec) {
        assert!(
            !matches!(message, ServerOpcodeMessage::SMSG_MESSAGECHAT(_)),
            "a lost command is not a Refusal the GM reads"
        );
    }
    assert!(
        server.join().unwrap().is_err(),
        "a Transport Loss ends the World Session"
    );
}

fn chat_store() -> WorldFake {
    let base = quest_store();
    WorldFake {
        chat: ChatState {
            speaker_facts: Some(human_speaker()),
            ..base.chat
        },
        characters: vec![codec::CharacterView {
            guid: 1,
            name: "Tester".into(),
            ..Default::default()
        }],
        ..base
    }
}

fn say_line(message: &str) -> CMSG_MESSAGECHAT {
    CMSG_MESSAGECHAT {
        chat_type: CMSG_MESSAGECHAT_ChatType::Say,
        language: Language::Common,
        message: message.into(),
    }
}

fn notification(message: ServerOpcodeMessage) -> String {
    match message {
        ServerOpcodeMessage::SMSG_NOTIFICATION(notice) => notice.notification,
        other => panic!("expected SMSG_NOTIFICATION, got {other}"),
    }
}

#[test]
fn a_flooding_session_is_muted_before_any_chat_durable_request() {
    let store = std::sync::Arc::new(chat_store());
    let (mut client, mut c_enc, mut c_dec, server) = enter_world(store.clone(), 1);
    for n in 0..12 {
        say_line(&format!("line {n}"))
            .write_encrypted_client(&mut client, &mut c_enc)
            .unwrap();
    }
    CMSG_MESSAGECHAT {
        chat_type: CMSG_MESSAGECHAT_ChatType::Party,
        language: Language::Common,
        message: "form up".into(),
    }
    .write_encrypted_client(&mut client, &mut c_enc)
    .unwrap();
    for _ in 0..2 {
        assert_eq!(
            notification(ServerOpcodeMessage::read_encrypted(&mut client, &mut c_dec).unwrap()),
            "You must wait 10 Second(s). before speaking again."
        );
    }
    sync(&mut client, &mut c_enc, &mut c_dec, |_, _| {});
    drop(client);
    server.join().unwrap();
    assert_eq!(store.speech.chats.lock().unwrap().len(), 11);
    assert!(store.chat.realm_chats.lock().unwrap().is_empty());
}

#[test]
fn a_muted_session_still_sends_addon_lines() {
    let store = std::sync::Arc::new(chat_store());
    let (mut client, mut c_enc, mut c_dec, server) = enter_world(store.clone(), 1);
    for n in 0..12 {
        say_line(&format!("line {n}"))
            .write_encrypted_client(&mut client, &mut c_enc)
            .unwrap();
    }
    CMSG_MESSAGECHAT {
        chat_type: CMSG_MESSAGECHAT_ChatType::Guild,
        language: Language::Addon,
        message: "LCTEST\tping".into(),
    }
    .write_encrypted_client(&mut client, &mut c_enc)
    .unwrap();
    notification(ServerOpcodeMessage::read_encrypted(&mut client, &mut c_dec).unwrap());
    sync(&mut client, &mut c_enc, &mut c_dec, |_, _| {});
    drop(client);
    server.join().unwrap();
    let requests = store.chat.realm_chats.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].1.language, 0xFFFF_FFFF);
}

#[test]
fn a_game_master_is_never_muted_for_flooding() {
    let mut s = chat_store();
    s.chat.gm_level = 1;
    let store = std::sync::Arc::new(s);
    let (mut client, mut c_enc, mut c_dec, server) = enter_world(store.clone(), 1);
    sync(&mut client, &mut c_enc, &mut c_dec, |c, e| {
        for n in 0..15 {
            say_line(&format!("line {n}"))
                .write_encrypted_client(&mut *c, &mut *e)
                .unwrap();
        }
    });
    drop(client);
    server.join().unwrap();
    assert_eq!(store.speech.chats.lock().unwrap().len(), 15);
}

#[test]
fn a_say_line_in_a_language_the_speaker_does_not_know_answers_the_vanilla_notice() {
    let mut s = chat_store();
    s.speech.send_chat_outcome = Some(ChatOutcome::Refused(
        lyracore_shared::chat::ChatRefusal::UnknownLanguage,
    ));
    let store = std::sync::Arc::new(s);
    let (mut client, mut c_enc, mut c_dec, server) = enter_world(store.clone(), 1);
    CMSG_MESSAGECHAT {
        chat_type: CMSG_MESSAGECHAT_ChatType::Say,
        language: Language::Orcish,
        message: "zug zug".into(),
    }
    .write_encrypted_client(&mut client, &mut c_enc)
    .unwrap();
    assert_eq!(
        notification(ServerOpcodeMessage::read_encrypted(&mut client, &mut c_dec).unwrap()),
        "You don't know that language"
    );
    sync(&mut client, &mut c_enc, &mut c_dec, |_, _| {});
    drop(client);
    server.join().unwrap();
}
