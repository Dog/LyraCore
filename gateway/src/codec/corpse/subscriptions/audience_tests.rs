use super::*;
use crate::stdb::world_index::CellKey;
use crate::stdb::world_view::test_support::{identity, queued_job, viewer_with_tx};
use crate::stdb::world_view::WorldView;
use crate::world::{Outbound, SessionTx};
use wow_world_messages::vanilla::opcodes::ServerOpcodeMessage;

fn self_res_spell_of(outbound: Vec<Outbound>) -> Option<i32> {
    use wow_world_messages::vanilla::{Object, UpdateMask};
    let [Outbound::One(ServerOpcodeMessage::SMSG_UPDATE_OBJECT(packet))] = outbound.as_slice()
    else {
        panic!("the owner must receive one SMSG_UPDATE_OBJECT");
    };
    match packet.objects.as_slice() {
        [Object::Values {
            mask1: UpdateMask::Player(player),
            ..
        }] => player.player_self_res_spell(),
        other => panic!("expected one Player VALUES block, got {other:?}"),
    }
}

#[test]
fn a_self_resurrect_option_reaches_only_its_character_on_this_shard() {
    let view = WorldView::new(true);
    let anchor = CellKey::at(0, 0, 0, 0);
    let (owner_tx, owner_rx) = SessionTx::with_depth(0);
    let (bystander_tx, bystander_rx) = SessionTx::with_depth(0);
    let (remote_tx, remote_rx) = SessionTx::with_depth(0);
    view.add_viewer_on_shard(viewer_with_tx(1, 9001, identity(1), owner_tx), anchor, 0);
    view.add_viewer_on_shard(
        viewer_with_tx(2, 9002, identity(2), bystander_tx),
        anchor,
        0,
    );
    view.add_viewer_on_shard(viewer_with_tx(3, 9003, identity(3), remote_tx), anchor, 1);

    self_res_option_changed(&view, 0, 9001, 3026);
    assert_eq!(self_res_spell_of(queued_job(&owner_rx)), Some(3026));
    assert!(owner_rx.try_recv().is_err(), "one change queues one job");
    assert!(
        bystander_rx.try_recv().is_err(),
        "a nearby Character never sees another Character's option"
    );

    self_res_option_changed(&view, 0, 9003, 3026);
    assert!(
        remote_rx.try_recv().is_err(),
        "a Character on another Shard is not this Shard's to address"
    );

    self_res_option_changed(&view, 0, 9001, 0);
    assert_eq!(self_res_spell_of(queued_job(&owner_rx)), Some(0));
}
