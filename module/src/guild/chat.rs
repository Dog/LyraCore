use spacetimedb::ReducerContext;

use lyracore_shared::chat::{chat_kind, ChatRefusal};
use lyracore_shared::guild::{has_right, rights};

use super::{game_guild_member, member, ranks, GuildRank};
use crate::realm_chat::{ChatAudience, RealmChatRequest};

pub(crate) fn guild_chat_audience(
    ctx: &ReducerContext,
    speaker_guid: u64,
    request: &RealmChatRequest,
) -> Result<ChatAudience, ChatRefusal> {
    let (speak_right, listen_right) = if request.kind == chat_kind::OFFICER {
        (rights::OFFCHATSPEAK, rights::OFFCHATLISTEN)
    } else {
        (rights::GCHATSPEAK, rights::GCHATLISTEN)
    };
    let speaker = member(ctx, speaker_guid).ok_or(ChatRefusal::NotInGuild)?;
    let guild_ranks = ranks(ctx, speaker.guild_id);
    if !has_right(rights_of(&guild_ranks, speaker.rank_id), speak_right) {
        return Err(ChatRefusal::NoGuildChatRight);
    }
    let recipients = ctx
        .db
        .game_guild_member()
        .by_guild()
        .filter(speaker.guild_id)
        .filter(|candidate| has_right(rights_of(&guild_ranks, candidate.rank_id), listen_right))
        .map(|candidate| candidate.character_guid)
        .collect();
    Ok(ChatAudience {
        recipients,
        ignorable: true,
        channel_name: String::new(),
    })
}

fn rights_of(guild_ranks: &[GuildRank], rank_id: u32) -> u32 {
    guild_ranks
        .iter()
        .find(|rank| rank.rank_id == rank_id)
        .map_or(0, |rank| rank.rights)
}
