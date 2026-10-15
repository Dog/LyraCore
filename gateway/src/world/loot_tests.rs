use super::loot;
use super::party;
use super::party_tests::{form_split_party, party_topology, GINGER, TRIN, VIM};
use super::*;
use lyracore_shared::loot_roll::loot_op;

const SOURCE: spacetimedb_sdk::Identity = spacetimedb_sdk::Identity::from_byte_array([7; 32]);

#[test]
fn a_loot_vote_during_realm_core_loss_cannot_vote_locally() {
    let (realm, world, _instances, _calls) = party_topology();
    *world.topology.realm_error.lock().unwrap() = Some("Realm-core unavailable".into());

    let error = loot::run_vote(
        world.as_ref(),
        Actor::new(GINGER).unwrap(),
        500,
        2,
        lyracore_shared::loot_roll::vote_kind::NEED,
    )
    .expect_err("Realm-core still owns the Loot Roll");

    assert_eq!(
        crate::stdb::classify(&error),
        crate::stdb::DurableFailure::TransportLoss
    );
    assert!(world.loot_roll.loot_rolls.lock().unwrap().is_empty());
    assert!(realm.loot_roll.realm_loot_ops.lock().unwrap().is_empty());
}

#[test]
fn loot_promotion_waits_for_realm_core_recovery() {
    let (realm, world, _instances, _calls) = party_topology();
    world
        .loot_roll
        .pending_rolls
        .lock()
        .unwrap()
        .push(loot::PendingLootRoll {
            promotion_source: SOURCE,
            roll_id: 77,
            corpse_guid: 500,
            slot: 2,
            item_entry: 1234,
            deadline_micros: 999_999,
            recipients: vec![GINGER, TRIN],
            random_property_id: 0,
        });
    *world.topology.realm_error.lock().unwrap() = Some("Realm-core unavailable".into());
    let mut watermark = 0;

    loot::relay_tick(world.as_ref(), &mut watermark);

    assert_eq!(watermark, 0);
    assert_eq!(world.loot_roll.pending_rolls.lock().unwrap().len(), 1);
    assert!(world.loot_roll.cleared_rolls.lock().unwrap().is_empty());
    assert!(realm.loot_roll.realm_loot_ops.lock().unwrap().is_empty());

    *world.topology.realm_error.lock().unwrap() = None;
    loot::relay_tick(world.as_ref(), &mut watermark);
    loot::relay_tick(world.as_ref(), &mut watermark);

    assert!(world.loot_roll.pending_rolls.lock().unwrap().is_empty());
    assert_eq!(realm.loot_roll.realm_loot_ops.lock().unwrap().len(), 1);
}

#[test]
fn an_unsharded_store_routes_the_vote_through_the_players_own_reducer() {
    let store = WorldFake::default();
    loot::run_vote(
        &store,
        Actor::new(42).unwrap(),
        500,
        2,
        lyracore_shared::loot_roll::vote_kind::NEED,
    )
    .unwrap();
    assert_eq!(
        store.loot_roll.loot_rolls.lock().unwrap().clone(),
        vec![(500, 2, lyracore_shared::loot_roll::vote_kind::NEED)],
        "unsharded, the vote must land on the SHARD's own `loot_roll` reducer"
    );
    assert!(
        store.loot_roll.realm_loot_ops.lock().unwrap().is_empty(),
        "unsharded, nothing must be routed to a realm-core plane"
    );
}

#[test]
fn a_sharded_store_routes_the_vote_to_realm_core_with_the_authenticated_guid() {
    let (realm, world, _instances, _calls) = party_topology();
    loot::run_vote(
        world.as_ref(),
        Actor::new(GINGER).unwrap(),
        500,
        2,
        lyracore_shared::loot_roll::vote_kind::GREED,
    )
    .unwrap();
    assert_eq!(
        realm.loot_roll.realm_loot_ops.lock().unwrap().clone(),
        vec![(
            loot_op::VOTE,
            500,
            2,
            0,
            GINGER,
            lyracore_shared::loot_roll::vote_kind::GREED,
            0,
            vec![],
            0,
            spacetimedb_sdk::Identity::ZERO,
            0
        )],
        "sharded, the vote must reach REALM-CORE, addressed by the AUTHENTICATED guid"
    );
    assert!(
        world.loot_roll.loot_rolls.lock().unwrap().is_empty(),
        "a multi-database gateway must not run the vote through the shard's own reducer — that is \
         exactly the shard-local behaviour the realm-wide loot relay removes for a promoted roll"
    );
}

#[test]
fn a_vote_on_a_staged_roll_reaches_realm_core_after_its_promotion() {
    let (realm, world, _instances, _calls) = party_topology();
    *world.loot_roll.pending_rolls.lock().unwrap() = vec![loot::PendingLootRoll {
        promotion_source: SOURCE,
        roll_id: 77,
        corpse_guid: 500,
        slot: 2,
        item_entry: 1234,
        deadline_micros: 999_999,
        recipients: vec![GINGER, TRIN],
        random_property_id: 0,
    }];

    loot::run_vote(
        world.as_ref(),
        Actor::new(GINGER).unwrap(),
        500,
        2,
        lyracore_shared::loot_roll::vote_kind::GREED,
    )
    .unwrap();

    let ops: Vec<(u8, u64, u8)> = realm
        .loot_roll
        .realm_loot_ops
        .lock()
        .unwrap()
        .iter()
        .map(|op| (op.0, op.1, op.2))
        .collect();
    assert_eq!(ops, vec![(loot_op::START, 500, 2), (loot_op::VOTE, 500, 2)]);
    assert_eq!(*world.loot_roll.cleared_rolls.lock().unwrap(), vec![77]);
}

#[test]
fn an_unsharded_store_does_nothing_even_with_pending_rolls_on_a_connected_peer() {
    let peer = std::sync::Arc::new(WorldFake {
        topology: TopologyState {
            shard: "peer".into(),
            ..Default::default()
        },
        ..Default::default()
    });
    *peer.loot_roll.pending_rolls.lock().unwrap() = vec![loot::PendingLootRoll {
        promotion_source: SOURCE,
        roll_id: 1,
        corpse_guid: 500,
        slot: 2,
        item_entry: 1234,
        deadline_micros: 999,
        recipients: vec![GINGER],
        random_property_id: 0,
    }];
    let store = WorldFake {
        topology: TopologyState {
            peers: std::sync::Mutex::new(vec![peer.clone()]),
            ..Default::default()
        },
        ..Default::default()
    };
    let mut watermark = 0u64;
    loot::relay_tick(&store, &mut watermark);
    assert!(
        peer.loot_roll.cleared_rolls.lock().unwrap().is_empty(),
        "no realm-core ⇒ nothing promoted, ever"
    );
    assert_eq!(
        watermark, 0,
        "the watermark must not advance on an unsharded store"
    );
}

#[test]
fn relay_tick_promotes_a_staging_roll_and_clears_it() {
    let (realm, world, instances, _calls) = party_topology();
    *world.loot_roll.pending_rolls.lock().unwrap() = vec![loot::PendingLootRoll {
        promotion_source: SOURCE,
        roll_id: 77,
        corpse_guid: 500,
        slot: 2,
        item_entry: 1234,
        deadline_micros: 999_999,
        recipients: vec![GINGER, TRIN],
        random_property_id: 0x1234_5678,
    }];
    let mut watermark = 0u64;
    loot::relay_tick(world.as_ref(), &mut watermark);
    assert_eq!(
        realm.loot_roll.realm_loot_ops.lock().unwrap().clone(),
        vec![(
            loot_op::START,
            500,
            2,
            1234,
            0,
            0,
            999_999,
            vec![GINGER, TRIN],
            0x1234_5678,
            SOURCE,
            77
        )],
        "the promoted roll must carry the ORIGINAL deadline and the FULL recipient snapshot"
    );
    assert_eq!(
        world.loot_roll.cleared_rolls.lock().unwrap().clone(),
        vec![77],
        "the staging copy must be cleared on the shard that created it, by its own roll id"
    );
    assert!(
        instances.loot_roll.cleared_rolls.lock().unwrap().is_empty(),
        "a shard with no pending rolls of its own must not be touched"
    );
}

#[test]
fn a_refused_promotion_keeps_its_source_identity_and_staging_row_for_retry() {
    let realm = std::sync::Arc::new(WorldFake {
        loot_roll: LootRollState {
            realm_loot_op_refusal: Some("another Loot Roll is active in this slot".into()),
            ..Default::default()
        },
        ..Default::default()
    });
    let world = std::sync::Arc::new(WorldFake {
        topology: TopologyState {
            realm: Some(realm.clone()),
            ..Default::default()
        },
        ..Default::default()
    });
    *world.topology.peers.lock().unwrap() = vec![world.clone()];
    let pending = loot::PendingLootRoll {
        roll_id: 78,
        corpse_guid: 500,
        slot: 2,
        item_entry: 1234,
        deadline_micros: 999_999,
        recipients: vec![GINGER, TRIN],
        random_property_id: 117,
        promotion_source: SOURCE,
    };
    *world.loot_roll.pending_rolls.lock().unwrap() = vec![pending.clone()];
    let mut watermark = 0;
    loot::relay_tick(world.as_ref(), &mut watermark);
    loot::relay_tick(world.as_ref(), &mut watermark);
    assert_eq!(
        *world.loot_roll.pending_rolls.lock().unwrap(),
        vec![pending]
    );
    assert!(world.loot_roll.cleared_rolls.lock().unwrap().is_empty());
    let attempts = realm.loot_roll.realm_loot_ops.lock().unwrap();
    assert_eq!(attempts.len(), 2);
    assert_eq!(attempts[0], attempts[1]);
    assert_eq!(attempts[0].8, 117);
    assert_eq!(attempts[0].9, SOURCE);
    assert_eq!(attempts[0].10, 78);
}

#[test]
fn relay_tick_settles_every_connected_shard_for_a_resolved_roll() {
    let (realm, world, instances, _calls) = party_topology();
    *realm.loot_roll.won_events.lock().unwrap() = vec![(500, 2, GINGER)];
    let mut watermark = 0u64;
    loot::relay_tick(world.as_ref(), &mut watermark);
    assert_eq!(
        world.loot_roll.settled_rolls.lock().unwrap().clone(),
        vec![(500, 2, GINGER)]
    );
    assert_eq!(
        instances.loot_roll.settled_rolls.lock().unwrap().clone(),
        vec![(500, 2, GINGER)],
        "settlement must fan out to EVERY connected shard, not just the one that happens to hold \
         the corpse — the relay cannot know which one that is in advance"
    );
    assert_eq!(
        watermark, 1,
        "the watermark must advance past the one event just settled"
    );

    loot::relay_tick(world.as_ref(), &mut watermark);
    assert_eq!(
        world.loot_roll.settled_rolls.lock().unwrap().len(),
        1,
        "an already-settled win must not be re-applied on the next tick"
    );
}

#[test]
fn a_failed_settlement_retries_after_the_shard_recovers() {
    let (realm, world, instances, _) = party_topology();
    let unavailable = std::sync::Arc::new(WorldFake {
        topology: TopologyState {
            shard: instances.topology.shard.clone(),
            ..Default::default()
        },
        loot_roll: LootRollState {
            settle_loot_roll_transport_lost: true,
            ..Default::default()
        },
        ..Default::default()
    });
    *world.topology.peers.lock().unwrap() = vec![unavailable.clone(), world.clone()];
    let win = (500, 2, GINGER);
    *realm.loot_roll.won_events.lock().unwrap() = vec![win];
    let mut watermark = 0;

    loot::relay_tick(world.as_ref(), &mut watermark);

    assert!(unavailable
        .loot_roll
        .settled_rolls
        .lock()
        .unwrap()
        .is_empty());
    assert_eq!(*world.loot_roll.settled_rolls.lock().unwrap(), vec![win]);
    assert_eq!(watermark, 0, "the failed Shard still needs this result");

    *world.topology.peers.lock().unwrap() = vec![instances.clone(), world.clone()];
    loot::relay_tick(world.as_ref(), &mut watermark);

    assert_eq!(
        *instances.loot_roll.settled_rolls.lock().unwrap(),
        vec![win]
    );
    assert_eq!(watermark, 1);
    loot::relay_tick(world.as_ref(), &mut watermark);
    assert_eq!(
        *instances.loot_roll.settled_rolls.lock().unwrap(),
        vec![win]
    );
}

#[test]
fn a_leave_flushes_every_connected_shards_pending_rolls_before_the_disband_reaches_realm_core() {
    let (realm, world, instances, calls) = party_topology();
    form_split_party(&world, &instances);
    *world.loot_roll.pending_rolls.lock().unwrap() = vec![loot::PendingLootRoll {
        promotion_source: SOURCE,
        roll_id: 99,
        corpse_guid: 500,
        slot: 2,
        item_entry: 1234,
        deadline_micros: 999,
        recipients: vec![GINGER],
        random_property_id: 0,
    }];

    party::run(
        instances.as_ref(),
        Actor::new(VIM).unwrap(),
        party::Op::Leave,
    )
    .expect("Vim leaves");

    assert_eq!(
        realm.loot_roll.realm_loot_ops.lock().unwrap().clone(),
        vec![(
            loot_op::START,
            500,
            2,
            1234,
            0,
            0,
            999,
            vec![GINGER],
            0,
            SOURCE,
            99
        )],
        "the pending roll on the OTHER shard must be promoted before the disband, not left behind"
    );
    assert_eq!(
        world.loot_roll.cleared_rolls.lock().unwrap().clone(),
        vec![99],
        "the staging copy must be cleared once promoted, same as a periodic relay tick would"
    );

    let log = calls.lock().unwrap().clone();
    let flush_at = log
        .iter()
        .position(|(_, c)| c == "realm_loot_op")
        .expect("the flush must have called realm_loot_op");
    let disband_at = log
        .iter()
        .rposition(|(_, c)| c == "realm_group_op")
        .expect("the LEAVE must have called realm_group_op");
    assert!(
        flush_at < disband_at,
        "the flush must land on realm-core BEFORE the disband op does. Calls were {log:?}"
    );
}

#[test]
fn a_non_disband_capable_op_never_flushes_pending_rolls() {
    let (realm, world, _instances, _calls) = party_topology();
    *world.loot_roll.pending_rolls.lock().unwrap() = vec![loot::PendingLootRoll {
        promotion_source: SOURCE,
        roll_id: 1,
        corpse_guid: 500,
        slot: 2,
        item_entry: 1234,
        deadline_micros: 999,
        recipients: vec![GINGER],
        random_property_id: 0,
    }];
    party::run(
        world.as_ref(),
        Actor::new(GINGER).unwrap(),
        party::Op::Invite(TRIN),
    )
    .expect("the invite runs");
    assert!(
        realm.loot_roll.realm_loot_ops.lock().unwrap().is_empty(),
        "an op that cannot shrink a group must not trigger a flush"
    );
    assert!(world.loot_roll.cleared_rolls.lock().unwrap().is_empty());
}

#[test]
fn loot_roll_dispatches_the_corpse_slot_and_vote() {
    let store = std::sync::Arc::new(quest_store());
    let (mut client, mut c_enc, _c_dec, server) = enter_world(store.clone(), 1);
    CMSG_LOOT_ROLL {
        item: Guid::new(60),
        item_slot: 2,
        vote: RollVote::Need,
    }
    .write_encrypted_client(&mut client, &mut c_enc)
    .unwrap();
    CMSG_LOOT_MONEY {}
        .write_encrypted_client(&mut client, &mut c_enc)
        .unwrap();
    drop(client);
    server.join().unwrap();
    assert_eq!(
        store.loot_roll.loot_rolls.lock().unwrap().as_slice(),
        &[(60, 2, RollVote::Need.as_int())]
    );
}

#[test]
fn loot_master_give_dispatches_the_corpse_slot_and_target() {
    let store = std::sync::Arc::new(quest_store());
    let (mut client, mut c_enc, _c_dec, server) = enter_world(store.clone(), 1);
    CMSG_LOOT_MASTER_GIVE {
        loot: Guid::new(60),
        slot_id: 3,
        player: Guid::new(9),
    }
    .write_encrypted_client(&mut client, &mut c_enc)
    .unwrap();
    CMSG_LOOT_MONEY {}
        .write_encrypted_client(&mut client, &mut c_enc)
        .unwrap();
    drop(client);
    server.join().unwrap();
    assert_eq!(
        store.loot_roll.loot_master_gives.lock().unwrap().as_slice(),
        &[(60, 3, 9)]
    );
}

#[test]
fn loot_roll_rejection_is_logged_and_ignored_not_session_fatal() {
    let mut s = quest_store();
    s.loot_roll.loot_action_refusal = Some(LootRefusal::RollUnavailable);
    let store = std::sync::Arc::new(s);
    let (mut client, mut c_enc, mut c_dec, server) = enter_world(store.clone(), 1);
    CMSG_LOOT_ROLL {
        item: Guid::new(60),
        item_slot: 2,
        vote: RollVote::Greed,
    }
    .write_encrypted_client(&mut client, &mut c_enc)
    .unwrap();
    CMSG_LOOT {
        guid: Guid::new(61),
    }
    .write_encrypted_client(&mut client, &mut c_enc)
    .unwrap();
    let (op, _) = read_raw_frame(&mut client, &mut c_dec);
    assert_eq!(
        op, OP_LOOT_RESPONSE,
        "the session must survive a rejected loot_roll"
    );
    drop(client);
    server.join().unwrap();
}

#[test]
fn loot_master_give_refusals_keep_the_world_session_alive() {
    for refusal in [
        LootRefusal::NotMasterLooter,
        LootRefusal::RecipientUnavailable,
        LootRefusal::RecipientInventoryFull,
    ] {
        let mut s = quest_store();
        s.loot_roll.loot_action_refusal = Some(refusal);
        let store = std::sync::Arc::new(s);
        let (mut client, mut c_enc, mut c_dec, server) = enter_world(store, 1);
        CMSG_LOOT_MASTER_GIVE {
            loot: Guid::new(60),
            slot_id: 3,
            player: Guid::new(9),
        }
        .write_encrypted_client(&mut client, &mut c_enc)
        .unwrap();
        CMSG_LOOT {
            guid: Guid::new(61),
        }
        .write_encrypted_client(&mut client, &mut c_enc)
        .unwrap();

        let (op, _) = read_raw_frame(&mut client, &mut c_dec);
        assert_eq!(op, OP_LOOT_RESPONSE, "{refusal:?}");
        drop(client);
        server.join().unwrap();
    }
}

#[test]
fn loot_roll_transport_loss_ends_the_world_session() {
    let mut s = quest_store();
    s.loot_roll.loot_action_transport_lost = true;
    let store = std::sync::Arc::new(s);
    let (mut client, mut c_enc, _c_dec, server) = enter_world(store, 1);
    CMSG_LOOT_ROLL {
        item: Guid::new(60),
        item_slot: 2,
        vote: RollVote::Greed,
    }
    .write_encrypted_client(&mut client, &mut c_enc)
    .unwrap();
    drop(client);

    assert!(
        server.join().is_err(),
        "an unknown vote result must end the World Session"
    );
}

#[test]
fn loot_master_give_transport_failure_ends_the_world_session() {
    let mut s = quest_store();
    s.loot_roll.loot_action_transport_lost = true;
    let store = std::sync::Arc::new(s);
    let (mut client, mut c_enc, _c_dec, server) = enter_world(store, 1);
    CMSG_LOOT_MASTER_GIVE {
        loot: Guid::new(60),
        slot_id: 3,
        player: Guid::new(9),
    }
    .write_encrypted_client(&mut client, &mut c_enc)
    .unwrap();
    drop(client);

    assert!(
        server.join().is_err(),
        "an unknown master-loot result must end the World Session"
    );
}
