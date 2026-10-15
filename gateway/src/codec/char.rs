use super::*;

#[derive(Clone, Debug, Default)]
pub struct CharacterView {
    pub guid: u64,
    pub name: String,
    pub race: u8,
    pub class: u8,
    pub gender: u8,
    pub skin: u8,
    pub face: u8,
    pub hair_style: u8,
    pub hair_color: u8,
    pub facial_hair: u8,
    pub level: u8,
    pub map_id: u32,
    pub zone_id: u32,
    pub x: f32,
    pub y: f32,
    pub z: f32,
    pub first_login: bool,
    pub equipment: [CharacterGear; 19],
    /// Whole seconds excluding the current World Session.
    pub played_total_secs: u32,
    /// Unix-epoch microseconds; zero means offline.
    pub session_start_micros: u64,
    pub guild_id: u32,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Appearance {
    pub skin: u8,
    pub face: u8,
    pub hair_style: u8,
    pub hair_color: u8,
    pub facial_hair: u8,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CharCreateOutcome {
    Success,
    NameInUse,
    ServerLimit,
    Failed,
}

pub fn build_char_create_response(outcome: CharCreateOutcome) -> SMSG_CHAR_CREATE {
    SMSG_CHAR_CREATE {
        result: match outcome {
            CharCreateOutcome::Success => WorldResult::CharCreateSuccess,
            CharCreateOutcome::NameInUse => WorldResult::CharCreateNameInUse,
            CharCreateOutcome::ServerLimit => WorldResult::CharCreateServerLimit,
            CharCreateOutcome::Failed => WorldResult::CharCreateError,
        },
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CharDeleteOutcome {
    Success,
    Failed,
}

pub fn build_char_delete_response(outcome: CharDeleteOutcome) -> SMSG_CHAR_DELETE {
    SMSG_CHAR_DELETE {
        result: match outcome {
            CharDeleteOutcome::Success => WorldResult::CharDeleteSuccess,
            CharDeleteOutcome::Failed => WorldResult::CharDeleteFailed,
        },
    }
}

pub fn build_char_enum(chars: &[CharacterView]) -> Result<SMSG_CHAR_ENUM> {
    let mut ordered: Vec<&CharacterView> = chars.iter().collect();
    ordered.sort_unstable_by_key(|c| c.guid);
    let mut characters = Vec::with_capacity(ordered.len());
    for c in ordered {
        characters.push(Character {
            guid: Guid::new(c.guid),
            name: c.name.clone(),
            race: Race::try_from(c.race).map_err(|_| anyhow!("invalid race {}", c.race))?,
            class: Class::try_from(c.class).map_err(|_| anyhow!("invalid class {}", c.class))?,
            gender: Gender::try_from(c.gender)
                .map_err(|_| anyhow!("invalid gender {}", c.gender))?,
            skin: c.skin,
            face: c.face,
            hair_style: c.hair_style,
            hair_color: c.hair_color,
            facial_hair: c.facial_hair,
            level: Level::new(c.level),
            area: Area::try_from(c.zone_id).unwrap_or_default(),
            map: Map::try_from(c.map_id).map_err(|_| anyhow!("invalid map {}", c.map_id))?,
            position: Vector3d {
                x: c.x,
                y: c.y,
                z: c.z,
            },
            guild_id: c.guild_id,
            flags: CharacterFlags::empty(),
            first_login: c.first_login,
            pet_display_id: 0,
            pet_level: Level::new(0),
            pet_family: CreatureFamily::default(),
            equipment: c.equipment,
        });
    }
    Ok(SMSG_CHAR_ENUM { characters })
}

pub fn build_name_query_response(c: &CharacterView) -> Result<SMSG_NAME_QUERY_RESPONSE> {
    Ok(SMSG_NAME_QUERY_RESPONSE {
        guid: Guid::new(c.guid),
        character_name: c.name.clone(),
        realm_name: String::new(),
        race: Race::try_from(c.race).map_err(|_| anyhow!("invalid race {}", c.race))?,
        gender: Gender::try_from(c.gender).map_err(|_| anyhow!("invalid gender {}", c.gender))?,
        class: Class::try_from(c.class).map_err(|_| anyhow!("invalid class {}", c.class))?,
    })
}

/// The client renders inspection from the target's existing visible-item fields.
pub fn build_inspect_response(target_guid: u64) -> SMSG_INSPECT {
    SMSG_INSPECT {
        guid: Guid::new(target_guid),
    }
}

pub fn logout_sequence() -> Vec<ServerOpcodeMessage> {
    vec![
        ServerOpcodeMessage::SMSG_LOGOUT_RESPONSE(SMSG_LOGOUT_RESPONSE {
            result: LogoutResult::Success,
            speed: LogoutSpeed::Instant,
        }),
        ServerOpcodeMessage::SMSG_LOGOUT_COMPLETE,
    ]
}

pub fn logout_denied_in_combat() -> SMSG_LOGOUT_RESPONSE {
    SMSG_LOGOUT_RESPONSE {
        result: LogoutResult::FailureInCombat,
        speed: LogoutSpeed::Instant,
    }
}

pub fn build_played_time(
    played_total_secs: u32,
    session_start_micros: u64,
    now_micros: u64,
) -> SMSG_PLAYED_TIME {
    let live_secs = if session_start_micros == 0 {
        0
    } else {
        now_micros.saturating_sub(session_start_micros) / 1_000_000
    };
    let total = played_total_secs.saturating_add(live_secs as u32);
    SMSG_PLAYED_TIME {
        total_played_time: total,
        level_played_time: total,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn char_create_response_maps_every_outcome() {
        assert_eq!(
            build_char_create_response(CharCreateOutcome::Success).result,
            WorldResult::CharCreateSuccess
        );
        assert_eq!(
            build_char_create_response(CharCreateOutcome::NameInUse).result,
            WorldResult::CharCreateNameInUse
        );
        assert_eq!(
            build_char_create_response(CharCreateOutcome::ServerLimit).result,
            WorldResult::CharCreateServerLimit
        );
        assert_eq!(
            build_char_create_response(CharCreateOutcome::Failed).result,
            WorldResult::CharCreateError
        );
    }

    #[test]
    fn char_delete_response_maps_both_outcomes() {
        assert_eq!(
            build_char_delete_response(CharDeleteOutcome::Success).result,
            WorldResult::CharDeleteSuccess
        );
        assert_eq!(
            build_char_delete_response(CharDeleteOutcome::Failed).result,
            WorldResult::CharDeleteFailed
        );
    }

    #[test]
    fn logout_sequence_acks_then_completes_instantly() {
        let seq = logout_sequence();
        assert_eq!(seq.len(), 2);
        match &seq[0] {
            ServerOpcodeMessage::SMSG_LOGOUT_RESPONSE(r) => {
                assert_eq!(r.result, LogoutResult::Success);
                assert_eq!(r.speed, LogoutSpeed::Instant);
            }
            other => panic!("expected SMSG_LOGOUT_RESPONSE, got {other}"),
        }
        assert!(matches!(seq[1], ServerOpcodeMessage::SMSG_LOGOUT_COMPLETE));
    }

    #[test]
    fn logout_denied_in_combat_carries_the_failure_result() {
        let msg = logout_denied_in_combat();
        assert_eq!(msg.result, LogoutResult::FailureInCombat);
    }

    #[test]
    fn played_time_folds_the_live_session_span_into_the_durable_total() {
        let offline = build_played_time(3600, 0, 999_999);
        assert_eq!(offline.total_played_time, 3600);
        assert_eq!(offline.level_played_time, 3600);
        let now = 10_000_000_000u64;
        let session_start = now - 5_000_000;
        let online = build_played_time(3600, session_start, now);
        assert_eq!(online.total_played_time, 3605);
    }

    #[test]
    fn played_time_never_underflows_a_clock_that_moved_backward() {
        let msg = build_played_time(100, 5_000_000, 1_000_000);
        assert_eq!(
            msg.total_played_time, 100,
            "backward clock must not inflate the total"
        );
    }
}
