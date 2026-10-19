use super::*;
use crate::stdb::world_index::CellKey;
use crate::stdb::world_view::test_support::{identity, queued_job, teleport, viewer_with_tx};
use crate::stdb::world_view::WorldView;
use crate::world::{Outbound, SessionTx};
use wow_world_messages::vanilla::opcodes::ServerOpcodeMessage;

#[test]
fn creature_observers_share_encoded_bodies_after_the_create_gate() {
    let view = Arc::new(WorldView::new(true));
    let mut receivers = Vec::new();
    for byte in 1u8..=3 {
        let id = u64::from(byte);
        let (tx, rx) = SessionTx::with_depth(0);
        let viewer = viewer_with_tx(id, id, identity(byte), tx);
        if id != 3 {
            viewer.created.lock().unwrap().insert(99);
        }
        view.add_viewer_on_shard(viewer, CellKey::at(0, 0, 0, 0), 0);
        receivers.push(rx);
    }
    let row = super::packet_tests::creature_spline(99, 0, 1000);
    creature_leg(&view, 0, &row);
    flush_motion(&view, 0);
    let first = queued_job(&receivers[0]);
    let second = queued_job(&receivers[1]);
    assert!(queued_job(&receivers[2]).is_empty());
    let (
        Outbound::Shared {
            opcode: first_opcode,
            body: first_body,
        },
        Outbound::Shared {
            opcode: second_opcode,
            body: second_body,
        },
    ) = (&first[0], &second[0])
    else {
        panic!("both observers must receive shared packet bodies");
    };
    assert_eq!(first_opcode, second_opcode);
    assert!(Arc::ptr_eq(first_body, second_body));
}

#[test]
fn a_viewer_receives_only_staged_rows_inside_its_audience_box() {
    let view = Arc::new(WorldView::new(true));
    let (tx, rx) = SessionTx::with_depth(0);
    let viewer = viewer_with_tx(1, 1, identity(1), tx);
    viewer.created.lock().unwrap().extend([98, 99]);
    view.add_viewer_on_shard(viewer, CellKey::at(0, 0, 0, 0), 0);
    let near = super::packet_tests::creature_spline(99, 0, 1000);
    let mut far = super::packet_tests::creature_spline(98, 0, 1000);
    far.grid_x = 6;

    creature_leg(&view, 0, &near);
    creature_leg(&view, 0, &far);
    flush_motion(&view, 0);

    let packets = queued_job(&rx);
    assert_eq!(packets.len(), 1, "only the nearby creature's path is sent");
    assert!(
        rx.try_recv().is_err(),
        "one flush for the whole SDK message"
    );
}

fn newest_path_after(fold_first: bool) {
    let view = Arc::new(WorldView::new(true));
    let (tx, rx) = SessionTx::with_depth(0);
    let viewer = viewer_with_tx(1, 1, identity(1), tx);
    viewer.created.lock().unwrap().insert(99);
    view.add_viewer_on_shard(viewer.clone(), CellKey::at(0, 0, 0, 0), 0);
    let older = super::packet_tests::creature_spline(99, 0, 1000);
    let mut newer = older.clone();
    newer.spline_id = 11;

    creature_leg(&view, 0, &older);
    flush_motion(&view, 0);
    if fold_first {
        assert!(fold_batches(&viewer), "a shed folds the waiting batch");
    }
    creature_leg(&view, 0, &newer);
    flush_motion(&view, 0);

    let packets = queued_job(&rx);
    assert!(rx.try_recv().is_err(), "one flush is queued");
    assert!(viewer.motion_pending.creature.lock().unwrap().is_empty());
    let expected = crate::world::egress::encode(creature_spline_outbound(&newer)).unwrap();
    let (
        [Outbound::Shared { body, .. }],
        Outbound::Shared {
            body: newest_body, ..
        },
    ) = (packets.as_slice(), expected[0].outbound())
    else {
        panic!("one path for the mover");
    };
    assert_eq!(*body, newest_body);
}

#[test]
fn a_late_flush_relays_only_the_newest_path_of_each_mover() {
    newest_path_after(false);
}

#[test]
fn a_flush_after_a_shed_relays_only_the_newest_path_of_each_mover() {
    newest_path_after(true);
}

#[test]
fn a_flush_never_echoes_a_characters_own_movement() {
    let view = Arc::new(WorldView::new(true));
    let (tx, rx) = SessionTx::with_depth(0);
    let viewer = viewer_with_tx(1, 7, identity(1), tx);
    view.add_viewer_on_shard(viewer.clone(), CellKey::at(0, 0, 0, 0), 0);
    let delivery = crate::stdb::movement_batch::MotionDelivery::default();
    for guid in [7, 8] {
        let row = EntityMotion {
            guid,
            map_id: 0,
            instance_id: 0,
            grid_x: 0,
            grid_y: 0,
            cell: lyracore_shared::spatial::grid_cell_id(0, 0),
            opcode: 0x00ee,
            movement_info: Vec::new(),
            seq: 0,
        };
        motion(&view, 0, &row, &delivery);
    }
    flush_motion(&view, 0);

    let Ok(Outbound::Motion(flush)) = rx.try_recv() else {
        panic!("one movement flush per SDK message");
    };
    assert!(flush().is_empty());
    let offers = viewer.motion_pending.create_offers.lock().unwrap();
    assert!(offers.contains_key(&8), "the unshown peer is offered");
    assert!(
        !offers.contains_key(&7),
        "the viewer's own Character is skipped"
    );
}

#[test]
fn a_nearby_mover_the_viewer_does_not_show_gets_one_create_offer_per_interval() {
    let view = Arc::new(WorldView::new(true));
    let (tx, rx) = SessionTx::with_depth(0);
    let viewer = viewer_with_tx(1, 1, identity(1), tx);
    view.add_viewer_on_shard(viewer, CellKey::at(0, 0, 0, 0), 0);
    let row = super::packet_tests::creature_spline(99, 0, 1000);

    let mut offers = 0;
    for _ in 0..2 {
        creature_leg(&view, 0, &row);
        flush_motion(&view, 0);
        let Ok(Outbound::Motion(flush)) = rx.try_recv() else {
            panic!("one movement flush per SDK message");
        };
        assert!(flush().is_empty(), "no path for an unshown creature");
        while let Ok(Outbound::Job(_)) = rx.try_recv() {
            offers += 1;
        }
    }

    assert_eq!(offers, 1, "the second flush waits for the interval");
}

#[test]
fn pending_motion_replaces_existing_rows_without_spending_the_budget_twice() {
    let mut pending = MotionRows::default();
    let row = Arc::new(SharedMotion::new((), |_| Vec::new()));
    let bytes = 1536 * 1024;
    assert!(pending.insert(1, row.clone(), bytes));
    assert!(pending.insert(1, row.clone(), bytes));
    assert!(!pending.insert(2, row.clone(), bytes));
    assert_eq!(pending.drain().len(), 1);
    assert!(pending.insert(2, row, bytes));
}

#[test]
fn teleport_routes_one_writer_job_to_its_owner_and_keeps_cross_map_order() {
    let view = WorldView::new(true);
    let (owner_tx, owner_rx) = SessionTx::with_depth(0);
    let (other_tx, other_rx) = SessionTx::with_depth(0);
    let owner = viewer_with_tx(1, 9001, identity(1), owner_tx);
    let other = viewer_with_tx(2, 9002, identity(2), other_tx);
    let anchor = CellKey::at(0, 0, 0, 0);
    view.add_viewer_on_shard(owner.clone(), anchor, 0);
    view.add_viewer_on_shard(other, anchor, 0);

    teleport_appeared(&view, 0, &teleport(owner.self_guid));

    assert!(
        other_rx.try_recv().is_err(),
        "an unrelated viewer receives nothing"
    );
    let out = queued_job(&owner_rx);
    assert_eq!(out.len(), 1, "one teleport row creates one writer result");
    match &out[0] {
        Outbound::Batch(messages) => {
            assert!(matches!(
                messages.as_slice(),
                [
                    ServerOpcodeMessage::SMSG_TRANSFER_PENDING(_),
                    ServerOpcodeMessage::SMSG_NEW_WORLD(_)
                ]
            ));
        }
        _ => panic!("cross-map teleport must remain one ordered writer batch"),
    }
    assert!(
        owner_rx.try_recv().is_err(),
        "the owner receives exactly one job"
    );
}

#[test]
fn a_fan_out_of_waiting_batches_relays_only_the_newest_path() {
    let view = Arc::new(WorldView::new(true));
    let (tx, rx) = SessionTx::with_depth(0);
    let viewer = viewer_with_tx(1, 1, identity(1), tx);
    viewer.created.lock().unwrap().insert(99);
    view.add_viewer_on_shard(viewer, CellKey::at(0, 0, 0, 0), 0);
    let older = super::packet_tests::creature_spline(99, 0, 1000);
    let mut newer = older.clone();
    newer.spline_id = 11;

    creature_leg(&view, 0, &older);
    let first = take_staged(&view, 0).unwrap();
    creature_leg(&view, 0, &newer);
    let second = take_staged(&view, 0).unwrap();
    fan_out(&view, 0, [first, second]);

    let packets = queued_job(&rx);
    assert!(rx.try_recv().is_err(), "one flush for both batches");
    let expected = crate::world::egress::encode(creature_spline_outbound(&newer)).unwrap();
    let ([Outbound::Shared { body, .. }], Outbound::Shared { body: newest, .. }) =
        (packets.as_slice(), expected[0].outbound())
    else {
        panic!("one path for the mover");
    };
    assert_eq!(*body, newest);
}

#[test]
fn the_after_message_hook_hands_staged_rows_to_the_fan_out_thread() {
    let view = Arc::new(WorldView::new(true));
    let (tx, rx) = SessionTx::with_depth(0);
    let viewer = viewer_with_tx(1, 1, identity(1), tx);
    viewer.created.lock().unwrap().insert(99);
    view.add_viewer_on_shard(viewer, CellKey::at(0, 0, 0, 0), 0);
    let hook = motion_fanout(
        view.clone(),
        0,
        crate::stage_timing::pump_timing("fan-out test"),
    )
    .unwrap();

    creature_leg(&view, 0, &super::packet_tests::creature_spline(99, 0, 1000));
    hook();

    let flush = rx
        .recv_timeout(std::time::Duration::from_secs(5))
        .expect("the fan-out thread queues a flush");
    let Outbound::Motion(flush) = flush else {
        panic!("a movement flush");
    };
    assert_eq!(flush().len(), 1);
}

#[test]
fn a_fan_out_pass_takes_at_most_a_backlog_of_waiting_batches() {
    let (send, batches) = std::sync::mpsc::sync_channel(3 * FANOUT_BACKLOG);
    for batch in 0..3 * FANOUT_BACKLOG {
        send.send(batch).unwrap();
    }

    let pass = next_pass(&batches).unwrap();

    assert_eq!(pass, (0..=FANOUT_BACKLOG).collect::<Vec<_>>());
}

#[test]
fn dropping_the_after_message_hook_drains_the_fan_out_thread() {
    let view = Arc::new(WorldView::new(true));
    let (tx, rx) = SessionTx::with_depth(0);
    let viewer = viewer_with_tx(1, 1, identity(1), tx);
    viewer.created.lock().unwrap().insert(99);
    view.add_viewer_on_shard(viewer, CellKey::at(0, 0, 0, 0), 0);
    let hook = motion_fanout(
        view.clone(),
        0,
        crate::stage_timing::pump_timing("fan-out drain test"),
    )
    .unwrap();

    creature_leg(&view, 0, &super::packet_tests::creature_spline(99, 0, 1000));
    hook();
    drop(hook);

    assert!(
        matches!(rx.try_recv(), Ok(Outbound::Motion(_))),
        "the flush is queued before the hook finishes dropping"
    );
}
