use std::sync::{mpsc, Arc};

use spacetimedb_sdk::{DbContext, Table};

use crate::accept::BlockingTaskCapacity;
use crate::config::GatewayConfig;
use crate::durable_test_support::{module_bytes, Standalone, POLL_TIMEOUT};
use crate::stdb::bindings::*;
use crate::stdb::Coordinator;
use crate::world::{
    Actor, AuctionActionStore, AuctionBrowseRequest, AuctionQuery, CastStore, MailStore,
};

const REALM: &str = "subscription-scope-realm";
const TOPOLOGY_VARS: [&str; 3] = [
    "LYRACORE_SHARD_MAP",
    "LYRACORE_SHARD_MAP_FILE",
    "LYRACORE_REALM_CORE",
];

struct TopologyEnv {
    previous: [Option<std::ffi::OsString>; 3],
    _guard: std::sync::MutexGuard<'static, ()>,
}

impl TopologyEnv {
    fn install(mixed_role: bool) -> Self {
        let guard = super::subscriptions::DURABLE_TOPOLOGY_ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let previous = TOPOLOGY_VARS.map(std::env::var_os);
        std::env::remove_var("LYRACORE_SHARD_MAP_FILE");
        std::env::set_var(
            "LYRACORE_SHARD_MAP",
            if mixed_role {
                format!("1:*={REALM}")
            } else {
                String::new()
            },
        );
        std::env::set_var("LYRACORE_REALM_CORE", REALM);
        Self {
            previous,
            _guard: guard,
        }
    }
}

impl Drop for TopologyEnv {
    fn drop(&mut self) {
        for (name, previous) in TOPOLOGY_VARS.into_iter().zip(&self.previous) {
            match previous {
                Some(value) => std::env::set_var(name, value),
                None => std::env::remove_var(name),
            }
        }
    }
}

fn catalogue_counts(coordinator: &Coordinator) -> Vec<(&'static str, usize)> {
    let live = coordinator.0.coord();
    let db = &live.conn.db;
    vec![
        (
            "game_creature_template",
            db.game_creature_template().iter().count(),
        ),
        (
            "game_start_position",
            db.game_start_position().iter().count(),
        ),
        ("game_spell", db.game_spell().iter().count()),
        ("game_spell_effect", db.game_spell_effect().iter().count()),
        ("game_npc_vendor", db.game_npc_vendor().iter().count()),
        (
            "game_quest_template",
            db.game_quest_template().iter().count(),
        ),
        (
            "game_quest_objective",
            db.game_quest_objective().iter().count(),
        ),
        (
            "game_gameobject_template",
            db.game_gameobject_template().iter().count(),
        ),
        ("game_taxi_node", db.game_taxi_node().iter().count()),
    ]
}

fn assert_scope(shard: &Standalone, realm: &Coordinator, mixed_role: bool) {
    for (table, cached) in catalogue_counts(realm) {
        let durable = shard
            .query_database_rows(REALM, &format!("SELECT * FROM {table}"))
            .len();
        assert!(durable > 0, "the Module fixture must populate {table}");
        assert_eq!(cached, if mixed_role { durable } else { 0 }, "{table}");
    }
    let live = realm.0.coord();
    assert!(live.conn.db.game_faction_template().iter().next().is_some());
    assert_eq!(
        live.conn
            .db
            .game_item_random_property()
            .property_id()
            .find(&5090101)
            .unwrap()
            .enchant_id_1,
        5090103
    );
    assert_eq!(
        live.conn
            .db
            .game_item_enchantment()
            .id()
            .find(&1303066368)
            .unwrap()
            .amount,
        7
    );
}

fn assert_retained_store_reads(
    world: &Coordinator,
    realm: &Coordinator,
    max_durability: u32,
    bid: u32,
) {
    let item = realm.item_template(5090050).unwrap().unwrap();
    assert_eq!(item.max_durability, max_durability);
    assert!(item.name.starts_with("Scope Blade"));
    let mut mail = realm.mail_list(1).unwrap();
    mail.sort_by_key(|letter| letter.subject.clone());
    assert_eq!(mail.len(), 3);
    let attached: Vec<_> = mail
        .iter()
        .filter(|letter| letter.item_entry != 0)
        .collect();
    assert_eq!(attached.len(), 2);
    for letter in attached {
        assert_eq!(letter.item_entry, 5090050);
        assert_eq!(letter.item_stack_count, 1);
        assert_eq!(letter.item_durability, 17);
        assert_eq!(letter.max_durability, max_durability);
        assert_eq!(realm.mail_by_id(letter.id).unwrap().unwrap(), *letter);
    }
    let page = world
        .auction_query(
            Actor::new(1).unwrap(),
            1,
            AuctionQuery::Browse(AuctionBrowseRequest {
                auctioneer_guid: 0,
                offset: 0,
                name: "Scope Blade".into(),
                minimum_level: None,
                maximum_level: None,
                inventory_type: None,
                item_class: None,
                item_subclass: None,
                quality: None,
                usable_only: false,
            }),
        )
        .unwrap();
    assert_eq!(page.total, 1);
    let [auction] = page.rows.as_slice() else {
        panic!("expected one retained auction, got {:?}", page.rows);
    };
    assert_eq!(auction.id, 5090050);
    assert_eq!(auction.owner_guid, 5090050);
    assert_eq!(auction.item_entry, 5090050);
    assert_eq!(auction.item_stack_count, 2);
    assert_eq!(auction.random_property_id, 117);
    assert_eq!(auction.highest_bid, bid);
}

fn verify_subscription_scope(mixed_role: bool) {
    let _topology = TopologyEnv::install(mixed_role);
    let mut shard = Standalone::start("subscription-scope");
    shard.publish_module();
    shard.publish_named_module_bytes(REALM, module_bytes());
    shard.assert_call("claim_operator", &[]);
    shard.assert_call_database(REALM, "claim_operator", &[]);
    shard.assert_call("debug_seed_scenario_fixtures", &[]);
    shard.assert_call_database(REALM, "debug_seed_scenario_fixtures", &[]);
    shard.assert_call("debug_spawn_player_entity", &["1"]);
    shard.assert_call_database(REALM, "debug_stage_mail_take_fixture", &[]);
    shard.assert_call_database(REALM, "debug_stage_auction_buyout_fixture", &[]);
    shard.assert_sql("UPDATE game_item_template SET name = 'World Blade' WHERE entry = 5090050");
    shard.assert_sql_database(REALM, "UPDATE game_item_template SET name = 'Scope Blade', max_durability = 77 WHERE entry = 5090050");
    shard.assert_sql_database(REALM, "INSERT INTO game_item_random_property (property_id,enchant_id_1,enchant_id_2,enchant_id_3,suffix) VALUES (5090101,5090103,0,0,'of the Fixture')");
    shard.assert_sql_database(REALM, "INSERT INTO game_item_enchantment (id,enchant_id,effect_index,kind,amount,spell_id,school_mask) VALUES (1303066368,5090103,0,14,7,0,0)");
    let cfg = GatewayConfig {
        logon_bind: "127.0.0.1:0".into(),
        world_bind: "127.0.0.1:0".into(),
        stdb_uri: shard.server().into(),
        module_name: shard.shard_name().into(),
        coordinator_token: Some(shard.owner_token()),
        gateway_id: "subscription-scope-test".into(),
        blocking_task_capacity: BlockingTaskCapacity::new(1),
    };
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let world = runtime.block_on(Coordinator::connect(&cfg)).unwrap();
    let realm = world.realm_core().unwrap();
    assert_eq!(realm.shard_name(), REALM);
    assert_eq!(
        world.shard_for(1, 0).unwrap().shard_name() == REALM,
        mixed_role
    );
    assert!(catalogue_counts(&world).iter().all(|(_, count)| *count > 0));
    assert_scope(&shard, &realm, mixed_role);
    assert_retained_store_reads(&world, &realm, 77, 201);
    let generation = realm
        .0
        .coord()
        .durable_character_presence_snapshot(1, REALM)
        .unwrap()
        .generation;

    realm.park_pump_for_test();
    shard.assert_sql_database(REALM, "UPDATE game_item_template SET name = 'Scope Blade Rebuilt', max_durability = 88 WHERE entry = 5090050");
    shard.assert_sql_database(
        REALM,
        "UPDATE game_auction SET highest_bid = 251, revision = 4 WHERE id = 5090050",
    );
    shard.assert_sql_database(
        REALM,
        "UPDATE game_spell SET name = 'Scope Rebuilt' WHERE spell_id = 50310",
    );
    let (reconnected, ready) = mpsc::channel();
    realm.0.on_reconnect.lock().unwrap().push(Arc::new(move || {
        let _ = reconnected.send(());
    }));
    realm.0.coord().conn.disconnect().unwrap();
    realm.resume_pump_for_test();
    ready.recv_timeout(POLL_TIMEOUT).unwrap();
    let fresh = realm
        .0
        .coord()
        .durable_character_presence_snapshot(1, REALM)
        .unwrap();
    assert!(fresh.generation > generation);
    assert_scope(&shard, &realm, mixed_role);
    assert_retained_store_reads(&world, &realm, 88, 251);
    let spell = realm.0.coord().conn.db.game_spell().spell_id().find(&50310);
    assert_eq!(
        spell.map(|row| row.name),
        mixed_role.then(|| "Scope Rebuilt".into())
    );
}

#[test]
#[ignore = "requires the SpacetimeDB 2.7.1 CLI and Wasm toolchain"]
fn dedicated_realm_core_keeps_retained_store_reads_and_omits_world_catalogues_after_reconnect() {
    verify_subscription_scope(false);
}

#[test]
#[ignore = "requires the SpacetimeDB 2.7.1 CLI and Wasm toolchain"]
fn realm_core_that_also_owns_a_world_map_keeps_world_catalogues_after_reconnect() {
    verify_subscription_scope(true);
}
