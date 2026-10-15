use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::{Arc, RwLock};

use anyhow::{anyhow, Result};
use spacetimedb_sdk::{Table, TableWithPrimaryKey};

use super::super::bindings::*;
use super::super::connection::Coordinator;
use super::super::views::{AccountRow, RealmRow};
use crate::realm_core::SessionKey;

impl Coordinator {
    pub fn realm(&self) -> Result<RealmRow> {
        self.0
            .coord()
            .conn
            .db
            .game_realm()
            .iter()
            .next()
            .map(|r| RealmRow {
                id: r.id,
                name: r.name,
                address: r.address,
                realm_type: r.realm_type,
                flags: r.flags,
                population: r.population,
                timezone: r.timezone,
            })
            .ok_or_else(|| anyhow!("no game_realm row in the coordinator cache"))
    }

    pub fn account_by_username(&self, username: &str) -> Result<Option<AccountRow>> {
        Ok(self
            .0
            .coord()
            .conn
            .db
            .game_account()
            .username()
            .find(&username.to_string())
            .map(|a| AccountRow {
                id: a.id,
                username: a.username,
                salt: a.salt,
                verifier: a.verifier,
                banned: a.banned,
                alpha_test_tools: a.alpha_test_tools,
            }))
    }

    pub fn character_count(&self, account_id: u64) -> Result<u8> {
        let n = self
            .0
            .coord()
            .conn
            .db
            .game_character()
            .iter()
            .filter(|c| c.account_id == account_id)
            .count();
        Ok(n.min(u8::MAX as usize) as u8)
    }

    pub fn character_location(&self, guid: u64) -> Option<(u32, u64)> {
        let guard = self.0.coord();
        let db = &guard.conn.db;
        if let Some(e) = db.game_world_entity().guid().find(&guid) {
            return Some((e.map_id, e.instance_id));
        }
        db.game_character()
            .guid()
            .find(&guid)
            .map(|c| (c.map_id, c.pending_instance_id))
    }

    pub(crate) fn character_row(&self, guid: u64) -> Option<super::super::bindings::Character> {
        self.0.coord().conn.db.game_character().guid().find(&guid)
    }

    pub(crate) fn escrow_row(&self, guid: u64) -> Option<super::super::bindings::TransferOut> {
        use spacetimedb_sdk::Table as _;
        self.0
            .coord()
            .conn
            .db
            .game_transfer_out()
            .iter()
            .find(|r| r.character_guid == guid)
    }

    pub(crate) fn realm_character_partition(
        &self,
        guid: u64,
    ) -> Result<Option<crate::world::party::RealmCharacterPartition>> {
        let live = self.0.coord();
        if !live.is_healthy() {
            anyhow::bail!(
                "{} has no healthy Coordinator subscription for the Realm locator",
                self.shard_name()
            );
        }
        Ok(live
            .conn
            .db
            .game_character_shard()
            .character_guid()
            .find(&guid)
            .map(|row| crate::world::party::RealmCharacterPartition {
                map_id: row.map_id,
                instance_id: row.instance_id,
                revision: row.revision,
                transfer_pending: row.transfer_pending,
                pending_destination_map: row.pending_destination_map,
                pending_destination_instance: row.pending_destination_instance,
                bot_source_identity: row.bot_source_identity,
                bot_transfer_intent_id: row.bot_transfer_intent_id,
                bot_controller_generation: row.bot_controller_generation,
            }))
    }

    pub fn bound_identity(&self, account_id: u64) -> Result<[u8; 32]> {
        Ok(crate::config::synthetic_owner_identity(account_id))
    }

    pub fn session_key(&self, account_id: u64) -> Result<Option<SessionKey>> {
        Ok(self
            .0
            .coord()
            .conn
            .db
            .game_session()
            .account_id()
            .find(&account_id)
            .and_then(|s| {
                Some(SessionKey {
                    key: <[u8; 40]>::try_from(s.session_key).ok()?,
                    expires_at_micros: s.expires_at.to_micros_since_unix_epoch(),
                })
            }))
    }
}

#[derive(Default)]
pub(crate) struct ContactIndex {
    by_owner: HashMap<u64, BTreeMap<u64, (u64, bool)>>,
}

impl ContactIndex {
    fn insert(&mut self, row: &ContactEntry) {
        self.by_owner
            .entry(row.owner_guid)
            .or_default()
            .insert(row.id, (row.target_guid, row.is_ignore));
    }

    fn remove(&mut self, row: &ContactEntry) {
        if let Some(rows) = self.by_owner.get_mut(&row.owner_guid) {
            rows.remove(&row.id);
            if rows.is_empty() {
                self.by_owner.remove(&row.owner_guid);
            }
        }
    }

    fn targets(&self, owner_guid: u64, ignore: bool) -> impl Iterator<Item = u64> + '_ {
        self.by_owner
            .get(&owner_guid)
            .into_iter()
            .flat_map(|rows| rows.values())
            .filter(move |(_, is_ignore)| *is_ignore == ignore)
            .map(|(target_guid, _)| *target_guid)
    }

    pub(crate) fn ignored_by(&self, owner_guid: u64) -> Vec<u64> {
        self.targets(owner_guid, true).collect()
    }

    pub(crate) fn friends_of(&self, owner_guid: u64) -> Vec<u64> {
        self.targets(owner_guid, false).collect()
    }
}

#[derive(Default)]
pub(crate) struct CharacterNameIndex {
    by_name: HashMap<String, BTreeSet<u64>>,
}

impl CharacterNameIndex {
    fn insert(&mut self, guid: u64, name: &str) {
        self.by_name
            .entry(name.to_ascii_lowercase())
            .or_default()
            .insert(guid);
    }

    fn remove(&mut self, guid: u64, name: &str) {
        let key = name.to_ascii_lowercase();
        if let Some(guids) = self.by_name.get_mut(&key) {
            guids.remove(&guid);
            if guids.is_empty() {
                self.by_name.remove(&key);
            }
        }
    }

    pub(crate) fn guid_named(&self, name: &str) -> Option<u64> {
        self.by_name
            .get(&name.to_ascii_lowercase())
            .and_then(|guids| guids.first().copied())
    }
}

pub(crate) fn watch_character_names(conn: &DbConnection) -> Arc<RwLock<CharacterNameIndex>> {
    let index = Arc::new(RwLock::new(CharacterNameIndex::default()));
    let inserted = index.clone();
    conn.db.game_character().on_insert(move |_ctx, row| {
        let mut names = inserted.write().unwrap_or_else(|p| p.into_inner());
        names.insert(row.guid, &row.name);
    });
    let updated = index.clone();
    conn.db.game_character().on_update(move |_ctx, old, new| {
        let mut names = updated.write().unwrap_or_else(|p| p.into_inner());
        names.remove(old.guid, &old.name);
        names.insert(new.guid, &new.name);
    });
    let deleted = index.clone();
    conn.db.game_character().on_delete(move |_ctx, row| {
        let mut names = deleted.write().unwrap_or_else(|p| p.into_inner());
        names.remove(row.guid, &row.name);
    });
    index
}

pub(crate) fn watch_contacts(conn: &DbConnection) -> Arc<RwLock<ContactIndex>> {
    let index = Arc::new(RwLock::new(ContactIndex::default()));
    let inserted = index.clone();
    conn.db
        .game_character_contact()
        .on_insert(move |_ctx, row| {
            inserted
                .write()
                .unwrap_or_else(|p| p.into_inner())
                .insert(row);
        });
    let updated = index.clone();
    conn.db
        .game_character_contact()
        .on_update(move |_ctx, old, new| {
            let mut contacts = updated.write().unwrap_or_else(|p| p.into_inner());
            contacts.remove(old);
            contacts.insert(new);
        });
    let deleted = index.clone();
    conn.db
        .game_character_contact()
        .on_delete(move |_ctx, row| {
            deleted
                .write()
                .unwrap_or_else(|p| p.into_inner())
                .remove(row);
        });
    index
}

#[cfg(test)]
mod tests {
    use super::*;

    fn contact(id: u64, owner_guid: u64, target_guid: u64, is_ignore: bool) -> ContactEntry {
        ContactEntry {
            id,
            owner_guid,
            owner_identity: spacetimedb_sdk::Identity::ZERO,
            target_guid,
            is_ignore,
        }
    }

    #[test]
    fn the_contact_index_answers_each_owners_own_lists() {
        let mut index = ContactIndex::default();
        index.insert(&contact(1, 10, 20, true));
        index.insert(&contact(2, 10, 30, false));
        index.insert(&contact(3, 11, 40, true));
        assert_eq!(index.ignored_by(10), vec![20]);
        assert_eq!(index.friends_of(10), vec![30]);
        assert_eq!(index.ignored_by(11), vec![40]);
        assert!(index.friends_of(11).is_empty());
        assert!(index.ignored_by(12).is_empty());
    }

    #[test]
    fn a_typed_name_finds_its_character_in_any_case() {
        let mut index = CharacterNameIndex::default();
        index.insert(7, "Thrall");
        assert_eq!(index.guid_named("thrall"), Some(7));
        assert_eq!(index.guid_named("THRALL"), Some(7));
        assert_eq!(index.guid_named("Thral"), None);
    }

    #[test]
    fn a_renamed_or_deleted_character_leaves_the_name_index() {
        let mut index = CharacterNameIndex::default();
        index.insert(7, "Thrall");
        index.insert(8, "thrall");
        assert_eq!(
            index.guid_named("Thrall"),
            Some(7),
            "the lower guid, every time"
        );
        index.remove(7, "Thrall");
        assert_eq!(index.guid_named("Thrall"), Some(8));
        index.remove(8, "thrall");
        assert_eq!(
            index.guid_named("Thrall"),
            None,
            "no Character named it now"
        );
    }

    #[test]
    fn a_renamed_character_answers_only_to_its_new_name() {
        let mut index = CharacterNameIndex::default();
        index.insert(7, "Thrall");
        index.remove(7, "Thrall");
        index.insert(7, "Go'el");
        assert_eq!(index.guid_named("Thrall"), None, "the old name is gone");
        assert_eq!(index.guid_named("go'el"), Some(7), "the new name resolves");
    }

    #[test]
    fn a_deleted_contact_row_leaves_the_index() {
        let mut index = ContactIndex::default();
        index.insert(&contact(1, 10, 20, true));
        index.insert(&contact(2, 10, 21, true));
        index.remove(&contact(1, 10, 20, true));
        assert_eq!(index.ignored_by(10), vec![21]);
        index.remove(&contact(2, 10, 21, true));
        assert!(
            index.ignored_by(10).is_empty(),
            "an owner with no rows left ignores nobody"
        );
    }
}
