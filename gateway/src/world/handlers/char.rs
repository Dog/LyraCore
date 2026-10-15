use super::super::*;
use super::trainer::settle_per_action;

pub(crate) trait CharacterStore: Send + Sync {
    fn characters(&self, account_id: u64) -> Result<Vec<codec::CharacterView>>;

    fn create_character(
        &self,
        account_id: u64,
        name: &str,
        race: u8,
        class: u8,
        gender: u8,
        appearance: codec::Appearance,
    ) -> Result<codec::CharCreateOutcome>;

    fn delete_character(
        &self,
        account_id: u64,
        character: Actor,
    ) -> Result<codec::CharDeleteOutcome>;

    fn character_by_guid(&self, guid: u64) -> Result<Option<codec::CharacterView>>;

    fn character_exists_on_any_world_shard(&self, guid: u64) -> Result<bool>;

    fn player_skills(&self, character_guid: u64) -> Result<Vec<(u32, u16, u16)>>;

    fn effective_armor(&self, guid: u64) -> u32;

    fn effective_magic_resistances(&self, guid: u64) -> [u32; 6];

    fn spell_modifiers(&self, character_guid: u64) -> Vec<(u32, u8, i32, bool)>;

    fn player_learned_spells(&self, player_guid: u64) -> Result<Vec<u32>>;

    fn player_reputations(&self, player_guid: u64) -> Result<Vec<(i32, i32, bool)>>;

    fn player_actions(&self, player_guid: u64) -> Result<Vec<(u8, u32, u8)>>;

    /// A state the client may not pick is a Refusal; any other `Err` is a Transport Loss.
    fn set_stand_state(&self, actor: Actor, state: u8) -> Result<()>;

    /// 0 when the Character has no live entity.
    fn stand_state(&self, guid: u64) -> u8;
}

pub(crate) struct Character;

fn is_ignored(message: &ClientOpcodeMessage) -> bool {
    match message {
        ClientOpcodeMessage::CMSG_BUG(_) => {}
        ClientOpcodeMessage::CMSG_GMSURVEY_SUBMIT(_) => {}
        ClientOpcodeMessage::CMSG_WHOIS(_) => {}
        ClientOpcodeMessage::CMSG_WARDEN_DATA(_) => {}
        _ => return false,
    }
    true
}

impl<St: CharacterStore + GuildActionStore + SessionStore + SpeechStore + ?Sized> ProtocolFamily<St>
    for Character
{
    #[allow(clippy::too_many_lines)]
    fn handle(
        store: &St,
        session: &mut ProtocolSession,
        request: ProtocolRequest,
    ) -> Result<ProtocolReply> {
        let mut reply = ProtocolReply::default();
        let message = request.message()?;
        if is_ignored(&message) {
            return Ok(reply);
        }
        let response = match message {
            ClientOpcodeMessage::CMSG_PING(ping) => Some(ServerOpcodeMessage::SMSG_PONG(
                wow_world_messages::vanilla::SMSG_PONG {
                    sequence_id: ping.sequence_id,
                },
            )),
            ClientOpcodeMessage::CMSG_QUERY_TIME => {
                let seconds = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)?
                    .as_secs();
                Some(ServerOpcodeMessage::SMSG_QUERY_TIME_RESPONSE(
                    wow_world_messages::vanilla::SMSG_QUERY_TIME_RESPONSE {
                        time: u32::try_from(seconds)?,
                    },
                ))
            }
            ClientOpcodeMessage::CMSG_CHAR_ENUM => {
                let characters = store.characters(session.account_id)?;
                Some(ServerOpcodeMessage::SMSG_CHAR_ENUM(Box::new(
                    codec::build_char_enum(&characters)?,
                )))
            }
            ClientOpcodeMessage::CMSG_CHAR_CREATE(character) => {
                let appearance = codec::Appearance {
                    skin: character.skin_color,
                    face: character.face,
                    hair_style: character.hair_style,
                    hair_color: character.hair_color,
                    facial_hair: character.facial_hair,
                };
                let outcome = store.create_character(
                    session.account_id,
                    character.name.as_str(),
                    character.race.as_int(),
                    character.class.as_int(),
                    character.gender.as_int(),
                    appearance,
                )?;
                Some(ServerOpcodeMessage::SMSG_CHAR_CREATE(
                    codec::build_char_create_response(outcome),
                ))
            }
            ClientOpcodeMessage::CMSG_CHAR_DELETE(request) => {
                let character_guid = request.guid.guid();
                let outcome = match (
                    Actor::new(character_guid),
                    super::leads_a_guild(store, character_guid),
                ) {
                    (None, _) => codec::CharDeleteOutcome::Failed,
                    (Some(character), Ok(false)) => {
                        store.delete_character(session.account_id, character)?
                    }
                    (Some(_), Ok(true)) => {
                        log::info!("world: Guild Leader {character_guid} is not deleted");
                        codec::CharDeleteOutcome::Failed
                    }
                    (Some(_), Err(error)) => {
                        log::warn!(
                            "world: Guild Leader check for {character_guid} failed, not deleted: \
                             {error:#}"
                        );
                        codec::CharDeleteOutcome::Failed
                    }
                };
                Some(ServerOpcodeMessage::SMSG_CHAR_DELETE(
                    codec::build_char_delete_response(outcome),
                ))
            }
            ClientOpcodeMessage::CMSG_CHAR_RENAME(_) => Some(
                ServerOpcodeMessage::SMSG_CHAR_RENAME(Box::new(codec::build_char_rename_refusal())),
            ),
            ClientOpcodeMessage::CMSG_PLAYER_LOGIN(request) => {
                match Actor::new(request.guid.guid()) {
                    Some(character) => {
                        reply.after_queue = Some(WorldSessionAction::Login(character));
                        None
                    }
                    None => Some(ServerOpcodeMessage::SMSG_CHARACTER_LOGIN_FAILED(
                        codec::build_character_login_failed(),
                    )),
                }
            }
            ClientOpcodeMessage::MSG_MOVE_WORLDPORT_ACK => {
                reply.after_queue = Some(WorldSessionAction::WorldPortAck);
                None
            }
            ClientOpcodeMessage::CMSG_LOGOUT_CANCEL => {
                Some(ServerOpcodeMessage::SMSG_LOGOUT_CANCEL_ACK)
            }
            ClientOpcodeMessage::CMSG_LOGOUT_REQUEST | ClientOpcodeMessage::CMSG_PLAYER_LOGOUT => {
                if let WorldState::InWorld(world) = &session.state {
                    let now_ms = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_millis() as u64;
                    if store.player_combat_until_ms(world.self_guid) > now_ms {
                        return Ok(
                            vec![Outbound::One(ServerOpcodeMessage::SMSG_LOGOUT_RESPONSE(
                                codec::logout_denied_in_combat(),
                            ))]
                            .into(),
                        );
                    }
                }
                reply
                    .outbound
                    .push(Outbound::Batch(codec::logout_sequence()));
                reply.after_queue = Some(WorldSessionAction::Logout);
                None
            }
            ClientOpcodeMessage::CMSG_PLAYED_TIME => {
                if let Some(guid) = session.self_guid() {
                    if let Some(character) = store.character_by_guid(guid)? {
                        let now_micros = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_micros() as u64;
                        reply
                            .outbound
                            .push(Outbound::One(ServerOpcodeMessage::SMSG_PLAYED_TIME(
                                codec::build_played_time(
                                    character.played_total_secs,
                                    character.session_start_micros,
                                    now_micros,
                                ),
                            )));
                    }
                }
                None
            }
            ClientOpcodeMessage::CMSG_REQUEST_ACCOUNT_DATA(request) => {
                if let Some((opcode, body)) = codec::build_empty_account_data(request.data_type) {
                    reply.outbound.push(Outbound::Raw { opcode, body });
                }
                None
            }
            // Text emote 0 names no chat line, so observers see the animation only.
            ClientOpcodeMessage::CMSG_EMOTE(request) => {
                use wow_world_messages::vanilla::Emote;
                if let (Some(actor), Emote::OneshotNone | Emote::OneshotWave) =
                    (session.actor(), request.emote)
                {
                    settle_per_action(
                        "send_emote",
                        session.account_id,
                        store.send_emote(actor, 0, request.emote.as_int(), 0),
                    )?;
                }
                None
            }
            // The client applies its request at once, so a Refusal sends the stored state back.
            ClientOpcodeMessage::CMSG_STANDSTATECHANGE(request) => match session.actor() {
                Some(actor) => {
                    let requested = request.animation_state.as_int();
                    match store.set_stand_state(actor, requested) {
                        Ok(()) => None,
                        Err(error) => {
                            settle_per_action("set_stand_state", session.account_id, Err(error))?;
                            wow_world_messages::vanilla::UnitStandState::try_from(
                                store.stand_state(actor.guid()),
                            )
                            .ok()
                            .map(|stored| {
                                ServerOpcodeMessage::SMSG_STANDSTATE_UPDATE(
                                    codec::build_stand_state_update(stored),
                                )
                            })
                        }
                    }
                }
                None => None,
            },
            message => return Err(anyhow!("Character received an unowned opcode: {message}")),
        };
        if let Some(response) = response {
            reply.outbound.push(Outbound::One(response));
        }
        Ok(reply)
    }
}

/// Dropped before the protocol library reads them: it has no reader, or its reader is unsafe.
pub(crate) fn is_ignored_raw_request(opcode: u32) -> bool {
    const CMSG_ZONE_MAP: u32 = 0x000a;
    const CMSG_UNDRESSPLAYER: u32 = 0x0020;
    // The Module resolves the zone from terrain; the reader rejects unnamed area ids.
    const CMSG_ZONEUPDATE: u32 = 0x01f4;
    // The reader unwraps the client's zlib stream and sizes a buffer from its length.
    const CMSG_UPDATE_ACCOUNT_DATA: u32 = 0x020b;
    const CMSG_GM_REQUEST_PLAYER_INFO: u32 = 0x022f;
    const CMSG_CHAT_FILTERED: u32 = 0x0331;
    const CMSG_LOTTERY_QUERY_OBSOLETE: u32 = 0x0334;
    matches!(
        opcode,
        CMSG_ZONE_MAP
            | CMSG_UNDRESSPLAYER
            | CMSG_ZONEUPDATE
            | CMSG_UPDATE_ACCOUNT_DATA
            | CMSG_GM_REQUEST_PLAYER_INFO
            | CMSG_CHAT_FILTERED
            | CMSG_LOTTERY_QUERY_OBSOLETE
    )
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::stdb::ReducerCallError;
    use crate::world::test_support::{CharacterState, WorldFake};
    use wow_world_messages::vanilla::{
        Emote, Guid, UnitStandState, CMSG_BUG, CMSG_CHAR_RENAME, CMSG_EMOTE, CMSG_PLAYER_LOGIN,
        CMSG_REQUEST_ACCOUNT_DATA, CMSG_STANDSTATECHANGE, CMSG_WARDEN_DATA, CMSG_WHOIS,
    };

    fn handle(
        store: &WorldFake,
        session: &mut ProtocolSession,
        message: ClientOpcodeMessage,
    ) -> ProtocolReply {
        Character::handle(store, session, message.into()).unwrap()
    }

    fn sit() -> ClientOpcodeMessage {
        ClientOpcodeMessage::CMSG_STANDSTATECHANGE(CMSG_STANDSTATECHANGE {
            animation_state: UnitStandState::Sit,
        })
    }

    fn failing_stand_state(error: fn() -> anyhow::Error) -> WorldFake {
        WorldFake {
            character: CharacterState {
                stand_state_error: Some(error),
                ..Default::default()
            },
            ..Default::default()
        }
    }

    #[test]
    fn a_rename_is_refused_at_character_select() {
        let store = WorldFake::default();
        let mut session = ProtocolSession::new(7, "TESTER".into());
        let reply = handle(
            &store,
            &mut session,
            ClientOpcodeMessage::CMSG_CHAR_RENAME(Box::new(CMSG_CHAR_RENAME {
                character: Guid::new(1),
                new_name: "Renamed".into(),
            })),
        );
        assert!(matches!(
            reply.outbound.as_slice(),
            [Outbound::One(ServerOpcodeMessage::SMSG_CHAR_RENAME(answer))]
                if **answer == codec::build_char_rename_refusal()
        ));
    }

    #[test]
    fn a_login_that_names_no_character_fails_without_entering_the_world() {
        let store = WorldFake::default();
        let mut session = ProtocolSession::new(7, "TESTER".into());
        let reply = handle(
            &store,
            &mut session,
            ClientOpcodeMessage::CMSG_PLAYER_LOGIN(CMSG_PLAYER_LOGIN { guid: Guid::new(0) }),
        );
        assert!(reply.after_queue.is_none());
        assert!(matches!(
            reply.outbound.as_slice(),
            [Outbound::One(ServerOpcodeMessage::SMSG_CHARACTER_LOGIN_FAILED(answer))]
                if *answer == codec::build_character_login_failed()
        ));
    }

    #[test]
    fn an_account_data_request_gets_the_empty_section() {
        let store = WorldFake::default();
        let mut session = ProtocolSession::in_world(7, 1);
        let request = |data_type| {
            ClientOpcodeMessage::CMSG_REQUEST_ACCOUNT_DATA(CMSG_REQUEST_ACCOUNT_DATA { data_type })
        };
        let reply = handle(&store, &mut session, request(4));
        let (opcode, body) = codec::build_empty_account_data(4).unwrap();
        assert!(matches!(
            reply.outbound.as_slice(),
            [Outbound::Raw { opcode: sent, body: sent_body }]
                if *sent == opcode && *sent_body == body
        ));
        assert!(handle(&store, &mut session, request(8)).outbound.is_empty());
    }

    #[test]
    fn a_wave_plays_its_animation_without_a_text_line() {
        let store = WorldFake::default();
        let mut session = ProtocolSession::in_world(7, 1);
        for emote in [Emote::OneshotWave, Emote::OneshotNone, Emote::OneshotDance] {
            let reply = handle(
                &store,
                &mut session,
                ClientOpcodeMessage::CMSG_EMOTE(CMSG_EMOTE { emote }),
            );
            assert!(reply.outbound.is_empty());
        }
        assert_eq!(*store.speech.emotes.lock().unwrap(), [(0, 3, 0), (0, 0, 0)]);
    }

    #[test]
    fn sitting_down_is_stored_without_an_echo() {
        let store = WorldFake::default();
        let mut session = ProtocolSession::in_world(7, 1);
        let reply = handle(&store, &mut session, sit());
        assert_eq!(*store.character.stand_states.lock().unwrap(), [(1, 1)]);
        assert!(reply.outbound.is_empty());
    }

    #[test]
    fn a_refused_stand_state_snaps_the_client_back() {
        let mut store = failing_stand_state(|| {
            ReducerCallError::refused("gw_set_stand_state", "invalid stand state 2").into()
        });
        store.character.stored_stand_state = 8;
        let mut session = ProtocolSession::in_world(7, 1);
        let reply = handle(&store, &mut session, sit());
        assert!(matches!(
            reply.outbound.as_slice(),
            [Outbound::One(ServerOpcodeMessage::SMSG_STANDSTATE_UPDATE(update))]
                if update.state == UnitStandState::Kneel
        ));
    }

    #[test]
    fn a_lost_stand_state_request_ends_the_world_session() {
        let store =
            failing_stand_state(|| ReducerCallError::transport_lost("gw_set_stand_state").into());
        let mut session = ProtocolSession::in_world(7, 1);
        assert!(Character::handle(&store, &mut session, sit().into()).is_err());
    }

    #[test]
    fn a_stand_state_before_world_entry_is_not_stored() {
        let store = WorldFake::default();
        let mut session = ProtocolSession::new(7, "TESTER".into());
        assert!(handle(&store, &mut session, sit()).outbound.is_empty());
        assert!(store.character.stand_states.lock().unwrap().is_empty());
    }

    #[test]
    fn debug_gm_and_warden_requests_get_no_answer() {
        let store = WorldFake::default();
        let mut session = ProtocolSession::in_world(7, 1);
        for message in [
            ClientOpcodeMessage::CMSG_BUG(Box::new(CMSG_BUG {
                suggestion: 0,
                content: "bug".into(),
                bug_type: "type".into(),
            })),
            ClientOpcodeMessage::CMSG_WHOIS(Box::new(CMSG_WHOIS {
                character: "Tester".into(),
            })),
            ClientOpcodeMessage::CMSG_WARDEN_DATA(Box::new(CMSG_WARDEN_DATA {
                encrypted_data: vec![1, 2, 3],
            })),
        ] {
            let reply = handle(&store, &mut session, message);
            assert!(reply.outbound.is_empty());
            assert!(reply.after_queue.is_none());
        }
    }
}
