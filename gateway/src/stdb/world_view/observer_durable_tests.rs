use super::*;
use crate::accept::BlockingTaskCapacity;
use crate::codec::combat::subscriptions::aura_updated;
use crate::codec::entity::subscriptions::entity_changed;
use crate::config::GatewayConfig;
use crate::durable_test_support::{poll_until, Standalone, POLL_TIMEOUT};
use crate::world::Outbound;
use std::sync::mpsc::Receiver;
use wow_world_messages::vanilla::opcodes::ServerOpcodeMessage;

struct ObserverRealm {
    coord: Coordinator,
    node: Standalone,
    _runtime: tokio::runtime::Runtime,
    _topology: std::sync::MutexGuard<'static, ()>,
}

impl ObserverRealm {
    fn start() -> Self {
        let topology = crate::stdb::subscriptions::DURABLE_TOPOLOGY_ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for name in [
            "LYRACORE_SHARD_MAP",
            "LYRACORE_SHARD_MAP_FILE",
            "LYRACORE_REALM_CORE",
        ] {
            assert!(
                std::env::var_os(name).is_none(),
                "unset {name} for the isolated observer test"
            );
        }
        let mut node = Standalone::start("observer-relay");
        node.publish_module();
        node.assert_call("claim_operator", &[]);
        node.assert_call("install_guid_range", &["0"]);
        node.assert_call("debug_spawn_player_entity", &["1"]);
        node.assert_sql("UPDATE game_world_entity SET x = 0, y = 0, grid_x = 0, grid_y = 0, cell = 0, map_id = 0 WHERE guid = 1");
        node.assert_call("debug_fill_aura_slots", &["1", "1", "false", "1"]);
        let cfg = GatewayConfig {
            logon_bind: "127.0.0.1:0".into(),
            world_bind: "127.0.0.1:0".into(),
            stdb_uri: node.server().into(),
            module_name: node.shard_name().into(),
            coordinator_token: Some(node.owner_token()),
            gateway_id: "observer-relay".into(),
            blocking_task_capacity: BlockingTaskCapacity::new(1),
        };
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let coord = runtime.block_on(Coordinator::connect(&cfg)).unwrap();
        Self {
            coord,
            node,
            _runtime: runtime,
            _topology: topology,
        }
    }

    fn entity(&self) -> WorldEntity {
        self.coord
            .0
            .coord()
            .conn
            .db
            .game_world_entity()
            .guid()
            .find(&1)
            .unwrap()
    }

    fn aura(&self) -> Aura {
        self.coord
            .world_view()
            .auras
            .on_target(0, 1)
            .into_iter()
            .next()
            .unwrap()
    }

    fn viewer(&self, guid: u64) -> (Arc<Viewer>, Receiver<Outbound>) {
        let (viewer, rx) = test_support::registered_viewer(&self.coord.world_view(), 0, guid);
        viewer.created.lock().unwrap().insert(1);
        (viewer, rx)
    }

    fn after_callbacks(&self) {
        self.coord.0.on_pump(|_| {}).unwrap();
    }
}

fn drain(viewer: &Viewer, rx: &Receiver<Outbound>) -> (usize, Vec<Outbound>) {
    let mut jobs = 0;
    let mut packets = Vec::new();
    for outbound in rx.try_iter() {
        viewer.tx.depth_handle().fetch_sub(1, Ordering::Relaxed);
        match outbound {
            Outbound::Job(job) | Outbound::Motion(job) => {
                jobs += 1;
                packets.extend(job());
            }
            packet => packets.push(packet),
        }
    }
    (jobs, packets)
}

#[test]
#[ignore = "requires the SpacetimeDB 2.7.1 CLI and Wasm toolchain"]
fn committed_timer_and_position_updates_do_not_queue_observer_work() {
    let realm = ObserverRealm::start();
    let (owner, owner_rx) = realm.viewer(1);
    let (peer, peer_rx) = realm.viewer(2);

    realm.node.assert_sql("UPDATE game_aura SET next_tick_micros = 123, amount_remaining = 9, proc_ready_micros = 456 WHERE target_guid = 1");
    assert!(poll_until(POLL_TIMEOUT, || realm.aura().next_tick_micros == 123));
    realm
        .node
        .assert_sql("UPDATE game_world_entity SET x = 1, y = 1, last_move_ms = 123 WHERE guid = 1");
    assert!(poll_until(POLL_TIMEOUT, || realm.entity().last_move_ms == 123));
    realm.after_callbacks();
    assert_eq!(drain(&owner, &owner_rx).0, 0);
    assert_eq!(drain(&peer, &peer_rx).0, 0);

    realm
        .node
        .assert_sql("UPDATE game_aura SET level = 2 WHERE target_guid = 1");
    assert!(poll_until(POLL_TIMEOUT, || realm.aura().level == 2));
    realm.after_callbacks();
    for (viewer, rx) in [(&owner, &owner_rx), (&peer, &peer_rx)] {
        let (jobs, packets) = drain(viewer, rx);
        assert_eq!(jobs, 1);
        assert!(matches!(
            packets.as_slice(),
            [Outbound::Raw { opcode: 0x00a9, .. }]
        ));
    }
}

#[test]
#[ignore = "requires the SpacetimeDB 2.7.1 CLI and Wasm toolchain"]
fn movement_reentry_stays_ordered_after_a_queued_destroy() {
    let realm = ObserverRealm::start();
    let view = realm.coord.world_view();
    let (peer, rx) = realm.viewer(2);
    let old = realm.entity();
    let mut outside = old.clone();
    outside.grid_x = 10;
    outside.x = 500.0;
    entity_changed(&view, 0, &old, &outside);
    entity_changed(&view, 0, &outside, &old);

    let (jobs, packets) = drain(&peer, &rx);
    assert_eq!(jobs, 2);
    assert!(matches!(
        packets.first(),
        Some(Outbound::One(ServerOpcodeMessage::SMSG_DESTROY_OBJECT(_)))
    ));
    assert!(matches!(
        packets.get(1),
        Some(Outbound::One(ServerOpcodeMessage::SMSG_UPDATE_OBJECT(_)))
    ));
    assert!(peer.created.lock().unwrap().contains(&1));
}

#[test]
#[ignore = "requires the SpacetimeDB 2.7.1 CLI and Wasm toolchain"]
fn a_cell_crossing_queues_work_only_for_viewers_entering_the_audience() {
    let realm = ObserverRealm::start();
    let view = realm.coord.world_view();
    let (nearby, nearby_rx) = realm.viewer(2);
    let (distant, distant_rx) =
        test_support::registered_viewer_at(&view, 0, 3, CellKey::at(0, 0, 3, 0));
    distant.created.lock().unwrap().insert(1);
    let old = realm.entity();
    let mut crossed = old.clone();
    crossed.grid_x = 1;
    crossed.x = 60.0;

    entity_changed(&view, 0, &old, &crossed);

    assert_eq!(drain(&nearby, &nearby_rx).0, 0);
    assert_eq!(drain(&distant, &distant_rx).0, 1);
}

#[test]
#[ignore = "requires the SpacetimeDB 2.7.1 CLI and Wasm toolchain"]
fn a_moving_entity_offers_its_create_only_to_unshown_viewers_in_range() {
    let realm = ObserverRealm::start();
    let view = realm.coord.world_view();
    let (near, near_rx) = test_support::registered_viewer(&view, 0, 2);
    let (distant, distant_rx) =
        test_support::registered_viewer_at(&view, 0, 3, CellKey::at(0, 0, 9, 0));
    let entity = realm.entity();
    let row = crate::stdb::bindings::EntityMotion {
        guid: entity.guid,
        map_id: entity.map_id,
        instance_id: entity.instance_id,
        grid_x: entity.grid_x,
        grid_y: entity.grid_y,
        opcode: 0x00ee,
        movement_info: vec![],
        seq: 1,
        cell: entity.cell,
    };

    crate::codec::movement::subscriptions::motion(&view, 0, &row, &Default::default());
    crate::codec::movement::subscriptions::flush_motion(&view, 0);
    drain(&near, &near_rx);

    assert!(near.created.lock().unwrap().contains(&entity.guid));
    assert_eq!(drain(&distant, &distant_rx).0, 0);
}

#[test]
#[ignore = "requires the SpacetimeDB 2.7.1 CLI and Wasm toolchain"]
fn health_changes_still_reach_each_observer() {
    let realm = ObserverRealm::start();
    let view = realm.coord.world_view();
    let (owner, owner_rx) = realm.viewer(1);
    let (peer, peer_rx) = realm.viewer(2);
    let old = realm.entity();
    let mut new = old.clone();
    new.health -= 1;
    entity_changed(&view, 0, &old, &new);
    for (viewer, rx) in [(&owner, &owner_rx), (&peer, &peer_rx)] {
        let (_, packets) = drain(viewer, rx);
        let [Outbound::One(packet)] = packets.as_slice() else {
            panic!("health VALUES");
        };
        assert_eq!(
            *packet,
            ServerOpcodeMessage::SMSG_UPDATE_OBJECT(Box::new(crate::codec::build_health_values(
                1, new.health
            )))
        );
    }
}

#[test]
#[ignore = "requires the SpacetimeDB 2.7.1 CLI and Wasm toolchain"]
fn death_without_descriptor_changes_still_roots_the_owner() {
    let realm = ObserverRealm::start();
    let view = realm.coord.world_view();
    let (owner, owner_rx) = realm.viewer(1);
    let (peer, peer_rx) = realm.viewer(2);
    let old = realm.entity();
    let mut new = old.clone();
    new.dead = true;
    entity_changed(&view, 0, &old, &new);
    let (_, packets) = drain(&owner, &owner_rx);
    let (opcode, body) = crate::codec::build_force_move_root_raw(1, true);
    assert!(
        matches!(packets.as_slice(), [Outbound::Raw { opcode: actual, body: bytes }] if *actual == opcode && *bytes == body)
    );
    assert_eq!(drain(&peer, &peer_rx).0, 0);
}

#[test]
#[ignore = "requires the SpacetimeDB 2.7.1 CLI and Wasm toolchain"]
fn aura_speed_changes_reach_the_owner_and_observers() {
    let realm = ObserverRealm::start();
    let view = realm.coord.world_view();
    let (owner, owner_rx) = realm.viewer(1);
    let (peer, peer_rx) = realm.viewer(2);
    let before = realm.aura();
    let mut after = before.clone();
    after.eff_kind = lyracore_shared::spell::taxonomy::A_MOD_SPEED;
    after.eff_p_0 = i32::from(lyracore_shared::spell::taxonomy::SPEED_MOVE);
    after.amount = 50;
    aura_updated(&view, 0, &before, &after);
    let (_, packets) = drain(&owner, &owner_rx);
    assert!(packets.iter().any(|packet| matches!(packet, Outbound::One(ServerOpcodeMessage::SMSG_FORCE_RUN_SPEED_CHANGE(speed)) if speed.speed == 10.5)));
    let (_, packets) = drain(&peer, &peer_rx);
    let expected = crate::codec::build_spline_speed(1, crate::codec::ObservedSpeed::Run, 10.5);
    assert!(packets
        .iter()
        .any(|packet| matches!(packet, Outbound::One(message) if *message == expected)));
}

#[test]
#[ignore = "requires the SpacetimeDB 2.7.1 CLI and Wasm toolchain"]
fn loot_projection_keeps_each_observers_entitlement() {
    let realm = ObserverRealm::start();
    let view = realm.coord.world_view();
    let (eligible, eligible_rx) = realm.viewer(2);
    let (stranger, stranger_rx) = realm.viewer(3);
    realm.node.assert_sql(
        "INSERT INTO game_corpse_loot_eligible (id, corpse_guid, eligible_guid) VALUES (1, 1, 2)",
    );
    assert!(poll_until(POLL_TIMEOUT, || realm
        .coord
        .0
        .coord()
        .conn
        .db
        .game_corpse_loot_eligible()
        .id()
        .find(&1)
        .is_some()));

    let mut old = realm.entity();
    old.dead = true;
    old.dynamic_flags = 0;
    let mut new = old.clone();
    new.dynamic_flags = lyracore_shared::constants::unit_dynamic_flags::LOOTABLE;
    entity_changed(&view, 0, &old, &new);
    for (viewer, rx, flags) in [
        (&eligible, &eligible_rx, new.dynamic_flags),
        (&stranger, &stranger_rx, 0),
    ] {
        let (_, packets) = drain(viewer, rx);
        let [Outbound::One(packet)] = packets.as_slice() else {
            panic!("loot flags VALUES");
        };
        assert_eq!(
            *packet,
            ServerOpcodeMessage::SMSG_UPDATE_OBJECT(Box::new(
                crate::codec::build_dynamic_flags_values(1, flags)
            ))
        );
    }
}
