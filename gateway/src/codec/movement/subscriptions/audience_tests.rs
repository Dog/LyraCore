use super::*;
use crate::stdb::world_index::CellKey;
use crate::stdb::world_view::test_support::{identity, queued_job, teleport, viewer_with_tx};
use crate::stdb::world_view::WorldView;
use crate::world::{Outbound, SessionTx};
use wow_world_messages::vanilla::opcodes::ServerOpcodeMessage;

#[test]
fn creature_observers_share_encoded_bodies_after_the_create_gate() {
    let view = WorldView::new(true);
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
fn pending_motion_replaces_existing_rows_without_spending_the_budget_twice() {
    let mut pending = MotionRows::default();
    let row = Arc::new(SharedMotion::new(()));
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
