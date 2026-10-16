use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;

use spacetimedb_sdk::{DbContext, Table};

use crate::accept::BlockingTaskCapacity;
use crate::config::GatewayConfig;
use crate::durable_test_support::{poll_until, Standalone, POLL_TIMEOUT};
use crate::stdb::bindings::*;
use crate::stdb::Coordinator;
use crate::world::QuestActionStore;

struct SingleShardEnv {
    previous: [Option<std::ffi::OsString>; 3],
    _guard: std::sync::MutexGuard<'static, ()>,
}

const TOPOLOGY_VARS: [&str; 3] = [
    "LYRACORE_SHARD_MAP",
    "LYRACORE_SHARD_MAP_FILE",
    "LYRACORE_REALM_CORE",
];

impl SingleShardEnv {
    fn install() -> Self {
        let guard = crate::stdb::subscriptions::DURABLE_TOPOLOGY_ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let previous = TOPOLOGY_VARS.map(std::env::var_os);
        for name in TOPOLOGY_VARS {
            std::env::remove_var(name);
        }
        Self {
            previous,
            _guard: guard,
        }
    }
}

impl Drop for SingleShardEnv {
    fn drop(&mut self) {
        for (name, previous) in TOPOLOGY_VARS.into_iter().zip(&self.previous) {
            match previous {
                Some(value) => std::env::set_var(name, value),
                None => std::env::remove_var(name),
            }
        }
    }
}

fn connect(shard: &Standalone, runtime: &tokio::runtime::Runtime) -> Coordinator {
    let cfg = GatewayConfig {
        logon_bind: "127.0.0.1:0".into(),
        world_bind: "127.0.0.1:0".into(),
        stdb_uri: shard.server().into(),
        module_name: shard.shard_name().into(),
        coordinator_token: Some(shard.owner_token()),
        gateway_id: "indexed-durable-test".into(),
        blocking_task_capacity: BlockingTaskCapacity::new(1),
    };
    runtime.block_on(Coordinator::connect(&cfg)).unwrap()
}

fn fence(coordinator: &Coordinator) -> u64 {
    coordinator
        .0
        .coord()
        .durable_character_presence_snapshot(1, coordinator.shard_name())
        .unwrap()
        .generation
}

fn items(coordinator: &Coordinator, guid: u64) -> Vec<(u64, u32, u8, u32)> {
    coordinator
        .player_items(guid)
        .unwrap()
        .into_iter()
        .map(|item| {
            assert_eq!(item.owner_guid, guid);
            (item.guid, item.entry, item.slot, item.stack_count)
        })
        .collect()
}

fn lootable(coordinator: &Coordinator, viewer_guid: u64) -> bool {
    use lyracore_shared::constants::unit_dynamic_flags::LOOTABLE;
    let live = coordinator.0.coord();
    let entity = live.conn.db.game_world_entity().guid().find(&1).unwrap();
    crate::codec::entity::subscriptions::projected_dynamic_flags(&live, viewer_guid, &entity)
        & LOOTABLE
        != 0
}

fn quest_state(coordinator: &Coordinator, guid: u64) -> Vec<(u32, u8)> {
    coordinator
        .player_quest_log(guid)
        .unwrap()
        .into_iter()
        .map(|quest| (quest.quest_id, quest.state))
        .collect()
}

#[test]
#[ignore = "requires the SpacetimeDB 2.7.1 CLI and Wasm toolchain"]
fn indexed_store_reads_follow_subscribe_reassignment_deletion_and_reconnect() {
    let _topology = SingleShardEnv::install();
    let mut shard = Standalone::start("indexed-store");
    shard.publish_module();
    shard.assert_call("claim_operator", &[]);
    shard.assert_call("install_guid_range", &["0"]);
    shard.assert_call("debug_seed_scenario_fixtures", &[]);
    shard.assert_call("debug_spawn_player_entity", &["1"]);
    for table in [
        "game_item_instance",
        "game_player_skill",
        "game_character_quest",
    ] {
        shard.assert_sql(&format!("DELETE FROM {table}"));
    }
    shard.assert_call("debug_grant_item", &["1", "5090052", "2"]);
    let item_guid: u64 = shard
        .query_rows("SELECT guid FROM game_item_instance WHERE owner_guid = 1")[0]["guid"]
        .parse()
        .unwrap();
    shard.assert_call("debug_set_skill", &["1", "43", "3"]);
    shard.assert_sql(
        "UPDATE game_player_skill SET current = 3, max_rank = 5 WHERE character_guid = 1",
    );
    shard.assert_sql("UPDATE game_quest_objective SET kind = 1, target_entry = 5090052, required_count = 2 WHERE quest_entry = 50900");
    shard.assert_call("debug_grant_quest", &["1", "50900"]);
    shard.assert_sql(
        "INSERT INTO game_quest_event_requirement (id,quest_entry) VALUES (9000001,50900)",
    );
    shard.assert_sql("INSERT INTO game_character_quest_event_credit (id,character_guid,quest_entry) VALUES (9000001,1,50900),(9000002,1,50900)");
    shard.assert_call("debug_fill_aura_slots", &["1", "1", "false", "1"]);
    shard.assert_sql("UPDATE game_aura SET eff_kind = 161, eff_p0 = 2, amount = 7, stacks = 2 WHERE target_guid = 1");
    shard.assert_sql("UPDATE game_world_entity SET dead = true, dynamic_flags = 1 WHERE guid = 1");
    shard.assert_sql("INSERT INTO game_corpse_loot_eligible (id,corpse_guid,eligible_guid) VALUES (9000001,1,1),(9000002,1,1)");

    let runtime = tokio::runtime::Runtime::new().unwrap();
    let coordinator = connect(&shard, &runtime);
    let generation = fence(&coordinator);
    assert_eq!(items(&coordinator, 1), [(item_guid, 5090052, 23, 2)]);
    assert!(items(&coordinator, 2).is_empty());
    assert_eq!(coordinator.player_skills(1).unwrap(), [(43, 3, 5)]);
    assert_eq!(quest_state(&coordinator, 1), [(50900, 1)]);
    assert_eq!(
        coordinator.effective_magic_resistances(1),
        [14, 0, 0, 0, 0, 0]
    );
    assert!(lootable(&coordinator, 1));
    assert!(!lootable(&coordinator, 2));

    shard.assert_sql("DELETE FROM game_character_quest_event_credit WHERE id = 9000001");
    shard.assert_sql("DELETE FROM game_corpse_loot_eligible WHERE id = 9000001");
    fence(&coordinator);
    assert_eq!(quest_state(&coordinator, 1), [(50900, 1)]);
    assert!(lootable(&coordinator, 1));

    shard.assert_sql("UPDATE game_item_instance SET owner_guid = 2, slot = 24, stack_count = 3 WHERE owner_guid = 1");
    shard.assert_sql(
        "UPDATE game_player_skill SET character_guid = 2, current = 4 WHERE character_guid = 1",
    );
    shard.assert_sql("UPDATE game_character_quest SET character_guid = 2, quest_entry = 50901 WHERE character_guid = 1");
    shard.assert_sql(
        "UPDATE game_quest_objective SET quest_entry = 50901 WHERE quest_entry = 50900",
    );
    shard.assert_sql(
        "UPDATE game_quest_event_requirement SET quest_entry = 50901 WHERE id = 9000001",
    );
    shard.assert_sql("UPDATE game_character_quest_event_credit SET character_guid = 2, quest_entry = 50901 WHERE id = 9000002");
    shard.assert_sql(
        "UPDATE game_aura SET target_guid = 2, amount = 11, stacks = 1 WHERE target_guid = 1",
    );
    shard.assert_sql("UPDATE game_corpse_loot_eligible SET eligible_guid = 2 WHERE id = 9000002");
    fence(&coordinator);
    assert!(items(&coordinator, 1).is_empty());
    assert_eq!(items(&coordinator, 2), [(item_guid, 5090052, 24, 3)]);
    assert!(coordinator.player_skills(1).unwrap().is_empty());
    assert_eq!(coordinator.player_skills(2).unwrap(), [(43, 4, 5)]);
    assert!(quest_state(&coordinator, 1).is_empty());
    assert_eq!(quest_state(&coordinator, 2), [(50901, 1)]);
    assert_eq!(coordinator.effective_magic_resistances(1), [0; 6]);
    assert_eq!(
        coordinator.effective_magic_resistances(2),
        [11, 0, 0, 0, 0, 0]
    );
    assert!(!lootable(&coordinator, 1));
    assert!(lootable(&coordinator, 2));

    shard.assert_sql("DELETE FROM game_character_quest_event_credit WHERE id = 9000002");
    fence(&coordinator);
    assert_eq!(quest_state(&coordinator, 2), [(50901, 0)]);
    shard.assert_sql("DELETE FROM game_quest_event_requirement WHERE id = 9000001");
    fence(&coordinator);
    assert_eq!(quest_state(&coordinator, 2), [(50901, 1)]);

    for table in [
        "game_item_instance",
        "game_player_skill",
        "game_character_quest",
        "game_quest_objective",
        "game_aura",
        "game_corpse_loot_eligible",
    ] {
        shard.assert_sql(&format!("DELETE FROM {table}"));
    }
    fence(&coordinator);
    assert!(items(&coordinator, 2).is_empty());
    assert!(coordinator.player_skills(2).unwrap().is_empty());
    assert!(quest_state(&coordinator, 2).is_empty());
    assert_eq!(coordinator.effective_magic_resistances(2), [0; 6]);
    assert!(!lootable(&coordinator, 2));

    coordinator.park_pump_for_test();
    let old_inventory = coordinator.0.coord().inventory.clone();
    shard.assert_call("debug_grant_item", &["1", "5090052", "1"]);
    shard.assert_call("debug_set_skill", &["1", "43", "1"]);
    shard.assert_call("debug_grant_quest", &["1", "50900"]);
    shard.assert_sql("INSERT INTO game_quest_objective (id,quest_entry,obj_index,kind,target_entry,required_count) VALUES (9000001,50900,0,1,5090052,2)");
    shard.assert_call("debug_fill_aura_slots", &["1", "1", "false", "1"]);
    shard.assert_sql("UPDATE game_aura SET eff_kind = 161, eff_p0 = 2, amount = 13, stacks = 1 WHERE target_guid = 1");
    shard.assert_sql(
        "INSERT INTO game_corpse_loot_eligible (id,corpse_guid,eligible_guid) VALUES (9000003,1,1)",
    );
    let (reconnected, ready) = mpsc::channel();
    coordinator
        .0
        .on_reconnect
        .lock()
        .unwrap()
        .push(Arc::new(move || {
            let _ = reconnected.send(());
        }));
    coordinator.0.coord().conn.disconnect().unwrap();
    coordinator.resume_pump_for_test();
    ready.recv_timeout(POLL_TIMEOUT).unwrap();
    assert!(fence(&coordinator) > generation);
    assert!(!Arc::ptr_eq(
        &old_inventory,
        &coordinator.0.coord().inventory
    ));
    let old_items = old_inventory.read().unwrap().item_guids(1);
    let restored = items(&coordinator, 1);
    assert_eq!(restored.len(), 1);
    assert_eq!(
        (restored[0].1, restored[0].2, restored[0].3),
        (5090052, 23, 1)
    );
    assert_eq!(coordinator.player_skills(1).unwrap(), [(43, 1, 5)]);
    assert_eq!(quest_state(&coordinator, 1), [(50900, 0)]);
    assert_eq!(
        coordinator.effective_magic_resistances(1),
        [13, 0, 0, 0, 0, 0]
    );
    assert!(lootable(&coordinator, 1));
    shard.assert_sql("DELETE FROM game_item_instance WHERE owner_guid = 1");
    fence(&coordinator);
    assert!(items(&coordinator, 1).is_empty());
    assert_eq!(old_inventory.read().unwrap().item_guids(1), old_items);
}

#[test]
#[ignore = "requires the SpacetimeDB 2.7.1 CLI and Wasm toolchain"]
fn a_store_read_waits_until_a_real_multi_table_transaction_finishes_its_callbacks() {
    let _topology = SingleShardEnv::install();
    let mut shard = Standalone::start("indexed-publication");
    shard.publish_module();
    shard.assert_call("claim_operator", &[]);
    shard.assert_call("install_guid_range", &["0"]);
    shard.assert_call("debug_seed_scenario_fixtures", &[]);
    shard.assert_call("debug_spawn_player_entity", &["1"]);
    shard.assert_sql("DELETE FROM game_item_instance WHERE owner_guid = 1");
    shard
        .assert_sql("UPDATE game_quest_objective SET required_count = 1 WHERE quest_entry = 50900");
    shard.assert_call("debug_grant_quest", &["1", "50900"]);
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let coordinator = connect(&shard, &runtime);
    assert_eq!(quest_state(&coordinator, 1), [(50900, 0)]);
    let (entered, callback_entered) = mpsc::channel();
    let (release, released) = mpsc::channel();
    let released = Mutex::new(Some(released));
    coordinator
        .0
        .on_pump(move |conn| {
            conn.db.game_item_instance().on_insert(move |_ctx, item| {
                if item.owner_guid != 1 || item.entry != 5090052 {
                    return;
                }
                if let Some(released) = released.lock().unwrap().take() {
                    entered.send(()).unwrap();
                    released.recv_timeout(POLL_TIMEOUT).unwrap();
                }
            });
        })
        .unwrap();
    shard.assert_call("debug_stress_relay", &["1", "51000", "5090052", "0"]);
    callback_entered.recv_timeout(POLL_TIMEOUT).unwrap();
    let (started, reader_started) = mpsc::channel();
    let (finished, read_finished) = mpsc::channel();
    let reader = coordinator.clone();
    let read = std::thread::spawn(move || {
        started.send(()).unwrap();
        finished.send(items(&reader, 1)).unwrap();
    });
    reader_started.recv_timeout(POLL_TIMEOUT).unwrap();
    let premature = read_finished.recv_timeout(Duration::from_millis(100));
    release.send(()).unwrap();
    assert!(matches!(premature, Err(mpsc::RecvTimeoutError::Timeout)));
    let published = read_finished.recv_timeout(POLL_TIMEOUT).unwrap();
    read.join().unwrap();
    assert_eq!(published.len(), 1);
    assert_eq!(
        (published[0].1, published[0].2, published[0].3),
        (5090052, 23, 1)
    );
    assert!(poll_until(POLL_TIMEOUT, || quest_state(&coordinator, 1)
        == [(50900, 1)]));
    let log = coordinator.player_quest_log(1).unwrap();
    assert_eq!(log[0].counts, [1]);
}
