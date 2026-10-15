mod support;

use support::Standalone;

const OPTION: u32 = 509_9001;
const CONDITION: u32 = 509_9001;
const QUEST: u32 = 50_900;
const VENDOR: u32 = 51_004;

fn xp(shard: &Standalone) -> u32 {
    shard.query_rows("SELECT xp FROM game_world_entity WHERE guid = 1")[0]["xp"]
        .parse()
        .unwrap()
}

fn select(shard: &Standalone, npc: &str, row: u32, allowed: bool) {
    select_from(shard, npc, row, 0, allowed);
}

fn select_from(shard: &Standalone, npc: &str, row: u32, source_menu_id: u32, allowed: bool) {
    let before = xp(shard);
    let result = shard.call(
        "debug_gossip_select",
        &["1", npc, "0", &row.to_string(), &source_menu_id.to_string()],
    );
    assert_eq!(
        result.status.success(),
        allowed,
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert_eq!(
        xp(shard),
        before + u32::from(allowed),
        "only an eligible click may run the Runtime Script"
    );
}

fn fixture(name: &str) -> (Standalone, String) {
    let mut shard = Standalone::start(name);
    shard.publish_module();
    shard.assert_call("claim_operator", &[]);
    shard.assert_call("install_guid_range", &["0"]);
    shard.assert_call("debug_seed_scenario_fixtures", &[]);
    shard.assert_call("debug_spawn_player_entity", &["1"]);
    shard.assert_call("debug_spawn_at_feet", &["1", &VENDOR.to_string(), "2"]);
    let npc = shard.query_rows(&format!(
        "SELECT guid FROM game_world_entity WHERE entry = {VENDOR}"
    ))[0]["guid"]
        .clone();
    shard.assert_sql(&format!("INSERT INTO game_gossip_option (row_id,entry,option_index,icon,text,action,action_menu_id,cond_type,cond_value1,cond_value2) VALUES ({OPTION},{VENDOR},0,0,'Quest supplies',3,0,7,{CONDITION},0)"));
    shard.assert_sql(&format!("INSERT INTO game_gossip_condition (condition_id,kind,value1,value2,value3,value4,flags) VALUES ({CONDITION},9,{QUEST},1,0,0,0)"));
    let artifact = serde_json::json!({
        "kind": "script", "version": 1, "package": "test_fixture.gossip",
        "source_hash": "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
        "scripts": [{ "script_id": 100_901, "name": "gossip.clicked", "event": "on_gossip_select", "priority": 0, "enabled": true, "source": "grant_xp(event.actor, 1)" }]
    }).to_string();
    shard.assert_call(
        "apply_package_deltas",
        &["\"script\"", &serde_json::to_string(&artifact).unwrap()],
    );

    (shard, npc)
}

#[test]
#[ignore = "requires the SpacetimeDB 2.7.1 CLI and Wasm toolchain"]
fn imported_conditions_gate_current_quest_state_and_the_hook_in_one_transaction() {
    let (shard, npc) = fixture("gossip-conditions");
    select(&shard, &npc, OPTION, false);
    shard.assert_call("debug_grant_quest", &["1", &QUEST.to_string()]);
    select(&shard, &npc, OPTION, true);
    shard.assert_sql(&format!(
        "UPDATE game_gossip_condition SET value2 = 2 WHERE condition_id = {CONDITION}"
    ));
    select(&shard, &npc, OPTION, false);
    shard.assert_sql("UPDATE game_quest_objective SET required_count = 0 WHERE id = 5090000");
    select(&shard, &npc, OPTION, true);
    shard.assert_sql(&format!(
        "UPDATE game_character_quest SET rewarded = true WHERE quest_entry = {QUEST}"
    ));
    select(&shard, &npc, OPTION, false);
    shard.assert_sql(&format!(
        "UPDATE game_gossip_condition SET kind = 8, value2 = 0 WHERE condition_id = {CONDITION}"
    ));
    select(&shard, &npc, OPTION, true);

    shard.assert_sql(&format!(
        "UPDATE game_gossip_option SET entry = 51003 WHERE row_id = {OPTION}"
    ));
    select(&shard, &npc, OPTION, false);
    shard.assert_sql(&format!(
        "UPDATE game_gossip_option SET entry = {VENDOR} WHERE row_id = {OPTION}"
    ));
    select(&shard, &npc, OPTION + 1, false);
    shard.assert_sql(&format!("UPDATE game_gossip_condition SET kind = -3, value1 = 5099002, value2 = 0 WHERE condition_id = {CONDITION}"));
    shard.assert_sql("INSERT INTO game_gossip_condition (condition_id,kind,value1,value2,value3,value4,flags) VALUES (5099002,99,0,0,0,0,1)");
    select(&shard, &npc, OPTION, false);
    shard.assert_sql("DELETE FROM game_gossip_condition WHERE condition_id = 5099002");
    select(&shard, &npc, OPTION, false);
    shard.assert_sql(&format!(
        "UPDATE game_gossip_condition SET value1 = {CONDITION} WHERE condition_id = {CONDITION}"
    ));
    select(&shard, &npc, OPTION, false);
}

#[test]
#[ignore = "requires the SpacetimeDB 2.7.1 CLI and Wasm toolchain"]
fn a_menu_switch_refuses_colliding_rows_and_synthesized_options_before_the_hook() {
    const FIRST_MENU: u32 = 509_9001;
    const SECOND_MENU: u32 = 509_9002;
    let (shard, npc) = fixture("gossip-menu-source");
    shard.assert_sql(&format!("UPDATE game_gossip_condition SET kind = 0, value1 = 0, value2 = 0 WHERE condition_id = {CONDITION}"));
    shard.assert_sql(&format!("INSERT INTO game_gossip_menu_profile (menu_id,text_id) VALUES ({FIRST_MENU},1),({SECOND_MENU},1)"));
    shard.assert_sql(&format!("INSERT INTO game_gossip_menu_profile_option (row_id,menu_id,option_index,icon,text,action,action_menu_id,cond_type,cond_value1,cond_value2) VALUES ({OPTION},{FIRST_MENU},0,0,'Profile supplies',3,0,0,0,0)"));
    select_from(&shard, &npc, OPTION, 0, true);

    let entity = &shard.query_rows(&format!(
        "SELECT map_id,instance_id FROM game_world_entity WHERE guid = {npc}"
    ))[0];
    shard.assert_sql(&format!("INSERT INTO game_creature_gossip_menu_override (creature_guid,menu_id,map_id,instance_id) VALUES ({npc},{FIRST_MENU},{},{})", entity["map_id"], entity["instance_id"]));
    select_from(&shard, &npc, OPTION, 0, false);
    select_from(&shard, &npc, u32::MAX, 0, false);
    select_from(&shard, &npc, OPTION, FIRST_MENU, true);

    shard.assert_sql(&format!("UPDATE game_gossip_menu_profile_option SET menu_id = {SECOND_MENU} WHERE row_id = {OPTION}"));
    shard.assert_sql(&format!("UPDATE game_creature_gossip_menu_override SET menu_id = {SECOND_MENU} WHERE creature_guid = {npc}"));
    select_from(&shard, &npc, OPTION, FIRST_MENU, false);
    select_from(&shard, &npc, u32::MAX, FIRST_MENU, false);
    select_from(&shard, &npc, OPTION, SECOND_MENU, true);

    shard.assert_sql(&format!(
        "DELETE FROM game_creature_gossip_menu_override WHERE creature_guid = {npc}"
    ));
    select_from(&shard, &npc, OPTION, SECOND_MENU, false);
    select_from(&shard, &npc, OPTION, 0, true);
}
