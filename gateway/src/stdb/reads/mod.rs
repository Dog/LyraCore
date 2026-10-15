mod account;
mod channel;
mod guild;
mod items;
mod mail;
mod party;
mod quest;
mod spell;
mod talent_reputation;
mod templates;

use spacetimedb_sdk::Table;

use super::bindings::*;

pub(crate) use quest::build_quest_log_slots;

pub(crate) use account::{watch_character_names, watch_contacts, CharacterNameIndex, ContactIndex};
pub(crate) use channel::ChannelIndex;
pub(crate) use guild::{watch_guilds, GuildIndex};
pub(crate) use items::{property_enchant_ids, view_of_item_row};
pub(crate) use mail::{watch_mail_escrows, MailEscrowIndex};

pub(in crate::stdb) use guild::petition_view;
pub(in crate::stdb) use quest::quest_objectives_complete;
pub(in crate::stdb) use spell::spell_ranks_stack_in_book;

pub(in crate::stdb) fn player_item_count(db: &RemoteTables, owner_guid: u64, entry: u32) -> u32 {
    lyracore_shared::item::carried_quantity(
        entry,
        db.game_item_instance()
            .iter()
            .filter(|item| item.owner_guid == owner_guid)
            .map(|item| (item.slot, item.entry, item.stack_count)),
    )
}
