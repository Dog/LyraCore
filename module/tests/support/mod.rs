#![allow(dead_code)]

use std::collections::BTreeMap;

use lyracore_shared::spatial::{grid_cell, GRID_CELL_SIZE, MAP_COORD_MAX};
pub use lyracore_test_support::*;

pub const LEG_YD: f32 = 20.0;

pub type Row = BTreeMap<String, String>;

pub fn event_rows(subscription: &TableSubscription, table: &str) -> Vec<Row> {
    subscription
        .inserts(table)
        .into_iter()
        .map(|row| {
            row.as_object()
                .expect("event row is an object")
                .iter()
                .map(|(column, value)| {
                    let value = if let Some(micros) =
                        value["__timestamp_micros_since_unix_epoch__"].as_i64()
                    {
                        spacetimedb::Timestamp::from_micros_since_unix_epoch(micros)
                            .to_rfc3339()
                            .expect("event timestamp is representable")
                    } else {
                        value
                            .as_str()
                            .map(str::to_owned)
                            .unwrap_or_else(|| value.to_string())
                    };
                    (column.clone(), value)
                })
                .collect()
        })
        .collect()
}

pub fn wait_for_event_rows(subscription: &TableSubscription, table: &str) -> Vec<Row> {
    let mut rows = Vec::new();
    assert!(
        poll_until(POLL_TIMEOUT, || {
            rows = event_rows(subscription, table);
            !rows.is_empty()
        }),
        "no inserts arrived for {table}"
    );
    rows
}

pub fn number<T: std::str::FromStr>(row: &Row, column: &str) -> T {
    row[column]
        .parse()
        .unwrap_or_else(|_| panic!("{column} is not a number in {row:?}"))
}

pub fn wolves(shard: &mut Standalone, offsets: &[f32]) -> Vec<Row> {
    shard.publish_module();
    shard.assert_call("claim_operator", &[]);
    shard.assert_call("install_guid_range", &["0"]);
    shard.assert_call("debug_seed_scenario_fixtures", &[]);
    shard.assert_call("debug_spawn_player_entity", &["1"]);
    shard.assert_sql("DELETE FROM game_world_entity WHERE entry = 51000");
    for offset in offsets {
        shard.assert_call("debug_spawn_at_feet", &["1", "51000", &offset.to_string()]);
    }
    shard.assert_sql("DELETE FROM game_world_entity WHERE guid = 1");
    let mut wolves = shard.query_rows("SELECT * FROM game_world_entity WHERE entry = 51000");
    assert_eq!(wolves.len(), offsets.len(), "{wolves:?}");
    wolves.sort_by_key(|wolf| number::<u64>(wolf, "guid"));
    wolves
}

pub fn wolves_beside(shard: &Standalone, character: &str, offsets: &[f32]) -> Vec<String> {
    shard.assert_sql("DELETE FROM game_world_entity WHERE entry = 51000");
    for offset in offsets {
        shard.assert_call(
            "debug_spawn_at_feet",
            &[character, "51000", &offset.to_string()],
        );
    }
    let mut guids: Vec<u64> = shard
        .query_rows("SELECT guid FROM game_world_entity WHERE entry = 51000")
        .iter()
        .map(|wolf| number(wolf, "guid"))
        .collect();
    assert_eq!(guids.len(), offsets.len(), "{guids:?}");
    guids.sort_unstable();
    guids.iter().map(u64::to_string).collect()
}

pub fn lone_wolf(shard: &mut Standalone) -> Row {
    wolves(shard, &[5.0]).remove(0)
}

pub fn leg_destination(at: &Row) -> (f32, f32, f32) {
    let (x, y, z): (f32, f32, f32) = (number(at, "x"), number(at, "y"), number(at, "z"));
    let (grid_x, _) = grid_cell(x, y);
    let centre_x = MAP_COORD_MAX - (grid_x as f32 + 0.5) * GRID_CELL_SIZE;
    let destination = (x + LEG_YD.copysign(centre_x - x), y, z);
    assert_eq!(grid_cell(destination.0, y), grid_cell(x, y));
    destination
}

pub fn leg(shard: &Standalone, guid: &str) -> Row {
    let mut legs = shard.query_rows(&format!(
        "SELECT sx, sy, sz, dx, dy, dz, start_micros, dur_ms, spline_id FROM game_creature_spline \
         WHERE guid = {guid}"
    ));
    assert_eq!(legs.len(), 1, "{legs:?}");
    legs.remove(0)
}

pub fn position(row: &Row) -> (f32, f32, f32) {
    (number(row, "x"), number(row, "y"), number(row, "z"))
}

pub fn distance(a: (f32, f32, f32), b: (f32, f32, f32)) -> f32 {
    ((a.0 - b.0).powi(2) + (a.1 - b.1).powi(2) + (a.2 - b.2).powi(2)).sqrt()
}

pub fn assert_near(actual: (f32, f32, f32), expected: (f32, f32, f32), what: &str) {
    assert!(
        (actual.0 - expected.0).abs() < 0.01
            && (actual.1 - expected.1).abs() < 0.01
            && (actual.2 - expected.2).abs() < 0.01,
        "{what}: expected {expected:?}, got {actual:?}"
    );
}
