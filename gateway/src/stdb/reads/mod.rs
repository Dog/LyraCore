mod account;
mod channel;
mod combat_index;
mod guild;
mod inventory_index;
mod items;
mod loot_tag_index;
mod mail;
mod party;
mod quest;
mod quest_index;
mod spell;
mod talent_reputation;
mod templates;

pub(crate) use quest::build_quest_log_slots;

pub(crate) use account::{watch_character_names, watch_contacts, CharacterNameIndex, ContactIndex};
pub(crate) use channel::ChannelIndex;
pub(crate) use combat_index::{watch_combat, CombatIndex};
pub(crate) use guild::{watch_guilds, GuildIndex};
pub(crate) use inventory_index::{watch_inventory, InventoryIndex};
pub(crate) use items::{property_enchant_ids, view_of_item_row};
pub(crate) use loot_tag_index::{watch_loot_tag, LootTagIndex};
pub(crate) use mail::{watch_mail_escrows, MailEscrowIndex};
pub(crate) use quest_index::{watch_quest, QuestIndex};

pub(in crate::stdb) use guild::petition_view;
pub(in crate::stdb) use quest::quest_objectives_complete;
pub(in crate::stdb) use spell::spell_ranks_stack_in_book;

pub(in crate::stdb) fn player_item_count(
    live: &super::connection::LiveConn,
    owner_guid: u64,
    entry: u32,
) -> u32 {
    lyracore_shared::item::carried_quantity(
        entry,
        live.item_rows(owner_guid)
            .into_iter()
            .map(|item| (item.slot, item.entry, item.stack_count)),
    )
}
