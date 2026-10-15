use super::party_tests::{character, party_topology, BOT, DORMANT, GINGER, VIM};
use super::*;

fn place(shard: &WorldFake, guid: u64, entity: codec::MemberEntity) {
    shard
        .social
        .member_entities
        .lock()
        .unwrap()
        .push((guid, entity));
}

#[test]
fn presence_of_crosses_the_shard_boundary_and_tells_in_world_apart_from_session_online() {
    let (_realm, world, instances, _calls) = party_topology();
    place(&instances, VIM, codec::MemberEntity::default());
    place(&world, BOT, codec::MemberEntity::default());

    let vim = presence::of(world.as_ref(), VIM)
        .unwrap()
        .expect("Vim resolves from the open-world handle");
    assert_eq!(vim.name, "Vim");
    assert!(
        matches!(vim.whereabouts, presence::Whereabouts::InWorld { .. }),
        "Vim has a live entity on instances: {:?}",
        vim.whereabouts
    );
    assert!(vim.session_online, "Vim is not in offline_guids");

    let dormant = presence::of(world.as_ref(), DORMANT)
        .unwrap()
        .expect("a character row with no live entity still exists");
    assert_eq!(dormant.whereabouts, presence::Whereabouts::Offline);
    assert!(!dormant.session_online);

    let bot = presence::of(world.as_ref(), BOT)
        .unwrap()
        .expect("the playerbot resolves");
    assert!(
        matches!(bot.whereabouts, presence::Whereabouts::InWorld { .. }),
        "a playerbot has a live entity: {:?}",
        bot.whereabouts
    );
    assert!(!bot.session_online, "a playerbot never runs player_login");
}

#[test]
fn presence_of_reads_away_status_from_the_live_entitys_player_flags() {
    let store = std::sync::Arc::new(WorldFake {
        characters: vec![character(GINGER, "Ginger")],
        ..Default::default()
    });
    place(
        &store,
        GINGER,
        codec::MemberEntity {
            player_flags: lyracore_shared::constants::player_flags::DND,
            ..Default::default()
        },
    );
    let ginger = presence::of(store.as_ref(), GINGER).unwrap().unwrap();
    match ginger.whereabouts {
        presence::Whereabouts::InWorld { away, .. } => {
            assert_eq!(away, presence::AwayStatus::Dnd)
        }
        other => panic!("expected InWorld, got {other:?}"),
    }

    let no_entity = std::sync::Arc::new(WorldFake {
        characters: vec![character(DORMANT, "Dormant")],
        ..Default::default()
    });
    let dormant = presence::of(no_entity.as_ref(), DORMANT).unwrap().unwrap();
    assert_eq!(
        dormant.whereabouts,
        presence::Whereabouts::Offline,
        "a Character with no live entity has no Away Status to read"
    );
}

#[test]
fn a_live_hit_beats_a_hit_without_one_regardless_of_shard_order() {
    let calls: ShardCallLog = Default::default();
    let source = std::sync::Arc::new(WorldFake {
        topology: TopologyState {
            shard: "source".into(),
            calls: calls.clone(),
            ..Default::default()
        },
        characters: vec![character(GINGER, "Ginger")],
        ..Default::default()
    });
    let destination = std::sync::Arc::new(WorldFake {
        topology: TopologyState {
            shard: "destination".into(),
            calls,
            ..Default::default()
        },
        characters: vec![character(GINGER, "Ginger")],
        ..Default::default()
    });
    place(&destination, GINGER, codec::MemberEntity::default());
    for shard in [&source, &destination] {
        *shard.topology.peers.lock().unwrap() = vec![source.clone(), destination.clone()];
    }

    let found = presence::of(source.as_ref(), GINGER).unwrap().unwrap();
    assert!(
        matches!(found.whereabouts, presence::Whereabouts::InWorld { .. }),
        "the destination's live entity must win even though the source's own handle was asked \
         first and has no entity of its own: {:?}",
        found.whereabouts
    );
}

fn one_shard_topology(
    guid: u64,
    live: bool,
) -> (std::sync::Arc<WorldFake>, std::sync::Arc<WorldFake>) {
    let realm = std::sync::Arc::new(WorldFake {
        topology: TopologyState {
            shard: "realm".into(),
            ..Default::default()
        },
        party: PartyState {
            is_realm: true,
            ..Default::default()
        },
        ..Default::default()
    });
    let world = std::sync::Arc::new(WorldFake {
        topology: TopologyState {
            shard: "world".into(),
            realm: Some(realm.clone()),
            ..Default::default()
        },
        characters: vec![character(guid, "Named")],
        ..Default::default()
    });
    if live {
        place(&world, guid, codec::MemberEntity::default());
    }
    (realm, world)
}

#[test]
fn a_live_entity_on_any_connected_shard_is_live_even_while_another_shard_is_down() {
    let (_realm, world) = one_shard_topology(GINGER, true);
    let down = std::sync::Arc::new(WorldFake {
        topology: TopologyState {
            shard: "instances".into(),
            world_shard_set_error: Some("instances has no healthy Coordinator subscription".into()),
            ..Default::default()
        },
        ..Default::default()
    });
    *world.topology.peers.lock().unwrap() = vec![world.clone(), down.clone()];
    *down.topology.peers.lock().unwrap() = vec![world.clone(), down.clone()];

    let presence = presence::of(world.as_ref(), GINGER).unwrap().unwrap();
    assert!(
        matches!(presence.whereabouts, presence::Whereabouts::InWorld { .. }),
        "Ginger's live entity must win even though `instances` is down: {:?}",
        presence.whereabouts
    );
}

#[test]
fn a_pending_transfer_on_realm_core_is_in_transit() {
    let (realm, world) = one_shard_topology(GINGER, false);
    *realm.party.members_in_transit.lock().unwrap() = vec![GINGER];
    let down = std::sync::Arc::new(WorldFake {
        topology: TopologyState {
            shard: "instances".into(),
            world_shard_set_error: Some("down".into()),
            ..Default::default()
        },
        ..Default::default()
    });
    *world.topology.peers.lock().unwrap() = vec![world.clone(), down.clone()];

    let presence = presence::of(world.as_ref(), GINGER).unwrap().unwrap();
    assert_eq!(presence.whereabouts, presence::Whereabouts::InTransit);
}

#[test]
fn realm_core_loss_leaves_negative_whereabouts_unknown() {
    let (_realm, world) = one_shard_topology(GINGER, false);
    *world.topology.realm_error.lock().unwrap() = Some("Realm-core unavailable".into());

    for guid in [GINGER, 99_999] {
        let error = presence::of(world.as_ref(), guid)
            .expect_err("an unreadable pending Transfer cannot become absence");
        assert!(presence::is_unknown(&error));
        assert!(
            presence::unknown_as_absent::<presence::RealmPresence>(Err(error))
                .unwrap()
                .is_none()
        );
    }
}

#[test]
fn a_live_entity_still_answers_presence_during_realm_core_loss() {
    let (_realm, world) = one_shard_topology(GINGER, true);
    *world.topology.realm_error.lock().unwrap() = Some("Realm-core unavailable".into());

    let presence = presence::of(world.as_ref(), GINGER).unwrap().unwrap();

    assert!(matches!(
        presence.whereabouts,
        presence::Whereabouts::InWorld { .. }
    ));
}

#[test]
fn a_character_between_places_on_any_shard_is_in_transit() {
    let (_realm, world) = one_shard_topology(BOT, false);
    let instances = std::sync::Arc::new(WorldFake {
        topology: TopologyState {
            shard: "instances".into(),
            ..Default::default()
        },
        social: SocialState {
            members_between_places: vec![BOT].into(),
            ..Default::default()
        },
        ..Default::default()
    });
    *world.topology.peers.lock().unwrap() = vec![world.clone(), instances.clone()];
    *instances.topology.peers.lock().unwrap() = vec![world.clone(), instances.clone()];

    let presence = presence::of(world.as_ref(), BOT).unwrap().unwrap();
    assert_eq!(presence.whereabouts, presence::Whereabouts::InTransit);
}

#[test]
fn offline_needs_every_configured_shard_healthy() {
    let (_realm, world) = one_shard_topology(DORMANT, false);
    let down = std::sync::Arc::new(WorldFake {
        topology: TopologyState {
            shard: "instances".into(),
            world_shard_set_error: Some("instances has no healthy Coordinator subscription".into()),
            ..Default::default()
        },
        ..Default::default()
    });
    *world.topology.peers.lock().unwrap() = vec![world.clone(), down.clone()];
    let error = presence::of(world.as_ref(), DORMANT)
        .expect_err("an unhealthy configured Shard must refuse to let Offline through");
    assert!(
        presence::is_unknown(&error),
        "another Shard's health is not this session's transport"
    );

    let healthy = std::sync::Arc::new(WorldFake {
        topology: TopologyState {
            shard: "instances".into(),
            ..Default::default()
        },
        ..Default::default()
    });
    *world.topology.peers.lock().unwrap() = vec![world.clone(), healthy.clone()];
    let presence = presence::of(world.as_ref(), DORMANT).unwrap().unwrap();
    assert_eq!(presence.whereabouts, presence::Whereabouts::Offline);
}

#[test]
fn a_frozen_pre_transfer_copy_reads_online_but_not_in_world() {
    let store = std::sync::Arc::new(WorldFake {
        characters: vec![character(GINGER, "Ginger")],
        social: SocialState {
            members_between_places: vec![GINGER].into(),
            ..Default::default()
        },
        ..Default::default()
    });
    let presence = presence::of(store.as_ref(), GINGER).unwrap().unwrap();
    assert!(presence.session_online, "the frozen row keeps online true");
    assert!(
        !matches!(presence.whereabouts, presence::Whereabouts::InWorld { .. }),
        "a frozen copy with no live entity must never read as present: {:?}",
        presence.whereabouts
    );
    assert_eq!(presence.whereabouts, presence::Whereabouts::InTransit);
}
