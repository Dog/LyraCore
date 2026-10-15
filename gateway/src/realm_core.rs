use anyhow::{anyhow, Result};

use crate::config::ShardMap;
use crate::stdb::{AccountRow, RealmRow};
use crate::world::{Actor, WorldSession};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct SessionKey {
    pub key: [u8; 40],
    pub expires_at_micros: i64,
}

pub(crate) fn session_expired(now_micros: i64, expires_at_micros: i64) -> bool {
    expires_at_micros <= now_micros
}

fn now_micros() -> i64 {
    let micros = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_micros();
    i64::try_from(micros).unwrap_or(i64::MAX)
}

pub(crate) trait RealmDb: Clone + Sized + Send + Sync {
    fn shard_name(&self) -> &str;
    fn is_sharded(&self) -> bool;
    fn shard_map(&self) -> &ShardMap;
    fn realm_core(&self) -> Result<Self>;
    fn world_shards(&self) -> Vec<(String, Self)>;

    fn account_by_username(&self, username: &str) -> Result<Option<AccountRow>>;
    fn session_key(&self, account_id: u64) -> Result<Option<SessionKey>>;
    fn bound_identity(&self, account_id: u64) -> Result<[u8; 32]>;
    fn character_count(&self, account_id: u64) -> Result<u8>;
    fn realm(&self) -> Result<RealmRow>;

    fn establish_session(
        &self,
        account_id: u64,
        session_key: &[u8; 40],
        bound_identity: [u8; 32],
    ) -> Result<()>;

    fn request_gm_command(&self, actor: Actor, alpha_test_tools: bool, text: String) -> Result<()>;

    fn character_location(&self, guid: u64) -> Option<(u32, u64)>;
    fn set_character_shard(&self, guid: u64, map_id: u32, instance_id: u64) -> Result<()>;
    fn realm_character_partition(
        &self,
        guid: u64,
    ) -> Result<Option<crate::world::party::RealmCharacterPartition>>;
    #[allow(clippy::too_many_arguments)]
    fn begin_character_shard_transfer(
        &self,
        source_map: u32,
        source_instance: u64,
        source_revision: u64,
        destination_map: u32,
        destination_instance: u64,
        source_module_identity: spacetimedb_sdk::Identity,
        intent_id: u64,
        controller_generation: u64,
        character_guid: u64,
    ) -> Result<()>;
    fn finish_character_shard_transfer(
        &self,
        intent: &crate::world::transfer::BotTransferIntent,
    ) -> Result<()>;
    fn finish_player_character_shard_transfer(
        &self,
        character_guid: u64,
        source_map: u32,
        source_instance: u64,
        source_revision: u64,
        destination_map: u32,
        destination_instance: u64,
    ) -> Result<()>;
    #[allow(clippy::too_many_arguments)]
    fn finish_pending_character_shard_transfer(
        &self,
        character_guid: u64,
        source_map: u32,
        source_instance: u64,
        source_revision: u64,
        destination_map: u32,
        destination_instance: u64,
        source_module_identity: spacetimedb_sdk::Identity,
        transfer_intent_id: u64,
        controller_generation: u64,
    ) -> Result<()>;
    fn has_escrow(&self, guid: u64) -> bool;

    fn session_count(&self) -> usize;
    fn record_shard_load(
        &self,
        shard: &str,
        writer_occupancy_pct: f32,
        sessions: u32,
        gateway_key: u64,
    ) -> Result<()>;
}

fn unknown_account(account_name: &str) -> anyhow::Error {
    crate::stdb::ReducerCallError::Rejected {
        operation: "gm_command".to_string(),
        reason: format!("no Account named {account_name} on Realm-core"),
    }
    .into()
}

pub(crate) fn run_gm_command<D: RealmDb>(
    home_shard: &D,
    account_name: &str,
    actor: Actor,
    text: String,
) -> Result<()> {
    let account_name = account_name.to_ascii_uppercase();
    let realm_core = home_shard.realm_core()?;
    let authority = realm_core
        .account_by_username(&account_name)?
        .ok_or_else(|| unknown_account(&account_name))?
        .alpha_test_tools;
    home_shard.request_gm_command(actor, authority, text)
}

pub(crate) fn lookup_session<D: RealmDb>(
    db: &D,
    account_name: &str,
) -> Result<Option<WorldSession>> {
    let realm_core = db.realm_core()?;
    let Some(authoritative) = realm_core.account_by_username(account_name)? else {
        return Ok(None);
    };
    let Some(session) = realm_core.session_key(authoritative.id)? else {
        return Ok(None);
    };
    if session_expired(now_micros(), session.expires_at_micros) {
        log::info!("account {account_name}: session expired; refusing the world handshake");
        return Ok(None);
    }
    let Some(local) = db.account_by_username(account_name)? else {
        log::error!(
            "account {account_name} authenticated on realm-core but has no account row on world \
             shard {} — provision it there (accounts are keyed across databases by USERNAME; ids \
             are per-database)",
            db.shard_name()
        );
        return Ok(None);
    };
    Ok(Some(WorldSession {
        account_id: local.id,
        session_key: session.key,
    }))
}

pub(crate) fn publish_bot_shard_index<D: RealmDb>(
    db: &D,
    intent: &crate::world::transfer::BotTransferIntent,
) -> Result<()> {
    let realm = db.realm_core()?;
    realm.finish_character_shard_transfer(intent)?;
    wait_for_settled_locator(
        &realm,
        intent.bot_guid,
        intent.destination_map,
        intent.destination_instance,
        intent.source_locator_revision.saturating_add(1),
        Some((
            intent.source_module_identity,
            intent.id,
            intent.controller_generation,
        )),
    )
}

pub(crate) fn begin_shard_index_transfer<D: RealmDb>(
    db: &D,
    plan: &crate::world::transfer::TransferPlan,
    bot_intent: Option<(&crate::world::transfer::BotTransferIntent, u64)>,
) -> Result<crate::world::party::RealmCharacterPartition> {
    let realm = db.realm_core()?;
    let locator = realm.realm_character_partition(plan.character_guid)?;
    let (source_map, source_instance, source_revision, source_identity, intent_id, generation) =
        match (locator, bot_intent) {
            (Some(locator), Some((intent, _claim_token))) => {
                if intent.source_locator_revision == 0 {
                    anyhow::bail!("bot Transfer has no bound Realm locator predecessor");
                }
                let crossing = (
                    intent.source_module_identity,
                    intent.id,
                    intent.controller_generation,
                );
                if !locator.transfer_pending
                    && (locator.map_id, locator.instance_id, locator.revision)
                        == (
                            intent.destination_map,
                            intent.destination_instance,
                            intent.source_locator_revision.saturating_add(1),
                        )
                    && (
                        locator.bot_source_identity,
                        locator.bot_transfer_intent_id,
                        locator.bot_controller_generation,
                    ) == crossing
                {
                    return Ok(locator);
                }
                if (locator.map_id, locator.instance_id)
                    != (intent.source_map, intent.source_instance)
                    || locator.revision != intent.source_locator_revision
                {
                    anyhow::bail!(
                    "bot Transfer source locator is map {} instance {} revision {}, not intent {} source map {} instance {} revision {}",
                    locator.map_id,
                    locator.instance_id,
                    locator.revision,
                    intent.id,
                    intent.source_map,
                    intent.source_instance,
                    intent.source_locator_revision
                );
                }
                (
                    intent.source_map,
                    intent.source_instance,
                    intent.source_locator_revision,
                    intent.source_module_identity,
                    intent.id,
                    intent.controller_generation,
                )
            }
            (Some(locator), None) => (
                locator.map_id,
                locator.instance_id,
                locator.revision,
                spacetimedb_sdk::Identity::ZERO,
                0,
                0,
            ),
            (None, None) => {
                let (map_id, instance_id) = db
                    .character_location(plan.character_guid)
                    .ok_or_else(|| anyhow!("Transfer source has no Character"))?;
                (
                    map_id,
                    instance_id,
                    0,
                    spacetimedb_sdk::Identity::ZERO,
                    0,
                    0,
                )
            }
            (None, Some(_)) => anyhow::bail!("Transfer source has no Realm locator"),
        };
    realm.begin_character_shard_transfer(
        source_map,
        source_instance,
        source_revision,
        plan.dest_map_id,
        plan.dest_instance_id,
        source_identity,
        intent_id,
        generation,
        plan.character_guid,
    )?;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    loop {
        if let Some(observed) = realm
            .realm_character_partition(plan.character_guid)?
            .filter(|row| {
                row.transfer_pending
                    && (row.map_id, row.instance_id, row.revision)
                        == (source_map, source_instance, source_revision.max(1))
                    && (
                        row.pending_destination_map,
                        row.pending_destination_instance,
                    ) == (plan.dest_map_id, plan.dest_instance_id)
                    && (
                        row.bot_source_identity,
                        row.bot_transfer_intent_id,
                        row.bot_controller_generation,
                    ) == (source_identity, intent_id, generation)
            })
        {
            return Ok(observed);
        }
        if std::time::Instant::now() >= deadline {
            anyhow::bail!("Realm pending Transfer phase was not observable within 3s");
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

pub(crate) fn finish_player_shard_index_transfer<D: RealmDb>(
    db: &D,
    plan: &crate::world::transfer::TransferPlan,
    source_map: u32,
    source_instance: u64,
    source_revision: u64,
) -> Result<()> {
    let realm = db.realm_core()?;
    realm.finish_player_character_shard_transfer(
        plan.character_guid,
        source_map,
        source_instance,
        source_revision,
        plan.dest_map_id,
        plan.dest_instance_id,
    )?;
    wait_for_settled_locator(
        &realm,
        plan.character_guid,
        plan.dest_map_id,
        plan.dest_instance_id,
        source_revision.saturating_add(1),
        Some((spacetimedb_sdk::Identity::ZERO, 0, 0)),
    )
}

pub(crate) fn finish_pending_shard_index_transfer<D: RealmDb>(
    db: &D,
    character_guid: u64,
    destination_map: u32,
    destination_instance: u64,
    arrival: &crate::world::transfer::TransferArrival,
) -> Result<()> {
    let realm = db.realm_core()?;
    if arrival.character_guid != character_guid {
        anyhow::bail!("arrival fence names another Character");
    }
    if arrival.source_locator_revision == 0 {
        anyhow::bail!("arrival fence has no Realm locator predecessor");
    }
    let phase = realm
        .realm_character_partition(character_guid)?
        .ok_or_else(|| anyhow!("Transfer destination has no Realm locator"))?;
    let crossing = (
        arrival.bot_source_identity,
        arrival.bot_transfer_intent_id,
        arrival.bot_controller_generation,
    );
    if !phase.transfer_pending
        && (phase.map_id, phase.instance_id, phase.revision)
            == (
                destination_map,
                destination_instance,
                arrival.source_locator_revision.saturating_add(1),
            )
        && (
            phase.bot_source_identity,
            phase.bot_transfer_intent_id,
            phase.bot_controller_generation,
        ) == crossing
    {
        return Ok(());
    }
    if !phase.transfer_pending
        || (phase.map_id, phase.instance_id, phase.revision)
            != (
                arrival.source_map,
                arrival.source_instance,
                arrival.source_locator_revision,
            )
        || (
            phase.pending_destination_map,
            phase.pending_destination_instance,
        ) != (destination_map, destination_instance)
        || (
            phase.bot_source_identity,
            phase.bot_transfer_intent_id,
            phase.bot_controller_generation,
        ) != crossing
    {
        anyhow::bail!("destination fence does not match the pending Realm Transfer");
    }
    realm.finish_pending_character_shard_transfer(
        character_guid,
        arrival.source_map,
        arrival.source_instance,
        arrival.source_locator_revision,
        destination_map,
        destination_instance,
        arrival.bot_source_identity,
        arrival.bot_transfer_intent_id,
        arrival.bot_controller_generation,
    )?;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    loop {
        let settled = realm
            .realm_character_partition(character_guid)?
            .is_some_and(|row| {
                !row.transfer_pending
                    && (row.map_id, row.instance_id, row.revision)
                        == (
                            destination_map,
                            destination_instance,
                            arrival.source_locator_revision.saturating_add(1),
                        )
                    && (
                        row.bot_source_identity,
                        row.bot_transfer_intent_id,
                        row.bot_controller_generation,
                    ) == crossing
            });
        if settled {
            return Ok(());
        }
        if std::time::Instant::now() >= deadline {
            anyhow::bail!("settled Realm Transfer was not observable within 3s");
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

fn wait_for_settled_locator<D: RealmDb>(
    realm: &D,
    character_guid: u64,
    destination_map: u32,
    destination_instance: u64,
    revision: u64,
    crossing: Option<(spacetimedb_sdk::Identity, u64, u64)>,
) -> Result<()> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    loop {
        let observed = realm
            .realm_character_partition(character_guid)?
            .ok_or_else(|| anyhow!("settled Realm locator disappeared"))?;
        let observed_crossing = (
            observed.bot_source_identity,
            observed.bot_transfer_intent_id,
            observed.bot_controller_generation,
        );
        if !observed.transfer_pending
            && (observed.map_id, observed.instance_id, observed.revision)
                == (destination_map, destination_instance, revision)
            && crossing.is_none_or(|expected| expected == observed_crossing)
        {
            return Ok(());
        }
        if std::time::Instant::now() >= deadline {
            anyhow::bail!("settled Realm Transfer was not observable within 3s");
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

pub(crate) fn locate_home_shard<D: RealmDb>(db: &D, guid: u64) -> Option<D> {
    if !db.is_sharded() {
        return None;
    }
    let index = db.realm_core().ok();
    let shards = db.world_shards();
    let is_connected = |d: &str| shards.iter().any(|(n, _)| n == d);

    // Source Escrow remains authoritative even after a pending locator names the destination.
    if let Some(rc) = &index {
        if let Some(hint) = rc
            .realm_character_partition(guid)
            .ok()
            .flatten()
            .filter(|row| !row.transfer_pending)
        {
            let hinted_name =
                db.shard_map()
                    .resolve_connected(hint.map_id, hint.instance_id, is_connected);
            if let Some((_, hinted)) = shards.iter().find(|(n, _)| n == hinted_name) {
                if hinted.has_escrow(guid) || hinted.character_location(guid).is_some() {
                    heal_shard_entry(rc, guid, hinted);
                    return Some(hinted.clone());
                }
            }
        }
    }

    let mut agreeing: Option<&D> = None;
    let mut disagreeing: Option<&D> = None;
    for (_, shard) in &shards {
        if shard.has_escrow(guid) {
            if let Some(rc) = &index {
                heal_shard_entry(rc, guid, shard);
            }
            return Some(shard.clone());
        }
        if let Some(loc) = shard.character_location(guid) {
            if shard.shard_map().resolve(loc.0, loc.1) == shard.shard_name() {
                if agreeing.is_none() {
                    agreeing = Some(shard);
                }
            } else if disagreeing.is_none() {
                disagreeing = Some(shard);
            }
        }
    }
    let holder = agreeing.or(disagreeing);
    if let (Some(h), Some(rc)) = (holder, &index) {
        heal_shard_entry(rc, guid, h);
    }
    holder.cloned()
}

fn heal_shard_entry<D: RealmDb>(rc: &D, guid: u64, shard: &D) {
    let Some(actual) = shard.character_location(guid) else {
        return;
    };
    // Heal only a confirmed durable location; predicting the destination can strand recovery.
    if shard.shard_map().resolve(actual.0, actual.1) != shard.shard_name() {
        return;
    }
    let Ok(locator) = rc.realm_character_partition(guid) else {
        return;
    };
    if locator.is_some_and(|row| row.transfer_pending || (row.map_id, row.instance_id) == actual) {
        return;
    }
    if let Err(e) = rc.set_character_shard(guid, actual.0, actual.1) {
        log::warn!(
            "character {guid}: could not heal the realm-core shard index to map {}/instance {} \
             ({e:#}) — routing is unaffected, the probe will run again",
            actual.0,
            actual.1
        );
    }
}

pub(crate) fn resolve_delete_shard<D: RealmDb>(db: &D, guid: u64) -> Result<Option<D>> {
    if !db.is_sharded() {
        return Ok(None);
    }
    let Some(owner) = locate_home_shard(db, guid) else {
        return Ok(None);
    };
    if owner.has_escrow(guid) {
        return Err(anyhow::anyhow!(
            "character {guid} is mid-transfer — an escrow holds it on shard {} — refusing to \
             delete until the transfer settles",
            owner.shard_name()
        ));
    }
    Ok(Some(owner))
}

#[cfg(test)]
pub(crate) mod fake {
    use super::*;
    use anyhow::anyhow;
    use std::collections::{HashMap, HashSet};
    use std::sync::{Arc, Mutex};

    pub(crate) const NEVER: i64 = i64::MAX;

    #[derive(Default)]
    pub(crate) struct Db {
        pub accounts: Mutex<HashMap<String, AccountRow>>,
        #[allow(clippy::type_complexity)]
        pub sessions: Mutex<HashMap<u64, ([u8; 40], [u8; 32], i64)>>,
        #[allow(clippy::type_complexity)]
        pub characters: Mutex<HashMap<u64, (u64, (u32, u64))>>,
        pub shard_index: Mutex<HashMap<u64, (u32, u64)>>,
        pub shard_phases: Mutex<HashMap<u64, crate::world::party::RealmCharacterPartition>>,
        pub unobserved_locator_reads: Mutex<usize>,
        pub identities: Mutex<HashMap<u64, [u8; 32]>>,
        pub mints: Mutex<u8>,
        pub escrows: Mutex<HashSet<u64>>,
        pub open_sessions: Mutex<usize>,
        pub recorded_shard_loads: Mutex<Vec<(String, f32, u32, u64)>>,
        pub gm_commands: Mutex<Vec<(u64, bool, String)>>,
        pub log: Mutex<Vec<String>>,
    }

    impl Db {
        fn note(&self, what: &str) {
            self.log.lock().unwrap().push(what.to_string());
        }
        pub fn touched(&self) -> Vec<String> {
            self.log.lock().unwrap().clone()
        }
    }

    pub(crate) struct Realm {
        pub dbs: HashMap<String, Arc<Db>>,
        pub map: ShardMap,
        pub realm_core_up: bool,
    }

    #[derive(Clone)]
    pub(crate) struct Handle {
        pub realm: Arc<Realm>,
        pub db: String,
    }

    impl Handle {
        pub fn at(&self, db: &str) -> Handle {
            Handle {
                realm: self.realm.clone(),
                db: db.to_string(),
            }
        }
        pub fn store(&self) -> &Arc<Db> {
            self.realm
                .dbs
                .get(&self.db)
                .expect("no such database in the fake realm")
        }
        pub fn db_at(&self, name: &str) -> &Arc<Db> {
            self.realm
                .dbs
                .get(name)
                .expect("no such database in the fake realm")
        }

        fn stored_partition(
            &self,
            guid: u64,
        ) -> Option<crate::world::party::RealmCharacterPartition> {
            let phases = self.store().shard_phases.lock().unwrap();
            phases
                .get(&guid)
                .copied()
                .or_else(|| self.indexed_partition(guid))
        }

        fn indexed_partition(
            &self,
            guid: u64,
        ) -> Option<crate::world::party::RealmCharacterPartition> {
            self.store()
                .shard_index
                .lock()
                .unwrap()
                .get(&guid)
                .map(
                    |&(map_id, instance_id)| crate::world::party::RealmCharacterPartition {
                        map_id,
                        instance_id,
                        revision: 1,
                        transfer_pending: false,
                        pending_destination_map: 0,
                        pending_destination_instance: 0,
                        bot_source_identity: spacetimedb_sdk::Identity::ZERO,
                        bot_transfer_intent_id: 0,
                        bot_controller_generation: 0,
                    },
                )
        }

        #[allow(clippy::too_many_arguments)]
        fn settle_partition(
            &self,
            guid: u64,
            source_map: u32,
            source_instance: u64,
            source_revision: u64,
            destination_map: u32,
            destination_instance: u64,
            source_identity: spacetimedb_sdk::Identity,
            intent_id: u64,
            generation: u64,
        ) -> Result<()> {
            if source_revision == 0
                || (source_identity == spacetimedb_sdk::Identity::ZERO) != (intent_id == 0)
            {
                return Err(anyhow!("Transfer locator identity is incomplete"));
            }
            let db = self.store();
            let mut phases = db.shard_phases.lock().unwrap();
            let current = phases
                .get(&guid)
                .copied()
                .or_else(|| self.indexed_partition(guid))
                .ok_or_else(|| anyhow!("Transfer source has no Realm locator"))?;
            let crossing = (source_identity, intent_id, generation);
            if !current.transfer_pending
                && (current.map_id, current.instance_id, current.revision)
                    == (
                        destination_map,
                        destination_instance,
                        source_revision.saturating_add(1),
                    )
                && (
                    current.bot_source_identity,
                    current.bot_transfer_intent_id,
                    current.bot_controller_generation,
                ) == crossing
            {
                return Ok(());
            }
            if !current.transfer_pending
                || (current.map_id, current.instance_id, current.revision)
                    != (source_map, source_instance, source_revision)
                || (
                    current.pending_destination_map,
                    current.pending_destination_instance,
                ) != (destination_map, destination_instance)
                || (
                    current.bot_source_identity,
                    current.bot_transfer_intent_id,
                    current.bot_controller_generation,
                ) != crossing
            {
                return Err(anyhow!("Transfer Realm locator phase changed"));
            }
            let revision = source_revision
                .checked_add(1)
                .ok_or_else(|| anyhow!("Transfer Realm locator revision exhausted"))?;
            let settled = crate::world::party::RealmCharacterPartition {
                map_id: destination_map,
                instance_id: destination_instance,
                revision,
                transfer_pending: false,
                pending_destination_map: 0,
                pending_destination_instance: 0,
                ..current
            };
            phases.insert(guid, settled);
            db.shard_index
                .lock()
                .unwrap()
                .insert(guid, (destination_map, destination_instance));
            Ok(())
        }
    }

    pub(crate) fn realm(dbs: &[&str], rules: &str, realm_core: Option<&str>) -> Handle {
        let map = ShardMap::parse(dbs[0], rules).with_realm_core(realm_core);
        let realm = Realm {
            dbs: dbs
                .iter()
                .map(|d| (d.to_string(), Arc::new(Db::default())))
                .collect(),
            map,
            realm_core_up: true,
        };
        Handle {
            realm: Arc::new(realm),
            db: dbs[0].to_string(),
        }
    }

    pub(crate) fn realm_with_dead_core(dbs: &[&str], rules: &str, realm_core: &str) -> Handle {
        let h = realm(dbs, rules, Some(realm_core));
        let mut realm = Arc::try_unwrap(h.realm).ok().expect("sole owner");
        realm.realm_core_up = false;
        Handle {
            realm: Arc::new(realm),
            db: h.db,
        }
    }

    pub(crate) fn account(id: u64, username: &str, salt: u8, verifier: u8) -> AccountRow {
        AccountRow {
            id,
            username: username.to_string(),
            salt: vec![salt; 32],
            verifier: vec![verifier; 32],
            banned: false,
            alpha_test_tools: false,
        }
    }

    impl RealmDb for Handle {
        fn shard_name(&self) -> &str {
            &self.db
        }
        fn is_sharded(&self) -> bool {
            self.realm.dbs.len() > 1
        }
        fn shard_map(&self) -> &ShardMap {
            &self.realm.map
        }
        fn realm_core(&self) -> Result<Handle> {
            match self.realm.map.realm_core_db() {
                None => Ok(self.clone()),
                Some(db) if self.realm.realm_core_up => Ok(self.at(db)),
                Some(db) => Err(anyhow!(
                    "realm-core database {db} is not connected — refusing to authenticate against \
                     the world database's stale auth cache"
                )),
            }
        }
        fn world_shards(&self) -> Vec<(String, Handle)> {
            self.realm
                .map
                .shards()
                .into_iter()
                .filter(|d| self.realm.dbs.contains_key(d))
                .map(|d| {
                    let h = self.at(&d);
                    (d, h)
                })
                .collect()
        }
        fn account_by_username(&self, username: &str) -> Result<Option<AccountRow>> {
            let db = self.store();
            db.note(&format!("account_by_username({username})"));
            Ok(db.accounts.lock().unwrap().get(username).cloned())
        }
        fn session_key(&self, account_id: u64) -> Result<Option<SessionKey>> {
            let db = self.store();
            db.note(&format!("session_key({account_id})"));
            Ok(db
                .sessions
                .lock()
                .unwrap()
                .get(&account_id)
                .map(|(key, _, expires_at_micros)| SessionKey {
                    key: *key,
                    expires_at_micros: *expires_at_micros,
                }))
        }
        fn bound_identity(&self, account_id: u64) -> Result<[u8; 32]> {
            let db = self.store();
            db.note(&format!("bound_identity({account_id})"));
            let mut ids = db.identities.lock().unwrap();
            let n = ids.len();
            let mut mints = db.mints.lock().unwrap();
            Ok(*ids.entry(account_id).or_insert_with(|| {
                *mints += 1;
                let mut id = [0u8; 32];
                id[0] = account_id as u8;
                id[1] = n as u8 + 1;
                id[2] = self.db.len() as u8;
                id[3] = *mints;
                id
            }))
        }
        fn character_count(&self, account_id: u64) -> Result<u8> {
            let db = self.store();
            db.note(&format!("character_count({account_id})"));
            Ok(db
                .characters
                .lock()
                .unwrap()
                .values()
                .filter(|(a, _)| *a == account_id)
                .count() as u8)
        }
        fn realm(&self) -> Result<RealmRow> {
            self.store().note("realm()");
            Ok(RealmRow {
                id: 1,
                name: "Test".into(),
                address: "127.0.0.1:8085".into(),
                realm_type: 0,
                flags: 0,
                population: 0.0,
                timezone: 1,
            })
        }
        fn establish_session(
            &self,
            account_id: u64,
            session_key: &[u8; 40],
            bound_identity: [u8; 32],
        ) -> Result<()> {
            let db = self.store();
            db.note(&format!("establish_session({account_id})"));
            db.sessions
                .lock()
                .unwrap()
                .insert(account_id, (*session_key, bound_identity, NEVER));
            Ok(())
        }
        fn request_gm_command(
            &self,
            actor: Actor,
            alpha_test_tools: bool,
            text: String,
        ) -> Result<()> {
            let db = self.store();
            db.note(&format!("request_gm_command({})", actor.guid()));
            db.gm_commands
                .lock()
                .unwrap()
                .push((actor.guid(), alpha_test_tools, text.clone()));
            if alpha_test_tools && text.starts_with(".speed") {
                Ok(())
            } else {
                Err(
                    crate::stdb::ReducerCallError::refused("gw_gm_command", "permission denied")
                        .into(),
                )
            }
        }
        fn character_location(&self, guid: u64) -> Option<(u32, u64)> {
            let db = self.store();
            db.note(&format!("character_location({guid})"));
            db.characters
                .lock()
                .unwrap()
                .get(&guid)
                .map(|(_, loc)| *loc)
        }
        fn set_character_shard(&self, guid: u64, map_id: u32, instance_id: u64) -> Result<()> {
            let db = self.store();
            db.note(&format!("set_character_shard({guid})"));
            let mut phases = db.shard_phases.lock().unwrap();
            let current = phases
                .get(&guid)
                .copied()
                .or_else(|| self.indexed_partition(guid));
            if current.is_some_and(|row| row.transfer_pending) {
                return Err(anyhow!("Character has a pending Realm Transfer phase"));
            }
            let same_partition =
                current.is_some_and(|row| (row.map_id, row.instance_id) == (map_id, instance_id));
            phases.insert(
                guid,
                crate::world::party::RealmCharacterPartition {
                    map_id,
                    instance_id,
                    revision: current.map_or(1, |row| {
                        if same_partition {
                            row.revision.max(1)
                        } else {
                            row.revision.max(1).saturating_add(1)
                        }
                    }),
                    transfer_pending: false,
                    pending_destination_map: 0,
                    pending_destination_instance: 0,
                    bot_source_identity: current
                        .filter(|_| same_partition)
                        .map_or(spacetimedb_sdk::Identity::ZERO, |row| {
                            row.bot_source_identity
                        }),
                    bot_transfer_intent_id: current
                        .filter(|_| same_partition)
                        .map_or(0, |row| row.bot_transfer_intent_id),
                    bot_controller_generation: current
                        .filter(|_| same_partition)
                        .map_or(0, |row| row.bot_controller_generation),
                },
            );
            db.shard_index
                .lock()
                .unwrap()
                .insert(guid, (map_id, instance_id));
            Ok(())
        }
        fn realm_character_partition(
            &self,
            guid: u64,
        ) -> Result<Option<crate::world::party::RealmCharacterPartition>> {
            self.store()
                .note(&format!("realm_character_partition({guid})"));
            let mut remaining = self.store().unobserved_locator_reads.lock().unwrap();
            if *remaining > 0 {
                *remaining -= 1;
                return Ok(None);
            }
            Ok(self.stored_partition(guid))
        }
        fn begin_character_shard_transfer(
            &self,
            source_map: u32,
            source_instance: u64,
            source_revision: u64,
            destination_map: u32,
            destination_instance: u64,
            source_module_identity: spacetimedb_sdk::Identity,
            intent_id: u64,
            controller_generation: u64,
            character_guid: u64,
        ) -> Result<()> {
            let db = self.store();
            db.note(&format!("begin_character_shard_transfer({character_guid})"));
            if (source_module_identity == spacetimedb_sdk::Identity::ZERO) != (intent_id == 0)
                || (source_revision == 0
                    && (source_module_identity != spacetimedb_sdk::Identity::ZERO
                        || intent_id != 0
                        || controller_generation != 0))
            {
                return Err(anyhow!("Transfer locator identity is incomplete"));
            }
            let mut phases = db.shard_phases.lock().unwrap();
            let current = phases
                .get(&character_guid)
                .copied()
                .or_else(|| self.indexed_partition(character_guid));
            let Some(current) = current else {
                if source_revision != 0 {
                    return Err(anyhow!("Transfer source has no Realm locator"));
                }
                phases.insert(
                    character_guid,
                    crate::world::party::RealmCharacterPartition {
                        map_id: source_map,
                        instance_id: source_instance,
                        revision: 1,
                        transfer_pending: true,
                        pending_destination_map: destination_map,
                        pending_destination_instance: destination_instance,
                        bot_source_identity: source_module_identity,
                        bot_transfer_intent_id: intent_id,
                        bot_controller_generation: controller_generation,
                    },
                );
                db.shard_index
                    .lock()
                    .unwrap()
                    .insert(character_guid, (source_map, source_instance));
                return Ok(());
            };
            let crossing = (source_module_identity, intent_id, controller_generation);
            if current.transfer_pending
                && (current.map_id, current.instance_id, current.revision)
                    == (source_map, source_instance, source_revision.max(1))
                && (
                    current.pending_destination_map,
                    current.pending_destination_instance,
                ) == (destination_map, destination_instance)
                && (
                    current.bot_source_identity,
                    current.bot_transfer_intent_id,
                    current.bot_controller_generation,
                ) == crossing
            {
                return Ok(());
            }
            if source_revision == 0
                || current.transfer_pending
                || (current.map_id, current.instance_id, current.revision)
                    != (source_map, source_instance, source_revision)
            {
                return Err(anyhow!("Transfer Realm locator changed"));
            }
            phases.insert(
                character_guid,
                crate::world::party::RealmCharacterPartition {
                    transfer_pending: true,
                    pending_destination_map: destination_map,
                    pending_destination_instance: destination_instance,
                    bot_source_identity: source_module_identity,
                    bot_transfer_intent_id: intent_id,
                    bot_controller_generation: controller_generation,
                    ..current
                },
            );
            Ok(())
        }
        fn finish_character_shard_transfer(
            &self,
            intent: &crate::world::transfer::BotTransferIntent,
        ) -> Result<()> {
            self.store().note(&format!(
                "finish_character_shard_transfer({})",
                intent.bot_guid
            ));
            self.settle_partition(
                intent.bot_guid,
                intent.source_map,
                intent.source_instance,
                intent.source_locator_revision,
                intent.destination_map,
                intent.destination_instance,
                intent.source_module_identity,
                intent.id,
                intent.controller_generation,
            )
        }
        fn finish_player_character_shard_transfer(
            &self,
            character_guid: u64,
            source_map: u32,
            source_instance: u64,
            source_revision: u64,
            destination_map: u32,
            destination_instance: u64,
        ) -> Result<()> {
            self.store().note(&format!(
                "finish_player_character_shard_transfer({character_guid})"
            ));
            self.settle_partition(
                character_guid,
                source_map,
                source_instance,
                source_revision,
                destination_map,
                destination_instance,
                spacetimedb_sdk::Identity::ZERO,
                0,
                0,
            )
        }
        fn finish_pending_character_shard_transfer(
            &self,
            character_guid: u64,
            source_map: u32,
            source_instance: u64,
            source_revision: u64,
            destination_map: u32,
            destination_instance: u64,
            source_module_identity: spacetimedb_sdk::Identity,
            transfer_intent_id: u64,
            controller_generation: u64,
        ) -> Result<()> {
            self.store().note(&format!(
                "finish_pending_character_shard_transfer({character_guid})"
            ));
            self.settle_partition(
                character_guid,
                source_map,
                source_instance,
                source_revision,
                destination_map,
                destination_instance,
                source_module_identity,
                transfer_intent_id,
                controller_generation,
            )
        }
        fn has_escrow(&self, guid: u64) -> bool {
            let db = self.store();
            db.note(&format!("has_escrow({guid})"));
            db.escrows.lock().unwrap().contains(&guid)
        }
        fn session_count(&self) -> usize {
            let db = self.store();
            db.note("session_count()");
            *db.open_sessions.lock().unwrap()
        }
        fn record_shard_load(
            &self,
            shard: &str,
            writer_occupancy_pct: f32,
            sessions: u32,
            gateway_key: u64,
        ) -> Result<()> {
            let db = self.store();
            db.note(&format!("record_shard_load({shard})"));
            db.recorded_shard_loads.lock().unwrap().push((
                shard.to_string(),
                writer_occupancy_pct,
                sessions,
                gateway_key,
            ));
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fake::{account, realm, realm_with_dead_core, NEVER};
    use super::*;
    use crate::logon::{CoordinatorStore, LogonStore};

    const WORLD: &str = "world";
    const CORE: &str = "lyracore-realm";
    const INSTANCES: &str = "instances";
    const USER: &str = "PONYTAIL";
    const K: [u8; 40] = [7u8; 40];

    fn gm() -> Actor {
        Actor::new(42).unwrap()
    }

    fn split_realm() -> super::fake::Handle {
        let h = realm(&[WORLD, CORE], "", Some(CORE));
        h.db_at(CORE)
            .accounts
            .lock()
            .unwrap()
            .insert(USER.into(), account(9, USER, 0xAA, 0xBB));
        h.db_at(WORLD)
            .accounts
            .lock()
            .unwrap()
            .insert(USER.into(), account(3, USER, 0x11, 0x22));
        h
    }

    #[test]
    fn speed_uses_the_realm_wide_account_name_and_runs_on_the_home_shard() {
        let h = split_realm();
        h.db_at(CORE)
            .accounts
            .lock()
            .unwrap()
            .get_mut(USER)
            .expect("Realm-core Account")
            .alpha_test_tools = true;
        run_gm_command(&h, &USER.to_ascii_lowercase(), gm(), ".speed 3".into())
            .expect("Alpha Test Tools permit speed");

        assert_eq!(
            h.db_at(WORLD).gm_commands.lock().unwrap().as_slice(),
            &[(42, true, ".speed 3".to_string())],
            "the Home Shard did not receive current Realm-core authority"
        );
        assert!(
            h.db_at(CORE).gm_commands.lock().unwrap().is_empty(),
            "the Durable Request ran on Realm-core instead of the Actor's Home Shard"
        );
    }

    #[test]
    fn revoking_alpha_test_tools_refuses_the_next_command_without_a_new_world_session() {
        let h = split_realm();
        h.db_at(CORE)
            .accounts
            .lock()
            .unwrap()
            .get_mut(USER)
            .expect("Realm-core Account")
            .alpha_test_tools = true;
        run_gm_command(&h, USER, gm(), ".speed 3".into()).expect("first command is authorized");

        h.db_at(CORE)
            .accounts
            .lock()
            .unwrap()
            .get_mut(USER)
            .expect("Realm-core Account")
            .alpha_test_tools = false;
        let refusal = run_gm_command(&h, USER, gm(), ".speed 4".into())
            .expect_err("the same World Session must see the revocation");

        assert_eq!(
            crate::stdb::classify(&refusal),
            crate::stdb::DurableFailure::Refusal {
                reason: "permission denied"
            }
        );
        assert_eq!(
            h.db_at(WORLD).gm_commands.lock().unwrap().as_slice(),
            &[
                (42, true, ".speed 3".to_string()),
                (42, false, ".speed 4".to_string()),
            ],
            "each command must carry a fresh Realm-core authority result"
        );
    }

    #[test]
    fn a_dead_realm_core_fails_before_the_home_shard_command_request() {
        let h = realm_with_dead_core(&[WORLD, CORE], "", CORE);
        let error = run_gm_command(&h, USER, gm(), ".speed 3".into())
            .expect_err("a dead Realm-core cannot authorize a command");
        assert_eq!(
            crate::stdb::classify(&error),
            crate::stdb::DurableFailure::TransportLoss
        );
        assert!(h.db_at(WORLD).gm_commands.lock().unwrap().is_empty());
    }

    #[test]
    fn an_unknown_account_is_a_refusal_the_gm_reads() {
        let h = split_realm();
        let error = run_gm_command(&h, "NOBODY", gm(), ".speed 3".into())
            .expect_err("Realm-core holds no such Account");
        assert_eq!(
            crate::stdb::classify(&error),
            crate::stdb::DurableFailure::Refusal {
                reason: "no Account named NOBODY on Realm-core"
            }
        );
        assert!(h.db_at(WORLD).gm_commands.lock().unwrap().is_empty());
    }

    #[test]
    fn the_srp6_challenge_material_comes_from_realm_core_never_the_world_shards_cache() {
        let h = split_realm();
        let store = CoordinatorStore::new(h.clone());
        let a = store
            .account(USER)
            .expect("lookup succeeds")
            .expect("the account exists");
        assert_eq!(
            a.salt, [0xAA; 32],
            "the logon challenge was answered with the WORLD shard's salt. `game_account` on a \
             world shard is a write-through CACHE — refreshed at logon and \
             never authoritative — so authenticating against it means a password rotation or a ban \
             applied on realm-core silently stops being enforced. This is the regression the \
             realm-core auth split exists to prevent."
        );
        assert_eq!(a.verifier, [0xBB; 32], "same, for the SRP6 verifier");
        assert_eq!(
            a.id, 9,
            "the challenge must carry REALM-CORE's account id: it is the id the \
             proof step writes the authoritative session row under"
        );
    }

    #[test]
    fn a_wrong_length_credential_blob_is_an_error_never_a_zero_padded_verifier() {
        for field in ["salt", "verifier"] {
            let h = realm(&[WORLD, CORE], "", Some(CORE));
            let mut row = account(9, USER, 0xAA, 0xBB);
            match field {
                "salt" => row.salt.truncate(31),
                _ => row.verifier.truncate(16),
            }
            h.db_at(CORE)
                .accounts
                .lock()
                .unwrap()
                .insert(USER.into(), row);

            let store = CoordinatorStore::new(h);
            let err = store.account(USER).expect_err(
                "a wrong-length credential blob must be an error — coercing it to a zero-padded \
                 array forges the all-zero `init` placeholder, and the logon path reads that as \
                 `FailUnknownAccount`: a corrupt account becomes indistinguishable from a typo",
            );
            let text = format!("{err:#}");
            assert!(
                text.contains(field) && text.contains("expected 32"),
                "the error must name WHICH blob is malformed and what was expected, or an operator \
                 cannot act on it; got: {text}"
            );
        }
    }

    #[test]
    fn an_unreachable_realm_core_refuses_the_challenge_rather_than_serving_the_stale_cache() {
        let h = realm_with_dead_core(&[WORLD, CORE], "", CORE);
        h.db_at(WORLD)
            .accounts
            .lock()
            .unwrap()
            .insert(USER.into(), account(3, USER, 0x11, 0x22));
        let store = CoordinatorStore::new(h);
        assert!(
            store.account(USER).is_err(),
            "a dead realm-core must fail the logon CLOSED. Falling through to the world shard \
             serves the SDK's last-known snapshot of salt/verifier/banned — precisely the stale \
             auth cache fail-closed exists to refuse."
        );
    }

    #[test]
    fn save_session_writes_each_database_under_that_databases_own_account_id() {
        let h = split_realm();
        let store = CoordinatorStore::new(h.clone());
        store
            .save_session(9, USER, &K, [0x5A; 32])
            .expect("both writes land");

        assert_eq!(
            h.db_at(CORE)
.sessions
.lock()
.unwrap()
.get(&9)
.map(|(k, _, _)| *k),
            Some(K),
            "realm-core's `game_session` must be written under REALM-CORE's id (9) — it is the row \
             every world gateway later reads to complete a handshake"
        );
        assert_eq!(
            h.db_at(WORLD)
.sessions
.lock()
.unwrap()
.get(&3)
.map(|(k, _, _)| *k),
            Some(K),
            "the world shard's write-through cache write is missing, or landed under the wrong id. \
             Its real job is binding `game_character.owner_identity` for this account's characters \
             so per-owner RLS lets the player see them: skip it and the player enters the world \
             with an empty character list. Carry REALM-CORE's id (9) into it instead and — on any \
             deployment whose two databases were seeded in different orders, which is this one — \
             you bind THIS login to ANOTHER account's characters."
        );
        assert!(
            !h.db_at(WORLD).sessions.lock().unwrap().contains_key(&9),
            "realm-core's account id was carried onto the world shard: it names a different player \
             there"
        );
    }

    #[test]
    fn an_unconfigured_realm_core_writes_exactly_one_session_row() {
        let h = realm(&[WORLD], "", None);
        h.db_at(WORLD)
            .accounts
            .lock()
            .unwrap()
            .insert(USER.into(), account(3, USER, 1, 2));
        let store = CoordinatorStore::new(h.clone());
        store
            .save_session(3, USER, &K, [0x5A; 32])
            .expect("the single write lands");
        assert_eq!(
            h.db_at(WORLD)
                .touched()
                .iter()
                .filter(|c| c.starts_with("establish_session"))
                .count(),
            1,
            "an unconfigured gateway must write `game_session` exactly ONCE — a second, identical \
             reducer call per logon is not byte-identical to the pre-realm-core gateway"
        );
    }

    #[test]
    fn every_world_side_call_resolves_the_world_shards_own_account_id_from_the_username() {
        let h = split_realm();
        h.db_at(WORLD)
            .characters
            .lock()
            .unwrap()
            .insert(100, (3, (0, 0)));
        h.db_at(WORLD)
            .characters
            .lock()
            .unwrap()
            .insert(200, (9, (0, 0)));
        h.db_at(WORLD)
            .characters
            .lock()
            .unwrap()
            .insert(201, (9, (0, 0)));
        let store = CoordinatorStore::new(h.clone());

        let realms = store.realms(9, USER).expect("realm list");
        assert_eq!(
            realms[0].number_of_characters, 1,
            "the realm-select screen counted a DIFFERENT account's characters: \
             `game_character.account_id` is the WORLD shard's key, and realm-core's id (9) names \
             another player there"
        );

        let ours = store.bound_identity(9, USER).expect("identity");
        let theirs = h.db_at(WORLD).identities.lock().unwrap().get(&9).copied();
        assert!(
            theirs != Some(ours),
            "the world connection was checked out under REALM-CORE's account id. \
             `Coordinator::player_conn` caches it under that id and `establish_session` stamps its \
             identity onto the world shard, but the world phase later checks the connection out \
             under the WORLD shard's id — so either nobody can enter the world (identity not \
             bound), or, with two accounts' ids transposed between the databases, one player \
             re-owns the other's characters."
        );
    }

    #[test]
    fn a_username_missing_on_the_world_shard_is_an_error_not_a_reused_realm_core_id() {
        let h = realm(&[WORLD, CORE], "", Some(CORE));
        h.db_at(CORE)
            .accounts
            .lock()
            .unwrap()
            .insert(USER.into(), account(9, USER, 0xAA, 0xBB));
        let store = CoordinatorStore::new(h);
        assert!(
            store.save_session(9, USER, &K, [0x5A; 32]).is_err(),
            "falling back to realm-core's id hands the world shard an id that, on any deployment \
             whose databases were not seeded in the same order, names a DIFFERENT player"
        );
    }

    #[test]
    fn the_world_handshake_reads_k_from_realm_core_and_the_account_id_from_the_world_shard() {
        let h = split_realm();
        h.db_at(CORE)
            .sessions
            .lock()
            .unwrap()
            .insert(9, (K, [0x5A; 32], NEVER));
        h.db_at(WORLD)
            .sessions
            .lock()
            .unwrap()
            .insert(3, ([0xEE; 40], [0x5A; 32], NEVER));

        let s = lookup_session(&h, USER)
            .expect("handshake lookup")
            .expect("a live session");
        assert_eq!(
            s.session_key, K,
            "the world handshake completed against the WORLD shard's cached copy of K. That copy \
             is refreshed at logon and never authoritative: sourcing K from it means a \
             gateway restarted mid-session, or a client reconnecting to a different shard's \
             gateway, authenticates off a snapshot instead of realm state. Realm-core owns the current session key."
        );
        assert_eq!(
            s.account_id, 3,
            "`WorldSession::account_id` carried REALM-CORE's id (9) into the world phase. Every \
             world-phase call (`characters`, `player_login`, `create_character`) means the WORLD \
             shard's `#[auto_inc]` key — hand it realm-core's and the player is served another \
             account's characters."
        );
    }

    #[test]
    fn an_expired_session_is_refused_like_an_absent_one() {
        let h = split_realm();
        h.db_at(CORE)
            .sessions
            .lock()
            .unwrap()
            .insert(9, (K, [0x5A; 32], 1));
        assert!(
            lookup_session(&h, USER)
                .expect("handshake lookup")
                .is_none(),
            "an expired session row must refuse the world handshake"
        );
    }

    #[test]
    fn a_new_logon_replaces_an_expired_session_for_the_world_handshake() {
        let h = split_realm();
        h.db_at(CORE)
            .sessions
            .lock()
            .unwrap()
            .insert(9, (K, [0x5A; 32], 1));
        assert!(lookup_session(&h, USER).unwrap().is_none());

        let fresh_key = [0xA7; 40];
        CoordinatorStore::new(h.clone())
            .save_session(9, USER, &fresh_key, [0x5A; 32])
            .unwrap();
        let session = lookup_session(&h, USER).unwrap().unwrap();
        assert_eq!(session.session_key, fresh_key);
        assert_eq!(session.account_id, 3);
    }

    #[test]
    fn a_session_expires_at_its_deadline_and_not_before() {
        assert!(!session_expired(999, 1_000));
        assert!(session_expired(1_000, 1_000));
        assert!(session_expired(1_001, 1_000));
    }

    #[test]
    fn the_world_handshake_fails_closed_when_realm_core_is_unreachable() {
        let h = realm_with_dead_core(&[WORLD, CORE], "", CORE);
        h.db_at(WORLD)
            .accounts
            .lock()
            .unwrap()
            .insert(USER.into(), account(3, USER, 1, 2));
        h.db_at(WORLD)
            .sessions
            .lock()
            .unwrap()
            .insert(3, ([0xEE; 40], [0x5A; 32], NEVER));
        assert!(
            lookup_session(&h, USER).is_err(),
            "with realm-core down the handshake must be refused, not served from the world DB's \
             session cache"
        );
    }

    #[test]
    fn an_unconfigured_realm_core_answers_the_handshake_from_the_one_database() {
        let h = realm(&[WORLD], "", None);
        h.db_at(WORLD)
            .accounts
            .lock()
            .unwrap()
            .insert(USER.into(), account(3, USER, 1, 2));
        h.db_at(WORLD)
            .sessions
            .lock()
            .unwrap()
            .insert(3, (K, [0x5A; 32], NEVER));
        let s = lookup_session(&h, USER)
            .expect("lookup")
            .expect("a live session");
        assert_eq!(
            (s.account_id, s.session_key),
            (3, K),
            "single-database behaviour is unchanged"
        );
    }

    fn routed_realm() -> super::fake::Handle {
        let h = realm(&[WORLD, INSTANCES, CORE], "36:*=instances", Some(CORE));
        h.db_at(INSTANCES)
            .characters
            .lock()
            .unwrap()
            .insert(100, (1, (36, 7)));
        h
    }

    #[test]
    fn a_mid_portal_heal_never_makes_the_next_login_resolve_the_destination_as_the_holder() {
        let h = realm(&[WORLD, INSTANCES, CORE], "36:*=instances", Some(CORE));

        h.db_at(WORLD)
            .characters
            .lock()
            .unwrap()
            .insert(100, (1, (36, 42)));
        assert_eq!(
            locate_home_shard(&h, 100)
                .expect("she is on WORLD")
                .shard_name(),
            WORLD,
            "the outbound resolution itself was never wrong — WORLD holds the only durable row"
        );
        assert_ne!(
            h.db_at(CORE).shard_index.lock().unwrap().get(&100).copied(),
            Some((36, 42)),
            "the index now claims she lives on the shard that owns map 36, while her only durable \
             row is on WORLD and no escrow has even been opened yet. The index answers WHICH SHARD \
             holds her, and this heal predicted a move instead of confirming one — `publish_shard \
             _index` after a successful `finish_transfer` is the only thing allowed to move it."
        );

        h.db_at(WORLD).escrows.lock().unwrap().insert(100);
        h.db_at(INSTANCES)
            .characters
            .lock()
            .unwrap()
            .insert(100, (1, (36, 42)));
        assert_eq!(
            locate_home_shard(&h, 100).expect("resolves").shard_name(),
            WORLD,
            "resolution must answer the shard holding the ESCROW — that is the SOURCE, and the only \
             shard `settle_transfer` can re-drive the unfinished transfer from. Answering INSTANCES \
             (which merely holds the imported copy) makes owner == holder, so the resume is never \
             driven and the source copy is stranded forever — reproduced live 3/3 runs."
        );
    }

    #[test]
    fn the_scan_prefers_the_shard_whose_row_agrees_with_the_map_over_iteration_order() {
        let h = realm(&[WORLD, INSTANCES, CORE], "36:*=instances", Some(CORE));
        h.db_at(WORLD)
            .characters
            .lock()
            .unwrap()
            .insert(100, (1, (36, 7)));
        h.db_at(INSTANCES)
            .characters
            .lock()
            .unwrap()
            .insert(100, (1, (36, 7)));

        assert_eq!(
            locate_home_shard(&h, 100).expect("resolves").shard_name(),
            INSTANCES,
            "the scan returned the DEFAULT shard's stale artefact because it was visited first. A \
             durable row is authoritative only on the shard the map assigns its own location to; \
             resolving by iteration order sends the player to a shard they do not live on."
        );
    }

    #[test]
    fn a_lone_disagreeing_row_still_resolves_rather_than_failing_the_login() {
        let h = realm(&[WORLD, INSTANCES, CORE], "36:*=instances", Some(CORE));
        h.db_at(WORLD)
            .characters
            .lock()
            .unwrap()
            .insert(100, (1, (36, 7)));
        assert_eq!(
            locate_home_shard(&h, 100).map(|d| d.shard_name().to_string()),
            Some(WORLD.to_string()),
            "the only durable row in the realm was discarded for disagreeing with the shard map, \
             which turns a recoverable stale row into a login that resolves to nothing at all"
        );
    }

    #[test]
    fn a_heal_is_declined_by_the_shard_map_not_by_being_the_default_database() {
        let h = realm(&[WORLD, INSTANCES, CORE], "36:*=instances", Some(CORE));
        h.db_at(INSTANCES)
            .characters
            .lock()
            .unwrap()
            .insert(100, (1, (0, 0)));
        locate_home_shard(&h, 100);
        assert_ne!(
            h.db_at(CORE).shard_index.lock().unwrap().get(&100).copied(),
            Some((0, 0)),
            "the heal wrote a location that resolves to WORLD for a character whose durable row is \
             on INSTANCES. The index answers WHICH SHARD holds her, so this entry names the wrong \
             one — and on the return leg of a transfer that strands the copy again, with the escrow on \
             INSTANCES and the index pointing at WORLD."
        );
    }

    #[test]
    fn locate_home_shard_trusts_a_correct_index_entry_without_scanning_the_other_shard() {
        let h = routed_realm();
        h.db_at(CORE)
            .shard_index
            .lock()
            .unwrap()
            .insert(100, (36, 7));
        let found = locate_home_shard(&h, 100).expect("resolves");
        assert_eq!(
            found.shard_name(),
            INSTANCES,
            "must route to the shard the index names"
        );
        assert!(
            h.db_at(WORLD).touched().is_empty(),
            "a correct index entry must not cost a scan of the OTHER connected shard — reads \
             were: {:?}",
            h.db_at(WORLD).touched()
        );
    }

    #[test]
    fn a_wrong_index_entry_still_routes_correctly_on_the_production_path_and_is_healed() {
        let h = routed_realm();
        h.db_at(CORE)
            .shard_index
            .lock()
            .unwrap()
            .insert(100, (0, 0));
        let found = locate_home_shard(&h, 100).expect("the scan finds the character");
        assert_eq!(
            found.shard_name(),
            INSTANCES,
            "a wrong hint must cost one extra scan, never a wrong route"
        );
        assert_eq!(
            h.db_at(CORE).shard_index.lock().unwrap().get(&100).copied(),
            Some((36, 7)),
            "the stale entry must be healed on the path `settle_home_shard` calls"
        );
    }

    #[test]
    fn an_absent_index_entry_is_populated_by_the_scan_that_had_to_run_without_it() {
        let h = routed_realm();
        assert!(locate_home_shard(&h, 100).is_some());
        assert_eq!(
            h.db_at(CORE).shard_index.lock().unwrap().get(&100).copied(),
            Some((36, 7)),
            "the index must be populated by the scan that had to run without it"
        );
    }

    #[test]
    fn a_first_kalimdor_login_can_begin_transfer_without_a_realm_locator() {
        let h = realm(
            &[WORLD, "kalimdor", INSTANCES, CORE],
            "1:*=kalimdor,36:*=instances",
            Some(CORE),
        );
        h.db_at(WORLD)
            .characters
            .lock()
            .unwrap()
            .insert(100, (1, (1, 0)));
        let holder = locate_home_shard(&h, 100).expect("the Character remains on its Home Shard");
        assert_eq!(holder.shard_name(), WORLD);
        assert!(h.at(CORE).realm_character_partition(100).unwrap().is_none());

        let pending = begin_shard_index_transfer(&holder, &kalimdor_plan(), None)
            .expect("the first Kalimdor login must begin its Transfer");
        assert!(pending.transfer_pending);
        assert_eq!(pending.revision, 1);
        assert_eq!(pending.pending_destination_map, 1);
        assert_eq!(pending.pending_destination_instance, 0);
    }

    fn kalimdor_plan() -> crate::world::transfer::TransferPlan {
        crate::world::transfer::TransferPlan {
            transfer_id: 100,
            character_guid: 100,
            dest_map_id: 1,
            dest_instance_id: 0,
            dest_x: 0.0,
            dest_y: 0.0,
            dest_z: 0.0,
            dest_o: 0.0,
        }
    }

    #[test]
    fn an_in_world_transfer_can_begin_without_a_realm_locator() {
        let h = realm(&[WORLD, INSTANCES, CORE], "36:*=instances", Some(CORE));
        h.store()
            .characters
            .lock()
            .unwrap()
            .insert(100, (1, (0, 0)));
        let pending = begin_shard_index_transfer(&h, &kalimdor_plan(), None).unwrap();
        assert_eq!(
            (pending.map_id, pending.instance_id, pending.revision),
            (0, 0, 1)
        );
        assert!(pending.transfer_pending);
        assert_eq!(pending.pending_destination_map, 1);
    }

    #[test]
    fn a_missing_character_cannot_create_a_realm_transfer_locator() {
        let h = realm(&[WORLD, INSTANCES, CORE], "36:*=instances", Some(CORE));
        assert!(begin_shard_index_transfer(&h, &kalimdor_plan(), None).is_err());
        assert!(h.at(CORE).realm_character_partition(100).unwrap().is_none());
    }

    #[test]
    fn a_first_transfer_waits_for_its_new_locator_subscription() {
        let h = realm(&[WORLD, INSTANCES, CORE], "36:*=instances", Some(CORE));
        h.store()
            .characters
            .lock()
            .unwrap()
            .insert(100, (1, (1, 0)));
        *h.db_at(CORE).unobserved_locator_reads.lock().unwrap() = 2;
        let pending = begin_shard_index_transfer(&h, &kalimdor_plan(), None).unwrap();
        assert!(pending.transfer_pending);
        assert_eq!(pending.revision, 1);
    }

    #[test]
    fn a_pending_destination_hint_keeps_routing_to_source_escrow_until_source_finish() {
        let h = realm(
            &[WORLD, "kalimdor", INSTANCES, CORE],
            "1:*=kalimdor,36:*=instances",
            Some(CORE),
        );
        for shard in [WORLD, "kalimdor"] {
            h.db_at(shard)
                .characters
                .lock()
                .unwrap()
                .insert(100, (1, (1, 0)));
        }
        h.store().escrows.lock().unwrap().insert(100);
        let pending = begin_shard_index_transfer(&h, &kalimdor_plan(), None).unwrap();

        assert_eq!(locate_home_shard(&h, 100).unwrap().shard_name(), WORLD);
        assert_eq!(
            h.at(CORE).realm_character_partition(100).unwrap(),
            Some(pending)
        );

        h.store().characters.lock().unwrap().remove(&100);
        h.store().escrows.lock().unwrap().remove(&100);
        assert_eq!(locate_home_shard(&h, 100).unwrap().shard_name(), "kalimdor");
        assert_eq!(
            h.at(CORE).realm_character_partition(100).unwrap(),
            Some(pending)
        );
    }

    #[test]
    fn a_correct_index_entry_is_not_rewritten_by_locate_home_shard() {
        let h = routed_realm();
        h.db_at(CORE)
            .shard_index
            .lock()
            .unwrap()
            .insert(100, (36, 7));
        locate_home_shard(&h, 100);
        assert!(
            !h.db_at(CORE)
                .touched()
                .iter()
                .any(|c| c.starts_with("set_character_shard")),
            "an index entry the scan agrees with must not be rewritten — a reducer round trip per \
             login, forever, for a row that is already correct"
        );
    }

    #[test]
    fn locate_home_shard_degrades_to_the_scan_when_realm_core_is_unreachable() {
        let h = realm_with_dead_core(&[WORLD, INSTANCES, CORE], "36:*=instances", CORE);
        h.db_at(INSTANCES)
            .characters
            .lock()
            .unwrap()
            .insert(100, (1, (36, 7)));
        let found = locate_home_shard(&h, 100).expect("the scan still finds the character");
        assert_eq!(
            found.shard_name(),
            INSTANCES,
            "a dead realm-core must cost the hint, not the login — routing degrades to the scan"
        );
    }

    #[test]
    fn locate_home_shard_still_prefers_the_shard_holding_the_escrow_in_the_fallback_scan() {
        let h = realm(&[WORLD, INSTANCES, CORE], "36:*=instances", Some(CORE));
        h.db_at(WORLD)
            .characters
            .lock()
            .unwrap()
            .insert(100, (1, (0, 0)));
        h.db_at(INSTANCES)
            .characters
            .lock()
            .unwrap()
            .insert(100, (1, (36, 7)));
        h.db_at(INSTANCES).escrows.lock().unwrap().insert(100);
        let found = locate_home_shard(&h, 100).expect("resolves");
        assert_eq!(
            found.shard_name(),
            INSTANCES,
            "the shard holding the ESCROW must win outright over an earlier-iterated shard that \
             merely holds a durable row — it is the only side a resumed transfer can be driven from"
        );
    }

    #[test]
    fn locate_home_shard_short_circuits_on_an_unsharded_gateway_reading_nothing() {
        let h = realm(&[WORLD], "", None);
        h.db_at(WORLD)
            .characters
            .lock()
            .unwrap()
            .insert(100, (1, (0, 0)));
        assert!(
            locate_home_shard(&h, 100).is_none(),
            "one connected database can only ever answer `stay put`"
        );
        assert!(
            h.db_at(WORLD).touched().is_empty(),
            "an unsharded gateway paid a read resolving its OWN home shard — `LYRACORE_REALM_CORE`/\
             `LYRACORE_SHARD_MAP` unset must cost the login hot path nothing. Reads \
             were: {:?}",
            h.db_at(WORLD).touched()
        );
    }

    #[test]
    fn resolve_delete_shard_routes_to_the_shard_actually_holding_the_character() {
        let h = routed_realm();
        let owner = resolve_delete_shard(&h, 100)
            .expect("resolves")
            .expect("must route OFF the default shard — the character lives on `instances`");
        assert_eq!(
            owner.shard_name(),
            INSTANCES,
            "delete must run on the shard actually holding the row, not the default `world` shard \
            — routing it there is exactly the NOT_FOUND-shaped failure players hit"
        );
    }

    #[test]
    fn resolve_delete_shard_still_names_self_when_the_character_already_lives_here() {
        let h = realm(&[WORLD, INSTANCES, CORE], "36:*=instances", Some(CORE));
        h.db_at(WORLD)
            .characters
            .lock()
            .unwrap()
            .insert(200, (1, (0, 0)));
        let owner = resolve_delete_shard(&h, 200)
            .expect("resolves")
            .expect("the character is resident somewhere — it must resolve, not vanish");
        assert_eq!(
            owner.shard_name(),
            WORLD,
            "a character already resident on `self` must resolve back to `self`'s OWN database, \
             never to a different one"
        );
    }

    #[test]
    fn resolve_delete_shard_answers_none_for_an_unknown_character() {
        let h = routed_realm();
        assert!(
            resolve_delete_shard(&h, 999_999)
                .expect("resolves")
                .is_none(),
            "an unresolvable guid must fall through to `self`'s own NOT_FOUND, not be routed"
        );
    }

    #[test]
    fn resolve_delete_shard_refuses_a_character_mid_transfer() {
        let h = routed_realm();
        h.db_at(INSTANCES).escrows.lock().unwrap().insert(100);
        assert!(
            resolve_delete_shard(&h, 100).is_err(),
            "a character with an in-flight escrow must be refused — deleting either half of a \
             split-in-flight row either destroys the character a moment before it lands on the \
             other side, or leaves a frozen copy the resumed transfer then fails against"
        );
    }

    #[test]
    fn resolve_delete_shard_short_circuits_on_an_unsharded_gateway_reading_nothing() {
        let h = realm(&[WORLD], "", None);
        h.db_at(WORLD)
            .characters
            .lock()
            .unwrap()
            .insert(1, (1, (0, 0)));
        assert!(resolve_delete_shard(&h, 1).expect("resolves").is_none());
        assert!(
            h.db_at(WORLD).touched().is_empty(),
            "an unsharded gateway paid a read resolving a delete target — `LYRACORE_REALM_CORE`/\
             `LYRACORE_SHARD_MAP` unset must cost delete nothing new. Reads were: {:?}",
            h.db_at(WORLD).touched()
        );
    }

    #[test]
    fn the_realm_fake_refuses_a_repair_during_transfer_and_keeps_both_views_coherent() {
        let h = realm(&[WORLD, CORE], "", Some(CORE));
        let realm = h.at(CORE);
        realm
            .store()
            .shard_index
            .lock()
            .unwrap()
            .insert(100, (0, 0));
        realm
            .begin_character_shard_transfer(
                0,
                0,
                1,
                36,
                7,
                spacetimedb_sdk::Identity::ZERO,
                0,
                0,
                100,
            )
            .expect("the player Transfer becomes pending");
        assert!(
            realm.set_character_shard(100, 36, 7).is_err(),
            "an ordinary repair must not replace a pending Transfer phase"
        );
        realm
            .finish_player_character_shard_transfer(100, 0, 0, 1, 36, 7)
            .expect("the exact player crossing settles");
        realm
            .set_character_shard(100, 0, 0)
            .expect("a later settled repair succeeds");
        let partition = realm
            .realm_character_partition(100)
            .expect("partition read")
            .expect("partition row");
        assert_eq!((partition.map_id, partition.instance_id), (0, 0));
        assert_eq!(partition.revision, 3);
        assert!(!partition.transfer_pending);
    }
}
