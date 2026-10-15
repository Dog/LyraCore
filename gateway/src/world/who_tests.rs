use super::party_tests::{character, party_topology};
use super::*;
use wow_world_messages::vanilla::{Level, CMSG_WHO};

fn wide_open_who() -> CMSG_WHO {
    CMSG_WHO {
        minimum_level: Level::new(0),
        maximum_level: Level::new(100),
        player_name: String::new(),
        guild_name: String::new(),
        race_mask: u32::MAX,
        class_mask: u32::MAX,
        zones: Vec::new(),
        search_strings: Vec::new(),
    }
}

fn decode_who(body: &[u8]) -> (u32, u32, Vec<String>) {
    let listed_players = u32::from_le_bytes(body[0..4].try_into().unwrap());
    let online_players = u32::from_le_bytes(body[4..8].try_into().unwrap());
    let mut rest = &body[8..];
    let mut names = Vec::new();
    for _ in 0..listed_players {
        let name_end = rest.iter().position(|&b| b == 0).unwrap();
        names.push(String::from_utf8(rest[..name_end].to_vec()).unwrap());
        rest = &rest[name_end + 1..];
        let guild_end = rest.iter().position(|&b| b == 0).unwrap();
        rest = &rest[guild_end + 1..];
        rest = &rest[16..];
    }
    (listed_players, online_players, names)
}

#[test]
fn who_lists_a_character_on_the_instance_pool_from_the_world_shard_and_back() {
    let (_realm, world, instances, _calls) = party_topology();
    let request = wide_open_who();

    let (_, body) = who::respond(world.as_ref(), 1, &request)
        .unwrap()
        .expect("a well-formed request always answers");
    let (.., names) = decode_who(&body);
    assert!(
        names.iter().any(|n| n == "Ginger"),
        "the requester's own shard: {names:?}"
    );
    assert!(
        names.iter().any(|n| n == "Vim"),
        "the Instance Pool, across the boundary: {names:?}"
    );

    let (_, body) = who::respond(instances.as_ref(), 1, &request)
        .unwrap()
        .expect("a well-formed request always answers");
    let (.., names) = decode_who(&body);
    assert!(
        names.iter().any(|n| n == "Vim"),
        "the requester's own shard: {names:?}"
    );
    assert!(
        names.iter().any(|n| n == "Ginger"),
        "the World Shard, across the boundary: {names:?}"
    );
}

#[test]
fn fifty_matches_list_forty_nine_and_report_fifty() {
    let characters: Vec<_> = (0..50u64)
        .map(|i| character(100 + i, &format!("Who{i}")))
        .collect();
    let live_guids = characters.iter().map(|c| c.guid).collect();
    let store = std::sync::Arc::new(WorldFake {
        characters,
        session: SessionState {
            live_guids,
            ..Default::default()
        },
        ..Default::default()
    });

    let (_, body) = who::respond(store.as_ref(), 1, &wide_open_who())
        .unwrap()
        .expect("a well-formed request always answers");
    let (listed_players, online_players, names) = decode_who(&body);
    assert_eq!(listed_players, 49, "the vanilla client's display cap");
    assert_eq!(names.len(), 49);
    assert_eq!(online_players, 50, "the match count is uncapped");
}

#[test]
fn an_unsharded_gateway_answers_who_with_the_same_filters() {
    let store = std::sync::Arc::new(WorldFake {
        characters: vec![
            codec::CharacterView {
                guid: 1,
                name: "Low".into(),
                race: 1,
                class: 1,
                level: 5,
                ..Default::default()
            },
            codec::CharacterView {
                guid: 2,
                name: "High".into(),
                race: 1,
                class: 1,
                level: 40,
                ..Default::default()
            },
        ],
        session: SessionState {
            live_guids: vec![1, 2],
            ..Default::default()
        },
        ..Default::default()
    });
    assert!(
        store.world_stores().is_empty(),
        "an unsharded gateway has no world_stores() fan-out"
    );
    let mut request = wide_open_who();
    request.minimum_level = Level::new(20);

    let (_, body) = who::respond(store.as_ref(), 1, &request)
        .unwrap()
        .expect("a well-formed request always answers");
    let (listed_players, _, names) = decode_who(&body);
    assert_eq!(listed_players, 1);
    assert_eq!(names, ["High"]);
}

#[test]
fn a_search_string_matches_a_zone_name_the_store_resolves() {
    let store = std::sync::Arc::new(WorldFake {
        characters: vec![codec::CharacterView {
            guid: 1,
            name: "Ginger".into(),
            race: 1,
            class: 1,
            level: 10,
            zone_id: 12,
            ..Default::default()
        }],
        session: SessionState {
            live_guids: vec![1],
            ..Default::default()
        },
        social: SocialState {
            zone_names: [(12, "Elwynn Forest".to_string())].into(),
            ..Default::default()
        },
        ..Default::default()
    });
    let mut request = wide_open_who();
    request.search_strings = vec!["elwynn".to_string()];

    let (_, body) = who::respond(store.as_ref(), 1, &request)
        .unwrap()
        .expect("a well-formed request always answers");
    let (listed_players, _, names) = decode_who(&body);
    assert_eq!(
        listed_players, 1,
        "the seeded zone name must resolve and match: {names:?}"
    );

    let blank = std::sync::Arc::new(WorldFake {
        characters: vec![codec::CharacterView {
            guid: 1,
            name: "Ginger".into(),
            race: 1,
            class: 1,
            level: 10,
            zone_id: 12,
            ..Default::default()
        }],
        session: SessionState {
            live_guids: vec![1],
            ..Default::default()
        },
        ..Default::default()
    });
    let (_, body) = who::respond(blank.as_ref(), 1, &request)
        .unwrap()
        .expect("a well-formed request always answers");
    let (listed_players, ..) = decode_who(&body);
    assert_eq!(
        listed_players, 0,
        "a blank zone name must not match \"elwynn\""
    );
}

#[test]
fn who_reply_lists_every_online_player_with_guild_level_and_zone() {
    let mut s = quest_store();
    s.guild.guild_memberships = vec![codec::GuildMemberView {
        character_guid: 2,
        guild_id: 7,
        name: "Alpha".into(),
        ..Default::default()
    }];
    s.guild.guilds = vec![codec::GuildView {
        guild_id: 7,
        name: "Boundary Test".into(),
        ..Default::default()
    }];
    s.characters = vec![
        codec::CharacterView {
            guid: 1,
            name: "Tester".into(),
            race: 1,
            class: 4,
            level: 10,
            zone_id: 12,
            ..Default::default()
        },
        codec::CharacterView {
            guid: 2,
            name: "Alpha".into(),
            race: 1,
            class: 1,
            level: 5,
            zone_id: 12,
            ..Default::default()
        },
        codec::CharacterView {
            guid: 3,
            name: "Bravo".into(),
            race: 1,
            class: 1,
            level: 60,
            zone_id: 12,
            ..Default::default()
        },
    ];
    let store = std::sync::Arc::new(s);
    let (mut client, mut c_enc, mut c_dec, server) = enter_world(store, 1);
    CMSG_WHO {
        minimum_level: Level::new(1),
        maximum_level: Level::new(60),
        player_name: String::new(),
        guild_name: String::new(),
        race_mask: 1 << 1,
        class_mask: 1 << 1,
        zones: Vec::new(),
        search_strings: Vec::new(),
    }
    .write_encrypted_client(&mut client, &mut c_enc)
    .unwrap();
    let (opcode, body) = read_raw_frame(&mut client, &mut c_dec);
    assert_eq!(opcode, codec::social::SMSG_WHO_OPCODE);
    let online_players = u32::from_le_bytes(body[4..8].try_into().unwrap());
    assert_eq!(online_players, 2);
    let mut rest = &body[8..];
    for (name, guild, level) in [("Alpha", "Boundary Test", 5u32), ("Bravo", "", 60)] {
        let name_end = rest.iter().position(|&b| b == 0).unwrap();
        assert_eq!(std::str::from_utf8(&rest[..name_end]).unwrap(), name);
        rest = &rest[name_end + 1..];
        let guild_end = rest.iter().position(|&b| b == 0).unwrap();
        assert_eq!(std::str::from_utf8(&rest[..guild_end]).unwrap(), guild);
        rest = &rest[guild_end + 1..];
        assert_eq!(u32::from_le_bytes(rest[0..4].try_into().unwrap()), level);
        rest = &rest[16..];
    }
    assert!(rest.is_empty(), "exactly two listed rows");
    drop(client);
    server.join().unwrap();
}
