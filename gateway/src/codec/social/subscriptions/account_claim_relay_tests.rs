use super::*;
use crate::stdb::aoi::ViewerGates;
use crate::stdb::bindings::{AccountClaim, Character};
use crate::stdb::world_index::CellKey;
use crate::stdb::world_view::{ExplorationReplay, MotionPending, Viewer, WorldView};
use crate::world::{Outbound, SessionTx};
use std::collections::{HashMap, HashSet};
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex};

const ALLIANCE: u32 = lyracore_shared::faction::TEAM_ALLIANCE;

const HORDE: u32 = lyracore_shared::faction::TEAM_HORDE;

const FRIEND: u64 = 10;

fn listener(
    view: &WorldView,
    shard: usize,
    self_guid: u64,
    team: u32,
) -> (Arc<Viewer>, Receiver<Outbound>) {
    let (tx, rx) = SessionTx::with_depth(0);
    let viewer = Arc::new(Viewer {
        active: std::sync::atomic::AtomicBool::new(true),
        session: view.next_session_id(),
        self_guid,
        bound_identity: spacetimedb_sdk::Identity::from_byte_array([self_guid as u8; 32]),
        map_id: 0,
        instance_id: 0,
        zone_id: 0.into(),
        tx,
        created: Default::default(),
        gates: Arc::new(ViewerGates::default()),
        skill_slots: Arc::new(Mutex::new((HashMap::new(), 0))),
        explored: Mutex::new(ExplorationReplay::default()),
        motion_pending: Arc::new(MotionPending::default()),
        member_stats: Default::default(),
        ignored: Mutex::default(),
        friends: Mutex::new(HashSet::from([FRIEND])),
        team,
        group_events: Default::default(),
    });
    view.add_viewer_on_shard(viewer.clone(), CellKey::at(0, 0, 0, 0), shard);
    (viewer, rx)
}

fn claim(generation: u64, character_guid: u64, closed: bool) -> AccountClaim {
    AccountClaim {
        account_id: 1,
        generation,
        request_nonce: 1,
        character_guid,
        expires_micros: 1_000_000,
        closed,
    }
}

fn character_row(
    guid: u64,
    race: u8,
    class: u8,
    level: u8,
    zone_id: u32,
    online: bool,
    session_start_micros: u64,
) -> Character {
    Character {
        guid,
        account_id: 0,
        owner_identity: spacetimedb_sdk::Identity::ZERO,
        name: String::new(),
        race,
        class,
        gender: 0,
        skin: 0,
        face: 0,
        hair_style: 0,
        hair_color: 0,
        facial_hair: 0,
        level,
        xp: 0,
        next_level_xp: 0,
        map_id: 0,
        zone_id,
        x: 0.0,
        y: 0.0,
        z: 0.0,
        orientation: 0.0,
        first_login: false,
        online,
        money: 0,
        rested_xp: 0,
        last_logout_micros: 0,
        home_map: 0,
        home_zone: 0,
        home_x: 0.0,
        home_y: 0.0,
        home_z: 0.0,
        played_total_secs: 0,
        session_start_micros,
        health: 0,
        power: 0,
        respec_count: 0,
        death_expire_micros: 0,
        pending_instance_id: 0,
        gm_level: 0,
        pending_ghost: false,
        resting: false,
        rested_since_micros: 0,
        pending_godmode: false,
        pending_run_speed_mult_bp: 0,
        bank_bag_slots: 0,
        watched_faction_index: -1,
    }
}

#[test]
fn claim_closed_covers_every_bullet() {
    assert_eq!(claim_closed(None, &claim(1, FRIEND, false)), None);

    let old = claim(1, FRIEND, false);
    let mut renewed = old.clone();
    renewed.expires_micros += 1;
    assert_eq!(claim_closed(Some(&old), &renewed), None);

    let old = claim(1, FRIEND, false);
    let closed = claim(1, FRIEND, true);
    assert_eq!(claim_closed(Some(&old), &closed), Some(FRIEND));

    let old = claim(1, FRIEND, true);
    let reopened = claim(2, FRIEND, false);
    assert_eq!(claim_closed(Some(&old), &reopened), None);

    let old = claim(1, FRIEND, false);
    let replaced = claim(2, 99, false);
    assert_eq!(claim_closed(Some(&old), &replaced), Some(FRIEND));

    let old = claim(1, FRIEND, true);
    let still_closed = claim(1, FRIEND, true);
    assert_eq!(claim_closed(Some(&old), &still_closed), None);
}

#[test]
fn friend_notice_candidates_lists_every_friend_listing_viewer_but_never_the_subject() {
    let view = WorldView::new(true);
    let (friend, _friend_rx) = listener(&view, 0, 20, ALLIANCE);
    let (elsewhere, _elsewhere_rx) = listener(&view, 1, 21, ALLIANCE);
    let (other_team, _other_team_rx) = listener(&view, 0, 22, HORDE);
    let (non_friend, _non_friend_rx) = listener(&view, 0, 23, ALLIANCE);
    non_friend.friends.lock().unwrap().clear();
    let (subject, _subject_rx) = listener(&view, 0, FRIEND, ALLIANCE);
    subject.friends.lock().unwrap().insert(FRIEND);

    let mut candidates: Vec<u64> = friend_notice_candidates(&view, FRIEND)
        .iter()
        .map(|viewer| viewer.self_guid)
        .collect();
    candidates.sort_unstable();

    assert_eq!(
        candidates,
        [friend.self_guid, elsewhere.self_guid, other_team.self_guid],
        "every friend-listing viewer on any Shard is a candidate, team included (each caller \
         checks it its own way), but never a non-friend and never the subject's own viewer"
    );
}

#[test]
fn character_online_changed_reaches_a_same_team_friend_with_the_just_committed_row() {
    let view = WorldView::new(true);
    let (_friend, friend_rx) = listener(&view, 0, 20, ALLIANCE);
    let (_other_team, other_team_rx) = listener(&view, 0, 21, HORDE);
    let (non_friend, non_friend_rx) = listener(&view, 0, 22, ALLIANCE);
    non_friend.friends.lock().unwrap().clear();

    let old = character_row(FRIEND, 1, 4, 21, 12, false, 0);
    let new = character_row(FRIEND, 1, 4, 22, 33, true, 500_000);

    character_online_changed(&view, &old, &new);

    match friend_rx
        .try_recv()
        .expect("a same-team friend must hear it")
    {
        Outbound::Job(job) => match job().as_slice() {
            [Outbound::Raw { opcode, body }] => {
                assert_eq!(*opcode, crate::codec::social::SMSG_FRIEND_STATUS_OPCODE);
                assert_eq!(body[0], 0x02, "ONLINE");
                assert_eq!(body[1..9], FRIEND.to_le_bytes());
                assert_eq!(
                    body[9], 1,
                    "status ONLINE — Away Status always clears at login"
                );
                assert_eq!(body[10..14], 33u32.to_le_bytes(), "the POST-login zone");
                assert_eq!(body[14..18], 22u32.to_le_bytes(), "the POST-login level");
                assert_eq!(body[18..22], 4u32.to_le_bytes(), "class");
            }
            _ => panic!("expected one Raw packet"),
        },
        _ => panic!("expected a job"),
    }
    assert!(
        other_team_rx.try_recv().is_err(),
        "a viewer on the other team hears nothing"
    );
    assert!(
        non_friend_rx.try_recv().is_err(),
        "a same-team viewer who does not list the guid as a friend hears nothing"
    );
    assert!(
        view.take_character_online(FRIEND),
        "the login must be marked so the later claim close can find it"
    );
}

#[test]
fn character_online_changed_ignores_every_update_that_is_not_a_login() {
    let view = WorldView::new(true);
    let (_friend, friend_rx) = listener(&view, 0, 20, ALLIANCE);

    let old = character_row(FRIEND, 1, 4, 21, 33, true, 500_000);
    let new = character_row(FRIEND, 1, 4, 22, 33, true, 500_000);
    character_online_changed(&view, &old, &new);
    assert!(friend_rx.try_recv().is_err());
    assert!(!view.take_character_online(FRIEND));

    let old = character_row(FRIEND, 1, 4, 22, 33, true, 500_000);
    let new = character_row(FRIEND, 1, 4, 22, 33, false, 0);
    character_online_changed(&view, &old, &new);
    assert!(friend_rx.try_recv().is_err());

    let old = character_row(FRIEND, 1, 4, 22, 33, true, 500_000);
    let new = character_row(FRIEND, 1, 4, 22, 33, true, 0);
    character_online_changed(&view, &old, &new);
    assert!(friend_rx.try_recv().is_err());
    assert!(!view.take_character_online(FRIEND));
}

#[test]
fn character_online_changed_fires_for_a_real_login_even_when_online_was_stuck_true() {
    let view = WorldView::new(true);
    let (_friend, friend_rx) = listener(&view, 0, 20, ALLIANCE);

    let old = character_row(FRIEND, 1, 4, 20, 12, true, 0);
    let new = character_row(FRIEND, 1, 4, 20, 12, true, 700_000);
    character_online_changed(&view, &old, &new);

    assert!(
        friend_rx.try_recv().is_ok(),
        "the real login must be heard even though `online` never flipped"
    );
    assert!(
        view.take_character_online(FRIEND),
        "the login must be marked so the later claim close can find it"
    );
}
