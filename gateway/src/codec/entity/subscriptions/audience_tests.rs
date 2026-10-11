use super::*;
use crate::stdb::bindings::PlayerReputation;
use crate::stdb::world_index::CellKey;
use crate::stdb::world_view::test_support::{identity, queued_job, viewer_with_tx};
use crate::stdb::world_view::WorldView;
use crate::world::{Outbound, SessionTx};

fn reputation(character_guid: u64, reputation_index: i32, standing: i32) -> PlayerReputation {
    PlayerReputation {
        id: 1,
        character_guid,
        owner_identity: identity(1),
        faction_id: 72,
        standing,
        reputation_index,
        at_war: false,
    }
}

#[test]
fn reputation_insert_and_update_route_only_to_the_owner_by_reputation_index() {
    let view = WorldView::new(true);
    let (owner_tx, owner_rx) = SessionTx::with_depth(0);
    let (other_tx, other_rx) = SessionTx::with_depth(0);
    let owner = viewer_with_tx(1, 9001, identity(1), owner_tx);
    let other = viewer_with_tx(2, 9002, identity(2), other_tx);
    let anchor = CellKey::at(0, 0, 0, 0);
    view.add_viewer_on_shard(owner, anchor, 0);
    view.add_viewer_on_shard(other, anchor, 0);

    reputation_appeared(&view, 0, &reputation(9001, 19, 3175));
    reputation_appeared(&view, 0, &reputation(9001, 19, 3200));

    assert!(
        other_rx.try_recv().is_err(),
        "an unrelated owner receives nothing"
    );
    for standing in [3175, 3200] {
        let (opcode, body) = crate::codec::build_set_faction_standing_raw(19, standing)
            .expect("a valid reputation index has a packet");
        match queued_job(&owner_rx).as_slice() {
            [Outbound::Raw {
                opcode: actual_opcode,
                body: actual_body,
            }] => {
                assert_eq!((*actual_opcode, actual_body), (opcode, &body));
            }
            _ => panic!("the reputation row must use its reputation_index, not faction_id"),
        }
    }
    assert!(
        owner_rx.try_recv().is_err(),
        "each reputation row queues exactly one job"
    );
}
