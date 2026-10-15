use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::Arc;

use spacetimedb_sdk::{Table, TableWithPrimaryKey};

use super::bindings::{EventContext, Reducer, RemoteTables};
use super::world_index::ShardId;
use super::world_view::{GroupEventSource, WorldView};
use super::Coordinator;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Replay {
    Snapshot,
    Live,
}

impl Replay {
    fn admits(self, event: &spacetimedb_sdk::Event<Reducer>) -> bool {
        self == Self::Snapshot || !matches!(event, spacetimedb_sdk::Event::SubscribeApplied)
    }
}

#[derive(Clone)]
pub(crate) struct RelayContext {
    pub(crate) view: Arc<WorldView>,
    pub(crate) coord: Coordinator,
    pub(crate) realm: Coordinator,
    pub(crate) shard: ShardId,
    pub(crate) source: GroupEventSource,
}

pub(crate) struct RelayTable {
    #[cfg(test)]
    pub(crate) table: &'static str,
    pub(crate) query: &'static str,
    #[cfg(test)]
    pub(crate) realm: bool,
    #[cfg(test)]
    pub(crate) legs: &'static [(&'static str, Replay)],
}

macro_rules! declare_relays {
    ($($table:ident [$realm:literal] {
        $($leg:ident($replay:ident) => $callback:expr;)*
    })*) => {
        pub(crate) const RELAYS: &[crate::stdb::relays::RelayTable] = &[
            $(crate::stdb::relays::RelayTable {
                #[cfg(test)]
                table: stringify!($table),
                query: concat!("SELECT * FROM ", stringify!($table)),
                #[cfg(test)]
                realm: $realm,
                #[cfg(test)]
                legs: &[$((stringify!($leg), crate::stdb::relays::Replay::$replay)),*],
            }),*
        ];

        pub(crate) fn register_relays(
            context: &crate::stdb::relays::RelayContext,
            db: &crate::stdb::bindings::RemoteTables,
            realm_only: bool,
        ) {
            $(if !realm_only || $realm {
                $(crate::stdb::relays::$leg(
                    db.$table(),
                    concat!(stringify!($table), ".", stringify!($leg)),
                    context.clone(),
                    crate::stdb::relays::Replay::$replay,
                    $callback,
                );)*
            })*
        }
    };
}
pub(crate) use declare_relays;

fn guarded(label: &str, body: impl FnOnce()) {
    if catch_unwind(AssertUnwindSafe(body)).is_err() {
        log::error!(
            "Relay `{label}` panicked; this delta was dropped and the Shard pump continues"
        );
    }
}

pub(crate) fn insert<T: Table<EventContext = EventContext>>(
    table: T,
    label: &'static str,
    context: RelayContext,
    replay: Replay,
    mut callback: impl FnMut(&RelayContext, &EventContext, &T::Row) + Send + 'static,
) {
    table.on_insert(move |event, row| {
        if replay.admits(&event.event) {
            guarded(label, || callback(&context, event, row));
        }
    });
}

pub(crate) fn update<T: TableWithPrimaryKey<EventContext = EventContext>>(
    table: T,
    label: &'static str,
    context: RelayContext,
    _replay: Replay,
    mut callback: impl FnMut(&RelayContext, &EventContext, &T::Row, &T::Row) + Send + 'static,
) {
    table.on_update(move |event, old, row| {
        guarded(label, || callback(&context, event, old, row));
    });
}

pub(crate) fn delete<T: Table<EventContext = EventContext>>(
    table: T,
    label: &'static str,
    context: RelayContext,
    _replay: Replay,
    mut callback: impl FnMut(&RelayContext, &EventContext, &T::Row) + Send + 'static,
) {
    table.on_delete(move |event, row| {
        guarded(label, || callback(&context, event, row));
    });
}

macro_rules! protocol_relays {
    ($($family:ident),* $(,)?) => {
        pub(crate) fn tables() -> impl Iterator<Item = &'static RelayTable> {
            [$(crate::codec::$family::subscriptions::RELAYS),*].into_iter().flatten()
        }

        fn register(context: RelayContext, db: &RemoteTables, realm_only: bool) {
            $(crate::codec::$family::subscriptions::register_relays(&context, db, realm_only);)*
        }
    };
}

protocol_relays!(
    addon, auction, breath, channel, combat, corpse, duel, entity, gameobject, guild, item,
    leveling, mail, movement, pet, quest, social, taxi, trainer, weather,
);

pub(crate) fn queries() -> Vec<&'static str> {
    tables().map(|table| table.query).collect()
}

pub(crate) fn register_shard(
    view: Arc<WorldView>,
    coord: Coordinator,
    shard: ShardId,
    db: &RemoteTables,
) {
    register(
        RelayContext {
            view,
            realm: coord.clone(),
            coord,
            shard,
            source: GroupEventSource::Shard(shard),
        },
        db,
        false,
    );
}

pub(crate) fn register_realm(
    view: Arc<WorldView>,
    realm: Coordinator,
    coord: Coordinator,
    db: &RemoteTables,
) {
    register(
        RelayContext {
            view,
            coord,
            realm,
            shard: 0,
            source: GroupEventSource::RealmCore,
        },
        db,
        true,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    #[test]
    fn declarations_subscribe_each_registered_table_once() {
        let tables: Vec<_> = tables().collect();
        let unique: BTreeSet<_> = tables.iter().map(|table| table.table).collect();
        assert!(!tables.is_empty());
        assert_eq!(unique.len(), tables.len());
        for table in tables {
            assert_eq!(table.query, format!("SELECT * FROM {}", table.table));
            let legs: BTreeSet<_> = table.legs.iter().map(|(leg, _)| *leg).collect();
            assert_eq!(
                legs.len(),
                table.legs.len(),
                "duplicate callback on {}",
                table.table
            );
            assert!(!legs.is_empty());
        }
    }

    #[test]
    fn realm_core_registers_only_the_private_nine_table_subset() {
        let realm: BTreeSet<_> = tables()
            .filter(|table| table.realm)
            .map(|table| table.table)
            .collect();
        assert_eq!(
            realm,
            BTreeSet::from([
                "game_account_claim",
                "game_auction_notice",
                "game_chat_channel_notice_event",
                "game_group_event",
                "game_guild_event",
                "game_guild_member",
                "game_guild_petition",
                "game_mail_arrival",
                "game_realm_chat_event",
            ])
        );
    }

    #[test]
    fn initial_apply_drops_only_the_live_insert_legs() {
        let event = spacetimedb_sdk::Event::SubscribeApplied;
        let suppressed: BTreeSet<_> = tables()
            .filter(|table| {
                table
                    .legs
                    .iter()
                    .any(|(leg, replay)| *leg == "insert" && !replay.admits(&event))
            })
            .map(|table| table.table)
            .collect();
        assert_eq!(
            suppressed,
            BTreeSet::from([
                "game_addon_message",
                "game_auction_notice",
                "game_character_quest_event_credit",
                "game_chat_channel_notice_event",
                "game_duel_event",
                "game_group_event",
                "game_guild_event",
                "game_item_instance",
                "game_levelup_event",
                "game_mail_arrival",
                "game_player_reputation",
                "game_realm_chat_event",
                "game_resurrect_request",
                "game_system_message_event",
                "game_teleport_event",
                "game_trade_event",
                "game_xp_event",
            ])
        );
        assert!(Replay::Snapshot.admits(&event));
    }

    #[test]
    fn callback_panic_does_not_escape_the_shard_pump() {
        let mut next_callback_ran = false;
        guarded("test", || panic!("bad callback"));
        guarded("next", || next_callback_ran = true);
        assert!(next_callback_ran);
    }
}
