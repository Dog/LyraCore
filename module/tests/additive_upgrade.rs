mod support;

use std::fs;

use serde_json::Value;
use support::{
    actor, event_rows, fence_subscription, module_bytes, poll_until, subscribe_with_fence, Row,
    Standalone, POLL_TIMEOUT,
};

const LEGACY_NOTIFICATIONS: &[&str] = &[
    "game_combat_event",
    "game_spell_cast_event",
    "game_spell_impact_event",
    "game_chat_event",
    "game_emote_event",
];

#[test]
#[ignore = "requires LYRACORE_TEST_BASELINE_WASM from 44b04878 and the SpacetimeDB 2.7.1 CLI"]
fn populated_old_module_upgrades_without_clearing() {
    let baseline_path = std::env::var_os("LYRACORE_TEST_BASELINE_WASM")
        .expect("set LYRACORE_TEST_BASELINE_WASM to the Module Wasm built at 44b04878f2050936faa61d6f087f3aa1a7a78a46 with debug_reducers,package_test_fixture");
    let baseline = fs::read(baseline_path).expect("read the baseline Module Wasm");
    assert!(baseline.starts_with(b"\0asm"), "baseline is not Wasm");
    let current = module_bytes();
    let mut shard = populated_old_module(&baseline);
    let character_before = character(&shard);
    let entity_before = entity(&shard);
    for column in [
        "simulation_indexed",
        "regeneration_pending",
        "combat_active",
    ] {
        assert!(
            !entity_before.contains_key(column),
            "the baseline already has {column}"
        );
    }
    let legacy_emotes = shard.query_rows("SELECT * FROM game_emote_event WHERE sender_guid = 1");
    assert_eq!(legacy_emotes.len(), 1);
    let old_schema = shard.describe();

    shard.publish_module_bytes(current);

    assert_eq!(character(&shard), character_before);
    let mut upgraded = entity(&shard);
    for column in [
        "simulation_indexed",
        "regeneration_pending",
        "combat_active",
    ] {
        assert_eq!(
            upgraded.remove(column).as_deref(),
            Some("false"),
            "{column}"
        );
    }
    assert_eq!(
        upgraded, entity_before,
        "old WorldEntity columns must survive"
    );
    assert_eq!(
        shard.query_rows("SELECT * FROM game_emote_event WHERE sender_guid = 1"),
        legacy_emotes
    );
    let new_schema = shard.describe();
    for name in LEGACY_NOTIFICATIONS {
        assert_eq!(
            table_schema(&new_schema, name),
            table_schema(&old_schema, name),
            "the published schema of {name} must stay intact"
        );
        table(&new_schema, &format!("{name}_live"));
    }

    shard.assert_call("debug_arm_instance_tick", &["0", "100"]);
    assert!(
        poll_until(POLL_TIMEOUT, || {
            let row = entity(&shard);
            row["simulation_indexed"] == "true"
                && row["regeneration_pending"] == "true"
                && row["combat_active"] == "true"
        }),
        "the scheduled backfill must derive simulation columns from preserved gameplay state"
    );
    assert_eq!(character(&shard), character_before);

    let events = subscribe_with_fence(
        &shard,
        &[
            "SELECT * FROM game_emote_event_live WHERE sender_guid = 1",
            "SELECT * FROM game_emote_event WHERE sender_guid = 1",
        ],
    );
    send_emote(&shard, 35);
    fence_subscription(&shard, &events);
    let emotes = event_rows(&events, "game_emote_event_live");
    assert_eq!(emotes.len(), 1);
    assert_eq!(emotes[0]["sender_guid"], "1");
    assert_eq!(emotes[0]["text_emote"], "35");
    assert!(event_rows(&events, "game_emote_event").is_empty());
    assert!(shard
        .query_rows("SELECT * FROM game_emote_event_live")
        .is_empty());
    assert_eq!(
        shard.query_rows("SELECT * FROM game_emote_event WHERE sender_guid = 1"),
        legacy_emotes
    );
}

fn populated_old_module(baseline: &[u8]) -> Standalone {
    let mut shard = Standalone::start("additive-upgrade");
    shard.publish_module_bytes(baseline);
    shard.assert_call("claim_operator", &[]);
    shard.assert_call("debug_spawn_player_entity", &["1"]);
    shard.assert_sql("DELETE FROM game_creature_move_schedule");
    shard.assert_sql("DELETE FROM game_melee_schedule");
    shard.assert_sql("DELETE FROM game_event_reaper_schedule");
    shard.assert_sql("UPDATE game_character SET money = 123456, xp = 37 WHERE guid = 1");
    let in_combat = lyracore_shared::constants::unit_flags::IN_COMBAT;
    shard.assert_sql(&format!(
        "UPDATE game_world_entity SET health = 17, max_health = 100, power = 0, max_power = 0, \
         unit_flags = {in_combat}, combat_until_ms = {} WHERE guid = 1",
        u64::MAX
    ));
    send_emote(&shard, 34);
    shard
}

fn send_emote(shard: &Standalone, text_emote: u32) {
    shard.assert_call(
        "gw_send_emote",
        &[&actor("1"), &text_emote.to_string(), "66", "0"],
    );
}

fn character(shard: &Standalone) -> Row {
    shard
        .query_rows("SELECT * FROM game_character WHERE guid = 1")
        .remove(0)
}

fn entity(shard: &Standalone) -> Row {
    shard
        .query_rows("SELECT * FROM game_world_entity WHERE guid = 1")
        .remove(0)
}

fn table<'a>(schema: &'a Value, name: &str) -> &'a Value {
    schema["tables"]
        .as_array()
        .expect("Module tables")
        .iter()
        .find(|table| table["name"] == name)
        .unwrap_or_else(|| panic!("missing table {name}"))
}

fn table_schema(schema: &Value, name: &str) -> Value {
    let mut definition = table(schema, name).clone();
    let type_ref = definition
        .as_object_mut()
        .unwrap()
        .remove("product_type_ref")
        .expect("table row type")
        .as_u64()
        .expect("table row type reference") as usize;
    let types = schema["typespace"]["types"].as_array().expect("typespace");
    definition["row_type"] = resolve_type(&types[type_ref], types);
    definition
}

fn resolve_type(value: &Value, types: &[Value]) -> Value {
    match value {
        Value::Object(fields) => {
            if let Some(type_ref) = fields.get("Ref").and_then(Value::as_u64) {
                return resolve_type(&types[type_ref as usize], types);
            }
            Value::Object(
                fields
                    .iter()
                    .map(|(name, value)| (name.clone(), resolve_type(value, types)))
                    .collect(),
            )
        }
        Value::Array(values) => Value::Array(
            values
                .iter()
                .map(|value| resolve_type(value, types))
                .collect(),
        ),
        value => value.clone(),
    }
}
