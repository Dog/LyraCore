use super::*;
use crate::stdb::world_index::CellKey;
use crate::stdb::world_view::test_support::{identity, queued_job, teleport, viewer_with_tx};
use crate::stdb::world_view::WorldView;
use crate::world::{Outbound, SessionTx};
use wow_world_messages::vanilla::opcodes::ServerOpcodeMessage;

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
