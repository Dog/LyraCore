use super::super::*;

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
}

pub(crate) struct Character;

impl<St: CharacterStore + GuildActionStore + SessionStore + ?Sized> ProtocolFamily<St>
    for Character
{
    #[allow(clippy::too_many_lines)]
    fn handle(
        store: &St,
        session: &mut ProtocolSession,
        request: ProtocolRequest,
    ) -> Result<ProtocolReply> {
        let mut reply = ProtocolReply::default();
        let response = match request.message()? {
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
            ClientOpcodeMessage::CMSG_PLAYER_LOGIN(request) => {
                let character = Actor::new(request.guid.guid())
                    .ok_or_else(|| anyhow!("CMSG_PLAYER_LOGIN names no Character"))?;
                reply.after_queue = Some(WorldSessionAction::Login(character));
                None
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
            message => return Err(anyhow!("Character received an unowned opcode: {message}")),
        };
        if let Some(response) = response {
            reply.outbound.push(Outbound::One(response));
        }
        Ok(reply)
    }
}
