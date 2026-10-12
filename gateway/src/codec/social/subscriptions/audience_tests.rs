use super::*;
use crate::stdb::bindings::{InstanceRemoval, SystemMessageEvent};
use crate::stdb::world_index::CellKey;
use crate::stdb::world_view::test_support::{identity, queued_job, viewer_with_tx};
use crate::stdb::world_view::WorldView;
use crate::world::{Outbound, SessionTx};
use std::sync::Arc;
use wow_world_messages::vanilla::opcodes::ServerOpcodeMessage;

#[test]
fn a_system_message_reaches_only_its_recipient_and_missing_recipients_are_dropped() {
    let view = WorldView::new(true);
    let (recipient_tx, recipient_rx) = SessionTx::with_depth(0);
    let recipient = viewer_with_tx(1, 9001, identity(1), recipient_tx);
    let (bystander_tx, bystander_rx) = SessionTx::with_depth(0);
    let bystander = viewer_with_tx(2, 9002, identity(2), bystander_tx);
    let anchor = CellKey::at(0, 0, 0, 0);
    view.add_viewer_on_shard(recipient.clone(), anchor, 0);
    view.add_viewer_on_shard(bystander, anchor, 0);

    let row = SystemMessageEvent {
        id: 1,
        recipient_identity: recipient.bound_identity,
        recipient_guid: recipient.self_guid,
        message: "Package loaded".to_string(),
        created_at: spacetimedb_sdk::Timestamp::UNIX_EPOCH,
    };
    system_message_appeared(&view, &row);

    let outbound = queued_job(&recipient_rx);
    let [Outbound::One(ServerOpcodeMessage::SMSG_MESSAGECHAT(actual))] = outbound.as_slice() else {
        panic!("the recipient must receive one System Message packet");
    };
    assert_eq!(
        **actual,
        crate::codec::build_gm_system_message(row.message.clone())
    );
    assert!(
        bystander_rx.try_recv().is_err(),
        "a second Character must receive no queued work"
    );

    let missing = SystemMessageEvent {
        recipient_guid: 9999,
        ..row
    };
    system_message_appeared(&view, &missing);
    assert!(recipient_rx.try_recv().is_err());
    assert!(bystander_rx.try_recv().is_err());
}

fn at_micros(micros: i64) -> spacetimedb_sdk::ScheduleAt {
    spacetimedb_sdk::ScheduleAt::Time(spacetimedb_sdk::Timestamp::from_micros_since_unix_epoch(
        micros,
    ))
}

/// The one `SMSG_RAID_GROUP_ONLY` timer a writer job produced.
fn raid_group_only_timer(out: &[Outbound]) -> u32 {
    match out {
        [Outbound::One(ServerOpcodeMessage::SMSG_RAID_GROUP_ONLY(packet))] => {
            assert_eq!(
                packet.error,
                wow_world_messages::vanilla::RaidGroupError::Required
            );
            packet.homebind_timer
        }
        _ => panic!("expected one SMSG_RAID_GROUP_ONLY"),
    }
}

#[test]
fn an_instance_removal_shows_and_hides_the_countdown_for_its_owner_only() {
    let view = Arc::new(WorldView::new(true));
    let (owner_tx, owner_rx) = SessionTx::with_depth(0);
    let (other_tx, other_rx) = SessionTx::with_depth(0);
    let owner = viewer_with_tx(1, 9001, identity(1), owner_tx);
    let other = viewer_with_tx(2, 9002, identity(2), other_tx);
    let anchor = CellKey::at(36, 7, 0, 0);
    view.add_viewer_on_shard(owner.clone(), anchor, 1);
    view.add_viewer_on_shard(other, anchor, 1);
    // Due in 2100: far enough ahead that the time left is always the full countdown.
    let row = InstanceRemoval {
        scheduled_id: 3,
        scheduled_at: at_micros(4_102_444_800_000_000),
        character_guid: owner.self_guid,
        instance_id: 7,
        group_id: 42,
    };

    instance_removal_started(&view, 0, &row);
    assert!(owner_rx.try_recv().is_err(), "another Shard's row");
    instance_removal_started(&view, 1, &row);
    assert_eq!(raid_group_only_timer(&queued_job(&owner_rx)), 60_000);
    assert!(view.countdown_shown(owner.self_guid));
    instance_removal_ended(&view, 1, &row);
    assert_eq!(raid_group_only_timer(&queued_job(&owner_rx)), 0);
    assert!(!view.countdown_shown(owner.self_guid));
    assert!(owner_rx.try_recv().is_err(), "one job per row change");
    assert!(
        other_rx.try_recv().is_err(),
        "an unrelated viewer receives nothing"
    );
}

/// A world-port drops the source viewer before the source Shard deletes the row, so the delete
/// reaches nobody. The countdown the client still shows is hidden at the next world entry.
#[test]
fn a_world_entry_hides_a_countdown_whose_row_is_gone() {
    let view = Arc::new(WorldView::new(true));
    let (tx, rx) = SessionTx::with_depth(0);
    let source = viewer_with_tx(1, 9001, identity(1), tx);
    view.add_viewer_on_shard(source.clone(), CellKey::at(36, 7, 0, 0), 1);
    let row = InstanceRemoval {
        scheduled_id: 3,
        scheduled_at: at_micros(4_102_444_800_000_000),
        character_guid: source.self_guid,
        instance_id: 7,
        group_id: 42,
    };
    instance_removal_started(&view, 1, &row);
    queued_job(&rx);
    view.remove_viewer(source.session);
    instance_removal_ended(&view, 1, &row);

    let shown = view.countdown_shown(9001);
    assert_eq!(resident_countdown_ms(None, shown, 0), Some(0));
    assert_eq!(resident_countdown_ms(None, false, 0), None);
    assert_eq!(
        resident_countdown_ms(
            Some(&at_micros(1_900_000_000_000_000)),
            false,
            1_899_999_955_000_000
        ),
        Some(45_000)
    );
}

/// The world-entry sweep and a replayed insert both show the time left, not a fresh 60 s.
#[test]
fn the_countdown_shows_the_time_left_rounded_up_and_nothing_once_due() {
    let due = 1_900_000_000_000_000;
    assert_eq!(
        instance_removal_time_left_ms(&at_micros(due), 1_899_999_955_000_000),
        Some(45_000)
    );
    assert_eq!(
        instance_removal_time_left_ms(&at_micros(due), 1_899_999_999_999_999),
        Some(1)
    );
    assert_eq!(
        instance_removal_time_left_ms(&at_micros(due), 1_899_999_900_000_000),
        Some(60_000),
        "a Gateway clock behind the Module's shows no more than the full countdown"
    );
    assert_eq!(
        instance_removal_time_left_ms(&at_micros(due), 1_900_000_000_000_000),
        None
    );
    assert_eq!(
        instance_removal_time_left_ms(&at_micros(due), 1_900_000_000_000_005),
        None
    );
    let interval = spacetimedb_sdk::ScheduleAt::Interval(
        spacetimedb_sdk::TimeDuration::from_micros(1_000_000),
    );
    assert_eq!(
        instance_removal_time_left_ms(&interval, 1_899_999_955_000_000),
        None
    );
}
