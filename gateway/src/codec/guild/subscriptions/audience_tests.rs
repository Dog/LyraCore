use super::*;
use crate::stdb::bindings::{GuildEvent, GuildPetition};
use crate::stdb::world_index::{CellKey, EntityLayer};
use crate::stdb::world_view::test_support::{identity, queued_job, viewer_with_tx};
use crate::stdb::world_view::{
    GuildMembershipRead, GuildRosterSnapshotRead, PetitionRead, WorldView,
};
use crate::world::{GuildEventSnapshot, Outbound, SessionTx};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc::Receiver;
use std::sync::Arc;
use wow_world_messages::vanilla::opcodes::ServerOpcodeMessage;

fn raw_packets(outbound: Vec<Outbound>) -> Vec<(u16, Vec<u8>)> {
    outbound
        .into_iter()
        .map(|out| match out {
            Outbound::Raw { opcode, body } => (opcode, body),
            _ => panic!("guild relays write raw packets"),
        })
        .collect()
}

fn membership(rows: &[(u64, u32, u32)]) -> GuildMembershipRead {
    let rows = rows.to_vec();
    Arc::new(move |guid| {
        rows.iter()
            .find(|(member, _, _)| *member == guid)
            .map(|(_, guild_id, rank_id)| (*guild_id, *rank_id))
    })
}

fn unreachable_snapshot_read() -> GuildRosterSnapshotRead {
    Arc::new(|_guild_id, _kind| panic!("this Guild Event kind must not read a snapshot"))
}

fn counting_snapshot_read(
    snapshot: GuildEventSnapshot,
) -> (GuildRosterSnapshotRead, Arc<AtomicU32>) {
    let reads = Arc::new(AtomicU32::new(0));
    let recorded = reads.clone();
    let read: GuildRosterSnapshotRead = Arc::new(move |_guild_id, _kind| {
        recorded.fetch_add(1, Ordering::Relaxed);
        snapshot.clone()
    });
    (read, reads)
}

fn roster_variant(officer_note: &str) -> ServerOpcodeMessage {
    let guild = crate::codec::GuildView {
        guild_id: 7,
        name: "Knights".into(),
        ..Default::default()
    };
    let line = crate::codec::GuildRosterLine {
        guid: 9001,
        name: "Alice".into(),
        officer_note: officer_note.into(),
        ..Default::default()
    };
    ServerOpcodeMessage::SMSG_GUILD_ROSTER(Box::new(crate::codec::build_guild_roster(
        &guild,
        [line],
    )))
}

#[test]
fn a_membership_change_reaches_the_member_and_viewers_that_created_it() {
    let view = WorldView::new(true);
    let anchor = CellKey::at(0, 0, 0, 0);
    let (member_tx, member_rx) = SessionTx::with_depth(0);
    let member = viewer_with_tx(1, 9001, identity(1), member_tx);
    let (observer_tx, observer_rx) = SessionTx::with_depth(0);
    let observer = viewer_with_tx(2, 9002, identity(2), observer_tx);
    observer.created.lock().unwrap().insert(9001);
    let (stranger_tx, stranger_rx) = SessionTx::with_depth(0);
    let stranger = viewer_with_tx(3, 9003, identity(3), stranger_tx);
    for viewer in [&member, &observer, &stranger] {
        view.add_viewer_on_shard(viewer.clone(), anchor, 0);
    }
    view.spatial
        .upsert_entity(EntityLayer::WorldEntity, 9001, anchor, 0, 0);

    guild_membership_changed(&view, &membership(&[(9001, 7, 0)]), 9001);
    let founded = crate::codec::build_guild_values(9001, 7, 0);
    assert_eq!(raw_packets(queued_job(&member_rx)), vec![founded.clone()]);
    assert_eq!(raw_packets(queued_job(&observer_rx)), vec![founded]);
    assert!(queued_job(&stranger_rx).is_empty());

    guild_membership_changed(&view, &membership(&[]), 9001);
    assert_eq!(
        raw_packets(queued_job(&member_rx)),
        vec![crate::codec::build_guild_values(9001, 0, 0)]
    );
}

#[test]
fn a_removal_reaches_the_member_before_its_entity_is_indexed() {
    let view = WorldView::new(true);
    let anchor = CellKey::at(0, 0, 0, 0);
    let (member_tx, member_rx) = SessionTx::with_depth(0);
    let (other_tx, other_rx) = SessionTx::with_depth(0);
    view.add_viewer_on_shard(viewer_with_tx(1, 9001, identity(1), member_tx), anchor, 0);
    view.add_viewer_on_shard(viewer_with_tx(2, 9002, identity(2), other_tx), anchor, 0);

    guild_membership_changed(&view, &membership(&[]), 9001);
    assert_eq!(
        raw_packets(queued_job(&member_rx)),
        vec![crate::codec::build_guild_values(9001, 0, 0)]
    );
    assert!(other_rx.try_recv().is_err());
}

fn guild_event(recipient_guid: u64, kind: u8, subject_guid: u64) -> GuildEvent {
    GuildEvent {
        id: 1,
        guild_id: 7,
        recipient_guid,
        kind,
        subject_guid,
        other_guid: 0,
        strings: vec!["Alice".to_string()],
        created_at: spacetimedb_sdk::Timestamp::UNIX_EPOCH,
    }
}

#[test]
fn a_broadcast_guild_event_reaches_online_members_and_nobody_else() {
    use lyracore_shared::guild::event_kind;
    let view = WorldView::new(true);
    let anchor = CellKey::at(0, 0, 0, 0);
    let mut receivers = Vec::new();
    for (session, guid) in [(1, 9001), (2, 9002), (3, 9003), (4, 9004)] {
        let (tx, rx) = SessionTx::with_depth(0);
        view.add_viewer_on_shard(
            viewer_with_tx(session, guid, identity(session as u8), tx),
            anchor,
            session as usize,
        );
        receivers.push(rx);
    }
    let [alice, bob, stranger, other_guild] = <[_; 4]>::try_from(receivers).unwrap();
    let members = membership(&[(9001, 7, 0), (9002, 7, 4), (9004, 8, 0)]);
    let snapshot_read = unreachable_snapshot_read();

    guild_event_appeared(
        &view,
        &members,
        &snapshot_read,
        &guild_event(0, event_kind::SIGNED_OFF, 9001),
    );
    let signed_off =
        crate::codec::build_guild_event_raw(event_kind::SIGNED_OFF, &["Alice".to_string()], 9001);
    assert_eq!(raw_packets(queued_job(&alice)), vec![signed_off.clone()]);
    assert_eq!(raw_packets(queued_job(&bob)), vec![signed_off]);
    assert!(stranger.try_recv().is_err());
    assert!(other_guild.try_recv().is_err());

    guild_event_appeared(
        &view,
        &members,
        &snapshot_read,
        &guild_event(0, event_kind::SIGNED_ON, 9001),
    );
    assert!(
        alice.try_recv().is_err(),
        "a member does not hear its own sign-on"
    );
    assert_eq!(raw_packets(queued_job(&bob)).len(), 1);
}

#[test]
fn an_addressed_guild_event_reaches_only_its_recipient() {
    use lyracore_shared::guild::event_kind;
    let view = WorldView::new(true);
    let anchor = CellKey::at(0, 0, 0, 0);
    let (recipient_tx, recipient_rx) = SessionTx::with_depth(0);
    let (member_tx, member_rx) = SessionTx::with_depth(0);
    view.add_viewer_on_shard(
        viewer_with_tx(1, 9003, identity(1), recipient_tx),
        anchor,
        0,
    );
    view.add_viewer_on_shard(viewer_with_tx(2, 9001, identity(2), member_tx), anchor, 0);

    guild_event_appeared(
        &view,
        &membership(&[(9001, 7, 0)]),
        &unreachable_snapshot_read(),
        &guild_event(9003, event_kind::DISBANDED, 9001),
    );
    assert_eq!(raw_packets(queued_job(&recipient_rx)).len(), 1);
    assert!(member_rx.try_recv().is_err());
}

#[test]
fn tabard_changed_reaches_every_online_member_with_the_new_emblem() {
    use lyracore_shared::guild::event_kind;
    let view = WorldView::new(true);
    let anchor = CellKey::at(0, 0, 0, 0);
    let (leader_tx, leader_rx) = SessionTx::with_depth(0);
    let (member_tx, member_rx) = SessionTx::with_depth(0);
    view.add_viewer_on_shard(viewer_with_tx(1, 9001, identity(1), leader_tx), anchor, 0);
    view.add_viewer_on_shard(viewer_with_tx(2, 9002, identity(2), member_tx), anchor, 1);
    let emblem = crate::codec::GuildView {
        guild_id: 7,
        name: "Tabard Guild".into(),
        emblem_style: 11,
        emblem_color: 12,
        border_style: 3,
        border_color: 14,
        background_color: 15,
        ..Default::default()
    };
    let query = ServerOpcodeMessage::SMSG_GUILD_QUERY_RESPONSE(Box::new(
        crate::codec::build_guild_query_response(&emblem),
    ));
    let snapshot = GuildEventSnapshot {
        query: Some(query.clone()),
        ..GuildEventSnapshot::default()
    };
    let (snapshot_read, reads) = counting_snapshot_read(snapshot);
    let tabard_changed = GuildEvent {
        strings: Vec::new(),
        ..guild_event(0, event_kind::TABARD_CHANGED, 0)
    };

    guild_event_appeared(
        &view,
        &membership(&[(9001, 7, 0), (9002, 7, 4)]),
        &snapshot_read,
        &tabard_changed,
    );

    for rx in [&leader_rx, &member_rx] {
        let [Outbound::Raw { opcode, body }, Outbound::One(response)] =
            <[Outbound; 2]>::try_from(queued_job(rx)).ok().unwrap()
        else {
            panic!("TABARD_CHANGED renders the event, then the query response");
        };
        assert_eq!((opcode, body), (0x0092, vec![9, 0]));
        assert_eq!(response, query);
    }
    assert_eq!(
        reads.load(Ordering::Relaxed),
        1,
        "one snapshot read serves every recipient"
    );
}

#[test]
fn roster_to_actor_reaches_only_the_actor_with_the_roster_its_rank_sees() {
    use lyracore_shared::guild::event_kind;
    let view = WorldView::new(true);
    let anchor = CellKey::at(0, 0, 0, 0);
    let (actor_tx, actor_rx) = SessionTx::with_depth(0);
    let (other_tx, other_rx) = SessionTx::with_depth(0);
    view.add_viewer_on_shard(viewer_with_tx(1, 9001, identity(1), actor_tx), anchor, 0);
    view.add_viewer_on_shard(viewer_with_tx(2, 9002, identity(2), other_tx), anchor, 0);
    let shown = roster_variant("secret");
    let snapshot = GuildEventSnapshot {
        roster_hidden: Some(roster_variant("")),
        roster_shown: Some(shown.clone()),
        rank_rights: vec![(0, 0xF_F1FF)],
        ..GuildEventSnapshot::default()
    };
    let (snapshot_read, reads) = counting_snapshot_read(snapshot);

    guild_event_appeared(
        &view,
        &membership(&[(9001, 7, 0), (9002, 7, 0)]),
        &snapshot_read,
        &guild_event(9001, event_kind::ROSTER_TO_ACTOR, 0),
    );

    let [Outbound::One(actual)] = <[Outbound; 1]>::try_from(queued_job(&actor_rx))
        .ok()
        .unwrap()
    else {
        panic!("expected one roster packet");
    };
    assert_eq!(actual, shown, "rank 0 holds VIEWOFFNOTE in this snapshot");
    assert!(other_rx.try_recv().is_err());
    assert_eq!(reads.load(Ordering::Relaxed), 1);
}

#[test]
fn roster_to_actor_with_no_membership_row_sends_nothing() {
    use lyracore_shared::guild::event_kind;
    let view = WorldView::new(true);
    let anchor = CellKey::at(0, 0, 0, 0);
    let (actor_tx, actor_rx) = SessionTx::with_depth(0);
    view.add_viewer_on_shard(viewer_with_tx(1, 9001, identity(1), actor_tx), anchor, 0);

    guild_event_appeared(
        &view,
        &membership(&[]),
        &unreachable_snapshot_read(),
        &guild_event(9001, event_kind::ROSTER_TO_ACTOR, 0),
    );

    assert!(queued_job(&actor_rx).is_empty());
}

#[test]
fn roster_to_actor_with_membership_in_another_guild_sends_nothing() {
    use lyracore_shared::guild::event_kind;
    let view = WorldView::new(true);
    let anchor = CellKey::at(0, 0, 0, 0);
    let (actor_tx, actor_rx) = SessionTx::with_depth(0);
    view.add_viewer_on_shard(viewer_with_tx(1, 9001, identity(1), actor_tx), anchor, 0);

    guild_event_appeared(
        &view,
        &membership(&[(9001, 8, 0)]),
        &unreachable_snapshot_read(),
        &guild_event(9001, event_kind::ROSTER_TO_ACTOR, 0),
    );

    assert!(queued_job(&actor_rx).is_empty());
}

#[test]
fn roster_refresh_gives_each_online_member_the_roster_its_own_rank_sees() {
    use lyracore_shared::guild::event_kind;
    let view = WorldView::new(true);
    let anchor = CellKey::at(0, 0, 0, 0);
    let mut receivers = Vec::new();
    for (session, guid) in [(1, 9001), (2, 9002), (3, 9003)] {
        let (tx, rx) = SessionTx::with_depth(0);
        view.add_viewer_on_shard(
            viewer_with_tx(session, guid, identity(session as u8), tx),
            anchor,
            0,
        );
        receivers.push(rx);
    }
    let [leader, member, stranger] = <[_; 3]>::try_from(receivers).unwrap();
    let hidden = roster_variant("");
    let shown = roster_variant("secret");
    let query = ServerOpcodeMessage::SMSG_GUILD_QUERY_RESPONSE(Box::new(
        crate::codec::build_guild_query_response(&crate::codec::GuildView {
            guild_id: 7,
            ..Default::default()
        }),
    ));
    let snapshot = GuildEventSnapshot {
        query: Some(query),
        roster_hidden: Some(hidden.clone()),
        roster_shown: Some(shown.clone()),
        rank_rights: vec![(0, 0xF_F1FF), (4, 0x43)],
    };
    let (snapshot_read, reads) = counting_snapshot_read(snapshot);

    guild_event_appeared(
        &view,
        &membership(&[(9001, 7, 0), (9002, 7, 4)]),
        &snapshot_read,
        &guild_event(0, event_kind::ROSTER_REFRESH, 0),
    );

    let leader_out = queued_job(&leader);
    let member_out = queued_job(&member);
    assert!(stranger.try_recv().is_err());
    let [Outbound::One(_), Outbound::One(leader_roster)] =
        <[Outbound; 2]>::try_from(leader_out).ok().unwrap()
    else {
        panic!("expected a query response then a roster");
    };
    assert_eq!(leader_roster, shown, "rank 0 holds VIEWOFFNOTE");
    let [Outbound::One(_), Outbound::One(member_roster)] =
        <[Outbound; 2]>::try_from(member_out).ok().unwrap()
    else {
        panic!("expected a query response then a roster");
    };
    assert_eq!(member_roster, hidden, "rank 4 cannot view officer notes");
    assert_eq!(
        reads.load(Ordering::Relaxed),
        1,
        "one snapshot read serves every recipient"
    );
}

#[test]
fn an_invite_event_reaches_its_target_as_smsg_guild_invite() {
    use lyracore_shared::guild::event_kind;
    use wow_world_messages::vanilla::opcodes::ServerOpcodeMessage;
    let view = WorldView::new(true);
    let anchor = CellKey::at(0, 0, 0, 0);
    let (target_tx, target_rx) = SessionTx::with_depth(0);
    view.add_viewer_on_shard(viewer_with_tx(1, 9003, identity(1), target_tx), anchor, 0);

    let mut row = guild_event(9003, event_kind::INVITE, 0);
    row.strings = vec!["Alice".to_string(), "Tracer Guild".to_string()];
    guild_event_appeared(&view, &membership(&[]), &unreachable_snapshot_read(), &row);

    let outbound = queued_job(&target_rx);
    let [Outbound::One(ServerOpcodeMessage::SMSG_GUILD_INVITE(invite))] = outbound.as_slice()
    else {
        panic!(
            "expected exactly one SMSG_GUILD_INVITE, got {} packets",
            outbound.len()
        );
    };
    assert_eq!(invite.player_name, "Alice");
    assert_eq!(invite.guild_name, "Tracer Guild");
}

#[test]
fn a_decline_event_reaches_the_inviter_as_raw_smsg_guild_decline() {
    use lyracore_shared::guild::event_kind;
    let view = WorldView::new(true);
    let anchor = CellKey::at(0, 0, 0, 0);
    let (inviter_tx, inviter_rx) = SessionTx::with_depth(0);
    view.add_viewer_on_shard(viewer_with_tx(1, 9003, identity(1), inviter_tx), anchor, 0);

    let row = guild_event(9003, event_kind::DECLINE, 0);
    guild_event_appeared(&view, &membership(&[]), &unreachable_snapshot_read(), &row);

    assert_eq!(
        raw_packets(queued_job(&inviter_rx)),
        vec![(0x0086, b"Alice\0".to_vec())]
    );
}

const CHARTER: u64 = 0x4000_0000_0000_0101;

fn night_watch() -> crate::codec::PetitionView {
    crate::codec::PetitionView {
        petition_id: 42,
        charter_item_guid: CHARTER,
        owner_guid: 9001,
        name: "Night Watch".into(),
        signers: vec![9002],
    }
}

fn counting_petition_read(
    petition: Option<crate::codec::PetitionView>,
) -> (PetitionRead, Arc<AtomicU32>) {
    let reads = Arc::new(AtomicU32::new(0));
    let recorded = reads.clone();
    let read: PetitionRead = Arc::new(move |charter_item_guid| {
        recorded.fetch_add(1, Ordering::Relaxed);
        petition
            .clone()
            .filter(|petition| petition.charter_item_guid == charter_item_guid)
    });
    (read, reads)
}

fn unreachable_petition_read() -> PetitionRead {
    Arc::new(|_charter_item_guid| panic!("this petition kind must not read a Petition"))
}

fn petition_event(recipient_guid: u64, kind: u8, subject_guid: u64) -> GuildEvent {
    GuildEvent {
        guild_id: 0,
        other_guid: CHARTER,
        strings: Vec::new(),
        ..guild_event(recipient_guid, kind, subject_guid)
    }
}

fn recipient_and_bystander(view: &WorldView) -> (Receiver<Outbound>, Receiver<Outbound>) {
    let anchor = CellKey::at(0, 0, 0, 0);
    let (recipient_tx, recipient_rx) = SessionTx::with_depth(0);
    let (bystander_tx, bystander_rx) = SessionTx::with_depth(0);
    view.add_viewer_on_shard(
        viewer_with_tx(1, 9003, identity(1), recipient_tx),
        anchor,
        0,
    );
    view.add_viewer_on_shard(
        viewer_with_tx(2, 9004, identity(2), bystander_tx),
        anchor,
        0,
    );
    (recipient_rx, bystander_rx)
}

#[test]
fn an_offer_shows_its_target_the_signature_window_read_once() {
    use lyracore_shared::guild::event_kind;
    let view = WorldView::new(true);
    let (target_rx, bystander_rx) = recipient_and_bystander(&view);
    let (read, reads) = counting_petition_read(Some(night_watch()));

    petition_event_appeared(
        &view,
        &read,
        &petition_event(9003, event_kind::PETITION_OFFERED, 9001),
    );

    let outbound = queued_job(&target_rx);
    let [Outbound::One(ServerOpcodeMessage::SMSG_PETITION_SHOW_SIGNATURES(window))] =
        outbound.as_slice()
    else {
        panic!("expected one SMSG_PETITION_SHOW_SIGNATURES");
    };
    assert_eq!(
        **window,
        crate::codec::build_petition_show_signatures(&night_watch())
    );
    assert_eq!(reads.load(Ordering::Relaxed), 1);
    assert!(bystander_rx.try_recv().is_err());
}

#[test]
fn signature_events_render_sign_results_for_the_charter_and_the_signer() {
    use lyracore_shared::guild::event_kind;
    use wow_world_messages::vanilla::PetitionResult;
    let view = WorldView::new(true);
    let (owner_rx, _bystander_rx) = recipient_and_bystander(&view);
    for (kind, result) in [
        (event_kind::PETITION_SIGNED, PetitionResult::Ok),
        (
            event_kind::PETITION_ALREADY_SIGNED,
            PetitionResult::AlreadySigned,
        ),
    ] {
        petition_event_appeared(
            &view,
            &unreachable_petition_read(),
            &petition_event(9003, kind, 9002),
        );
        let outbound = queued_job(&owner_rx);
        let [Outbound::One(ServerOpcodeMessage::SMSG_PETITION_SIGN_RESULTS(results))] =
            outbound.as_slice()
        else {
            panic!("expected one SMSG_PETITION_SIGN_RESULTS");
        };
        assert_eq!(
            (
                results.petition.guid(),
                results.owner.guid(),
                results.result
            ),
            (CHARTER, 9002, result)
        );
    }
}

#[test]
fn a_decline_event_names_the_decliner_to_the_owner() {
    use lyracore_shared::guild::event_kind;
    let view = WorldView::new(true);
    let (owner_rx, _bystander_rx) = recipient_and_bystander(&view);
    petition_event_appeared(
        &view,
        &unreachable_petition_read(),
        &petition_event(9003, event_kind::PETITION_DECLINED, 9002),
    );
    let outbound = queued_job(&owner_rx);
    let [Outbound::One(ServerOpcodeMessage::MSG_PETITION_DECLINE(declined))] = outbound.as_slice()
    else {
        panic!("expected one MSG_PETITION_DECLINE");
    };
    assert_eq!(declined.petition.guid(), 9002);
}

#[test]
fn a_founder_event_names_the_new_guild() {
    use lyracore_shared::guild::event_kind;
    use wow_world_messages::vanilla::{GuildCommand, GuildCommandResult};
    let view = WorldView::new(true);
    let (signer_rx, _bystander_rx) = recipient_and_bystander(&view);
    let row = GuildEvent {
        guild_id: 12,
        strings: vec!["Night Watch".into()],
        ..petition_event(9003, event_kind::FOUNDER, 0)
    };
    petition_event_appeared(&view, &unreachable_petition_read(), &row);
    let outbound = queued_job(&signer_rx);
    let [Outbound::One(ServerOpcodeMessage::SMSG_GUILD_COMMAND_RESULT(result))] =
        outbound.as_slice()
    else {
        panic!("expected one SMSG_GUILD_COMMAND_RESULT");
    };
    assert_eq!(
        (result.command, result.string.as_str(), result.result),
        (
            GuildCommand::Founder,
            "Night Watch",
            GuildCommandResult::PlayerNoMoreInGuild
        )
    );
}

#[test]
fn a_lost_signature_sends_the_owner_a_fresh_petition_query() {
    use lyracore_shared::guild::event_kind;
    let view = WorldView::new(true);
    let (owner_rx, _bystander_rx) = recipient_and_bystander(&view);
    let (read, _) = counting_petition_read(Some(night_watch()));
    petition_event_appeared(
        &view,
        &read,
        &petition_event(9003, event_kind::PETITION_CHANGED, 9002),
    );
    let outbound = queued_job(&owner_rx);
    let [Outbound::One(ServerOpcodeMessage::SMSG_PETITION_QUERY_RESPONSE(query))] =
        outbound.as_slice()
    else {
        panic!("expected one SMSG_PETITION_QUERY_RESPONSE");
    };
    assert_eq!(
        (
            query.petition_id,
            query.charter_owner.guid(),
            query.guild_name.as_str()
        ),
        (42, 9001, "Night Watch")
    );

    let (gone, _) = counting_petition_read(None);
    petition_event_appeared(
        &view,
        &gone,
        &petition_event(9003, event_kind::PETITION_CHANGED, 9002),
    );
    assert!(
        queued_job(&owner_rx).is_empty(),
        "a Petition gone by the time the job runs renders nothing"
    );
}

#[test]
fn a_new_petition_shows_its_id_on_the_owners_charter() {
    let view = WorldView::new(true);
    let (owner_rx, bystander_rx) = recipient_and_bystander(&view);
    charter_petition_opened(
        &view,
        &GuildPetition {
            petition_id: 42,
            charter_item_guid: CHARTER,
            owner_guid: 9003,
            owner_name: "Owner".into(),
            team: 469,
            name: "Night Watch".into(),
            created_micros: 0,
        },
    );
    let packets = raw_packets(queued_job(&owner_rx));
    assert_eq!(packets.len(), 1);
    assert_eq!(packets[0].0, 0x00A9);
    let updates = lyracore_shared::values_mask::parse_values_updates(&packets[0].1);
    assert_eq!(updates.len(), 1);
    assert_eq!(updates[0].guid, CHARTER);
    assert_eq!(updates[0].fields, vec![(22, 42)]);
    assert!(bystander_rx.try_recv().is_err());
}

#[test]
fn a_disband_reaches_each_former_member_exactly_once() {
    use lyracore_shared::guild::event_kind;
    let view = WorldView::new(true);
    let anchor = CellKey::at(0, 0, 0, 0);
    let mut receivers = Vec::new();
    for (session, guid) in [(1, 9001), (2, 9002)] {
        let (tx, rx) = SessionTx::with_depth(0);
        view.add_viewer_on_shard(
            viewer_with_tx(session, guid, identity(session as u8), tx),
            anchor,
            0,
        );
        receivers.push(rx);
    }
    let [alice_rx, bob_rx] = <[_; 2]>::try_from(receivers).unwrap();
    let no_members = membership(&[]);

    guild_event_appeared(
        &view,
        &no_members,
        &unreachable_snapshot_read(),
        &guild_event(9001, event_kind::DISBANDED, 0),
    );
    guild_event_appeared(
        &view,
        &no_members,
        &unreachable_snapshot_read(),
        &guild_event(9002, event_kind::DISBANDED, 0),
    );

    assert_eq!(queued_job(&alice_rx).len(), 1);
    assert_eq!(queued_job(&bob_rx).len(), 1);
}

#[test]
fn a_removed_event_does_not_reach_the_removed_member() {
    use lyracore_shared::guild::event_kind;
    let view = WorldView::new(true);
    let anchor = CellKey::at(0, 0, 0, 0);
    let (remaining_tx, remaining_rx) = SessionTx::with_depth(0);
    let (removed_tx, removed_rx) = SessionTx::with_depth(0);
    view.add_viewer_on_shard(
        viewer_with_tx(1, 9001, identity(1), remaining_tx),
        anchor,
        0,
    );
    view.add_viewer_on_shard(viewer_with_tx(2, 9002, identity(2), removed_tx), anchor, 0);

    let members = membership(&[(9001, 7, 0)]);
    guild_event_appeared(
        &view,
        &members,
        &unreachable_snapshot_read(),
        &guild_event(0, event_kind::REMOVED, 0),
    );

    assert_eq!(queued_job(&remaining_rx).len(), 1);
    assert!(removed_rx.try_recv().is_err());
}
