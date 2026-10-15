use crate::world::{ShardRoutingStore, SocialStore};
use anyhow::Result;

use super::presence::{self, AwayStatus};
use super::WhisperTargetFacts;
use lyracore_shared::chat::chat_kind;

pub(crate) fn target_facts<St: ShardRoutingStore + SocialStore + ?Sized>(
    store: &St,
    speaker_guid: u64,
    typed_name: &str,
) -> Result<Option<WhisperTargetFacts>> {
    let mut unreadable = None;
    let mut target = None;
    for guid in presence::resolve_all_by_name(store, typed_name)? {
        match presence::of(store, guid) {
            Ok(Some(presence)) if presence.session_online => {
                target = Some(presence);
                break;
            }
            Ok(_) => {}
            Err(error) => unreadable = Some(error),
        }
    }
    let Some(target) = target else {
        return match unreadable {
            Some(error) => Err(error),
            None => Ok(None),
        };
    };
    let ignores_speaker = ignored_anywhere(store, target.guid, speaker_guid)?;
    let (away_kind, away_message) = match presence::auto_reply(store, &target)? {
        Some(reply) => (away_kind_of(reply.away), reply.message),
        None => (0, String::new()),
    };
    Ok(Some(WhisperTargetFacts {
        guid: target.guid,
        race: target.race,
        name: target.name,
        ignores_speaker,
        away_kind,
        away_message,
    }))
}

fn away_kind_of(away: AwayStatus) -> u8 {
    match away {
        AwayStatus::None => 0,
        AwayStatus::Afk => chat_kind::AFK,
        AwayStatus::Dnd => chat_kind::DND,
    }
}

pub(crate) fn ignored_anywhere<St: ShardRoutingStore + SocialStore + ?Sized>(
    store: &St,
    owner_guid: u64,
    other_guid: u64,
) -> Result<bool> {
    if store.ignored_guids(owner_guid)?.contains(&other_guid) {
        return Ok(true);
    }
    Ok(store.world_stores().iter().any(|shard| {
        shard
            .ignored_guids(owner_guid)
            .map(|ignored| ignored.contains(&other_guid))
            .unwrap_or(false)
    }))
}
