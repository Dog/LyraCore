use super::party_tests::{
    character, form_split_party, party_members, party_topology, BOT, GINGER, TRIN, VIM,
};
use super::*;
use crate::world::handlers::{member_stats_tick, MemberSnapshot, MemberStats};
use std::collections::HashMap;
use std::sync::atomic::Ordering;

type Snapshots = HashMap<u64, MemberSnapshot>;

fn caster() -> codec::MemberEntity {
    codec::MemberEntity {
        health: 1234,
        max_health: 1500,
        power: 800,
        max_power: 1000,
        unit_bytes_0: 0,
        level: 20,
        zone_id: 12,
        x: -8949.95,
        y: -132.49,
        dead: false,
        player_flags: 0,
        ..codec::MemberEntity::default()
    }
}

fn caster_with_auras() -> codec::MemberEntity {
    codec::MemberEntity {
        auras: vec![
            codec::MemberAuraSlot {
                slot: 0,
                spell_id: 100,
            },
            codec::MemberAuraSlot {
                slot: 5,
                spell_id: 200,
            },
            codec::MemberAuraSlot {
                slot: 35,
                spell_id: 300,
            },
        ],
        ..caster()
    }
}

fn hunter_pet() -> codec::MemberPetEntity {
    codec::MemberPetEntity {
        guid: 85,
        name: "Fluffy".into(),
        display_id: 618,
        health: 50,
        max_health: 60,
        power: 40,
        max_power: 100,
        unit_bytes_0: 0x0200_0000,
        auras: vec![codec::MemberAuraSlot {
            slot: 2,
            spell_id: 400,
        }],
    }
}

fn caster_aura_bytes() -> Vec<u8> {
    #[rustfmt::skip]
    let bytes = vec![
        0x21, 0x00, 0x00, 0x00,
        0x64, 0x00,
        0xC8, 0x00,
        0x08, 0x00,
        0x2C, 0x01,
    ];
    bytes
}

fn caster_base_bytes() -> Vec<u8> {
    vec![
        0x01, 0xD2, 0x04, 0xDC, 0x05, 0x00, 0x20, 0x03, 0xE8, 0x03, 0x14, 0x00, 0x0C, 0x00, 0x0B,
        0xDD, 0x7C, 0xFF,
    ]
}

fn hunter_pet_bytes() -> Vec<u8> {
    #[rustfmt::skip]
    let bytes = vec![
        0x55, 0, 0, 0, 0, 0, 0, 0,
        b'F', b'l', b'u', b'f', b'f', b'y', 0x00,
        0x6A, 0x02,
        0x32, 0x00,
        0x3C, 0x00,
        0x02,
        0x28, 0x00,
        0x64, 0x00,
    ];
    bytes
}

fn empty_positive_aura_bytes() -> Vec<u8> {
    let mut bytes = vec![0xFF, 0xFF, 0xFF, 0xFF];
    bytes.extend(std::iter::repeat_n(0u8, 32 * 2));
    bytes
}

fn empty_negative_aura_bytes() -> Vec<u8> {
    let mut bytes = vec![0xFF, 0xFF];
    bytes.extend(std::iter::repeat_n(0u8, 16 * 2));
    bytes
}

fn full_positive_aura_bytes(slot: usize, spell_id: u16) -> Vec<u8> {
    let mut bytes = vec![0xFF, 0xFF, 0xFF, 0xFF];
    for i in 0..32u16 {
        let value = if i as usize == slot { spell_id } else { 0 };
        bytes.extend_from_slice(&value.to_le_bytes());
    }
    bytes
}

fn zeroed_pet_bytes() -> Vec<u8> {
    let mut bytes = vec![0u8; 8];
    bytes.push(0);
    bytes.extend([0u8; 2]);
    bytes.extend([0u8; 2]);
    bytes.extend([0u8; 2]);
    bytes.push(0);
    bytes.extend([0u8; 2]);
    bytes.extend([0u8; 2]);
    bytes.extend(empty_positive_aura_bytes());
    bytes.extend(empty_negative_aura_bytes());
    bytes
}

fn caster_full_body(guid: u8) -> Vec<u8> {
    let mut body = vec![0x01, guid, 0xFF, 0x07, 0x00, 0x00];
    body.extend(caster_base_bytes());
    body.extend(empty_positive_aura_bytes());
    body.extend(empty_negative_aura_bytes());
    body
}

fn caster_first_send_body(guid: u8) -> Vec<u8> {
    let mut body = vec![0x01, guid, 0xFF, 0xFF, 0x1F, 0x00];
    body.extend(caster_base_bytes());
    body.extend(empty_positive_aura_bytes());
    body.extend(empty_negative_aura_bytes());
    body.extend(zeroed_pet_bytes());
    body
}

fn stats() -> codec::MemberStats {
    codec::MemberStats::from_entity(&caster())
}

fn place(shard: &WorldFake, guid: u64, entity: codec::MemberEntity) {
    let mut entities = shard.social.member_entities.lock().unwrap();
    entities.retain(|(g, _)| *g != guid);
    entities.push((guid, entity));
}

fn despawn(shard: &WorldFake, guid: u64) {
    shard
        .social
        .member_entities
        .lock()
        .unwrap()
        .retain(|(g, _)| *g != guid);
}

fn ginger_and_trin() -> (
    std::sync::Arc<WorldFake>,
    std::sync::Arc<WorldFake>,
    std::sync::Arc<WorldFake>,
) {
    let (realm, world, instances, _) = party_topology();
    party::run(
        world.as_ref(),
        Actor::new(GINGER).unwrap(),
        party::Op::Invite(TRIN),
    )
    .expect("invite Trin");
    party::run(world.as_ref(), Actor::new(TRIN).unwrap(), party::Op::Accept).expect("Trin accepts");
    place(&world, GINGER, caster());
    place(&world, TRIN, caster());
    (realm, world, instances)
}

fn tick(store: &WorldFake, created: &[u64], snapshots: &mut Snapshots) -> Vec<(u16, Vec<u8>)> {
    member_stats_tick(store, GINGER, |guid| created.contains(&guid), snapshots)
        .expect("the tick reads")
        .into_iter()
        .map(|packet| match packet {
            Outbound::Raw { opcode, body } => (opcode, body),
            _ => panic!("Member Stats are raw packets"),
        })
        .collect()
}

#[test]
fn a_live_mate_on_the_same_shard_gets_every_field_on_the_first_tick() {
    let (_realm, world, _instances) = ginger_and_trin();
    let packets = tick(&world, &[GINGER], &mut Snapshots::new());
    assert_eq!(packets, vec![(0x007E, caster_first_send_body(TRIN as u8))]);
}

#[test]
fn a_live_mate_on_another_shard_is_read_from_that_shards_cache() {
    let (_realm, world, instances, _) = party_topology();
    form_split_party(&world, &instances);
    let deadmines = codec::MemberEntity {
        zone_id: 1581,
        x: -16.4,
        y: -383.07,
        dead: true,
        ..caster()
    };
    place(&instances, VIM, deadmines);

    let packets = tick(&world, &[GINGER], &mut Snapshots::new());

    let mut expected = vec![0x01, 0x02, 0xFF, 0xFF, 0x1F, 0x00];
    #[rustfmt::skip]
    expected.extend([
        0x05,
        0xD2, 0x04, 0xDC, 0x05, 0x00, 0x20, 0x03, 0xE8, 0x03, 0x14, 0x00,
        0x2D, 0x06,
        0xF0, 0xFF,
        0x81, 0xFE,
    ]);
    expected.extend(empty_positive_aura_bytes());
    expected.extend(empty_negative_aura_bytes());
    expected.extend(zeroed_pet_bytes());
    assert_eq!(packets, vec![(0x007E, expected)]);
}

#[test]
fn a_mate_created_on_the_viewers_client_gets_nothing() {
    let (_realm, world, _instances) = ginger_and_trin();
    let mut snapshots = Snapshots::new();
    assert!(tick(&world, &[GINGER, TRIN], &mut snapshots).is_empty());
    assert!(snapshots.is_empty());
}

#[test]
fn after_only_health_changes_the_next_tick_sends_current_health_alone() {
    let (_realm, world, _instances) = ginger_and_trin();
    let mut snapshots = Snapshots::new();
    tick(&world, &[GINGER], &mut snapshots);
    place(
        &world,
        TRIN,
        codec::MemberEntity {
            health: 1000,
            ..caster()
        },
    );

    let packets = tick(&world, &[GINGER], &mut snapshots);

    let body = vec![0x01, 0x03, 0x02, 0x00, 0x00, 0x00, 0xE8, 0x03];
    assert_eq!(packets, vec![(0x007E, body)]);
    assert!(tick(&world, &[GINGER], &mut snapshots).is_empty());
}

#[test]
fn a_power_type_change_sends_the_type_and_both_power_values() {
    let (_realm, world, _instances) = ginger_and_trin();
    let mut snapshots = Snapshots::new();
    tick(&world, &[GINGER], &mut snapshots);
    place(
        &world,
        TRIN,
        codec::MemberEntity {
            unit_bytes_0: 0x0100_0000,
            ..caster()
        },
    );

    let packets = tick(&world, &[GINGER], &mut snapshots);

    let body = vec![
        0x01, 0x03, 0x38, 0x00, 0x00, 0x00, 0x01, 0x20, 0x03, 0xE8, 0x03,
    ];
    assert_eq!(packets, vec![(0x007E, body)]);
}

#[test]
fn a_mate_that_leaves_the_viewers_aoi_gets_every_field_again() {
    let (_realm, world, _instances) = ginger_and_trin();
    let mut snapshots = Snapshots::new();
    tick(&world, &[GINGER], &mut snapshots);
    assert!(tick(&world, &[GINGER, TRIN], &mut snapshots).is_empty());

    let packets = tick(&world, &[GINGER], &mut snapshots);

    assert_eq!(packets, vec![(0x007E, caster_first_send_body(TRIN as u8))]);
}

#[test]
fn an_offline_mate_gets_one_status_zero_packet_then_nothing_until_it_changes() {
    let (_realm, world, _instances) = ginger_and_trin();
    despawn(&world, TRIN);
    let mut snapshots = Snapshots::new();

    let packets = tick(&world, &[GINGER], &mut snapshots);
    let offline = vec![0x01, 0x03, 0x01, 0x00, 0x00, 0x00, 0x00];
    assert_eq!(packets, vec![(0x007E, offline)]);
    assert!(tick(&world, &[GINGER], &mut snapshots).is_empty());

    place(&world, TRIN, caster());
    let packets = tick(&world, &[GINGER], &mut snapshots);
    assert_eq!(packets, vec![(0x007E, caster_first_send_body(TRIN as u8))]);
}

#[test]
fn a_mate_in_transfer_gets_nothing_and_is_never_reported_offline() {
    let (realm, world, instances, _) = party_topology();
    form_split_party(&world, &instances);
    place(&instances, VIM, caster());
    let mut snapshots = Snapshots::new();
    tick(&world, &[GINGER], &mut snapshots);

    despawn(&instances, VIM);
    realm.party.members_in_transit.lock().unwrap().push(VIM);
    assert!(tick(&world, &[GINGER], &mut snapshots).is_empty());

    realm.party.members_in_transit.lock().unwrap().clear();
    place(&world, VIM, caster());
    assert!(tick(&world, &[GINGER], &mut snapshots).is_empty());
}

#[test]
fn a_viewer_with_no_group_gets_nothing_and_reads_no_member() {
    let (_realm, world, instances, _) = party_topology();
    place(&world, TRIN, caster());
    let mut snapshots = Snapshots::from([(TRIN, MemberSnapshot::Offline)]);

    assert!(tick(&world, &[GINGER], &mut snapshots).is_empty());

    assert!(
        snapshots.is_empty(),
        "a viewer with no group keeps no records"
    );
    for shard in [&world, &instances] {
        assert_eq!(
            shard
                .member_stats
                .member_presence_reads
                .load(Ordering::SeqCst),
            0
        );
    }
}

#[test]
fn a_mate_who_leaves_the_group_is_forgotten() {
    let (_realm, world, _instances) = ginger_and_trin();
    let mut snapshots = Snapshots::new();
    tick(&world, &[GINGER], &mut snapshots);
    party::run(world.as_ref(), Actor::new(TRIN).unwrap(), party::Op::Leave).expect("Trin leaves");

    assert!(tick(&world, &[GINGER], &mut snapshots).is_empty());
    assert!(!snapshots.contains_key(&TRIN));
}

fn request_with(store: &WorldFake, session: &mut ProtocolSession, guid: u64) -> ProtocolReply {
    MemberStats::handle(
        store,
        session,
        ClientOpcodeMessage::CMSG_REQUEST_PARTY_MEMBER_STATS(
            wow_world_messages::vanilla::CMSG_REQUEST_PARTY_MEMBER_STATS {
                guid: Guid::new(guid),
            },
        )
        .into(),
    )
    .unwrap()
}

fn request(store: &WorldFake, self_guid: Option<u64>, guid: u64) -> ProtocolReply {
    let mut session = match self_guid {
        Some(guid) => ProtocolSession::in_world(7, guid),
        None => ProtocolSession::new(7, "TESTER".into()),
    };
    request_with(store, &mut session, guid)
}

fn answer(outcome: ProtocolReply) -> Vec<(u16, Vec<u8>)> {
    let outbound = outcome.outbound;
    outbound
        .into_iter()
        .flat_map(|packet| match packet {
            Outbound::Job(job) => job(),
            packet => vec![packet],
        })
        .map(|packet| match packet {
            Outbound::Raw { opcode, body } => (opcode, body),
            _ => panic!("Member Stats are raw packets"),
        })
        .collect()
}

fn full_offline(guid: u8) -> Vec<(u16, Vec<u8>)> {
    vec![(0x02F2, vec![0x01, guid, 0x01, 0x00, 0x00, 0x00, 0x00])]
}

#[test]
fn a_request_for_a_group_mate_returns_one_full_packet() {
    let (_realm, world, instances, _) = party_topology();
    form_split_party(&world, &instances);
    place(&instances, VIM, caster());

    let packets = answer(request(&world, Some(GINGER), VIM));

    assert_eq!(packets, vec![(0x02F2, caster_full_body(VIM as u8))]);
}

#[test]
fn a_request_for_anyone_else_returns_the_offline_full_packet() {
    let (_realm, world, instances, _) = party_topology();
    form_split_party(&world, &instances);
    place(&world, BOT, caster());

    assert_eq!(
        answer(request(&world, Some(GINGER), BOT)),
        full_offline(BOT as u8)
    );
    assert_eq!(
        answer(request(&world, Some(GINGER), VIM)),
        full_offline(VIM as u8)
    );
}

#[test]
fn a_request_for_a_mate_between_two_places_answers_online_and_zone_out() {
    let (realm, world, instances, _) = party_topology();
    form_split_party(&world, &instances);
    let online_zone_out = vec![(0x02F2, vec![0x01, 0x02, 0x01, 0x00, 0x00, 0x00, 0x21])];

    realm.party.members_in_transit.lock().unwrap().push(VIM);
    assert_eq!(answer(request(&world, Some(GINGER), VIM)), online_zone_out);

    realm.party.members_in_transit.lock().unwrap().clear();
    instances
        .social
        .members_between_places
        .lock()
        .unwrap()
        .push(VIM);
    assert_eq!(answer(request(&world, Some(GINGER), VIM)), online_zone_out);
}

#[test]
fn after_a_full_answer_the_next_tick_sends_every_field() {
    let (realm, world, instances, _) = party_topology();
    form_split_party(&world, &instances);
    place(&instances, VIM, caster());
    let view = Arc::new(crate::stdb::world_view::WorldView::new(true));
    let (tx, _queued, _depth) = session_channel();
    let mut session = ProtocolSession::in_world(7, GINGER);
    let WorldState::InWorld(in_world) = &mut session.state else {
        unreachable!()
    };
    in_world.subs =
        PlayerSubscriptions::registered_for_test(view, GINGER, &codec::EntityView::default(), tx);
    let record = in_world.subs.member_stats_record().unwrap().clone();
    tick(&world, &[GINGER], &mut record.lock());

    despawn(&instances, VIM);
    realm.party.members_in_transit.lock().unwrap().push(VIM);
    answer(request_with(&world, &mut session, VIM));
    realm.party.members_in_transit.lock().unwrap().clear();
    place(&world, VIM, caster());

    let packets = tick(&world, &[GINGER], &mut record.lock());
    assert_eq!(packets, vec![(0x007E, caster_first_send_body(VIM as u8))]);
}

#[test]
fn a_failed_read_answers_nothing_and_is_never_offline() {
    let (realm, world, instances, _) = party_topology();
    form_split_party(&world, &instances);
    let unhealthy = WorldFake {
        topology: TopologyState {
            shard: "world".into(),
            realm: Some(realm),
            world_shard_set_error: Some("instances has no healthy Coordinator subscription".into()),
            ..Default::default()
        },
        ..Default::default()
    };
    *unhealthy.topology.peers.lock().unwrap() = vec![world.clone(), instances.clone()];

    assert!(answer(request(&unhealthy, Some(GINGER), VIM)).is_empty());
    let mut snapshots = Snapshots::new();
    assert!(
        member_stats_tick(&unhealthy, GINGER, |_| false, &mut snapshots).is_err(),
        "an unproven absence is not Offline"
    );
}

#[test]
fn a_bot_crossing_between_shards_is_never_reported_offline() {
    let (_realm, world, _instances, _) = party_topology();
    party::run(
        world.as_ref(),
        Actor::new(GINGER).unwrap(),
        party::Op::Invite(BOT),
    )
    .expect("invite the bot");
    place(&world, BOT, caster());
    let mut snapshots = Snapshots::new();
    tick(&world, &[GINGER], &mut snapshots);

    despawn(&world, BOT);
    world
        .social
        .members_between_places
        .lock()
        .unwrap()
        .push(BOT);

    assert!(tick(&world, &[GINGER], &mut snapshots).is_empty());
    assert_eq!(
        snapshots.get(&BOT).cloned(),
        Some(MemberSnapshot::Live(Box::new(stats())))
    );
}

#[test]
fn a_stats_request_outside_the_world_sends_nothing() {
    let (_realm, world, _instances) = ginger_and_trin();
    assert!(request(&world, None, TRIN).outbound.is_empty());
}

#[test]
fn a_stats_request_is_answered_through_the_encrypted_session() {
    let store = {
        let base = quest_store();
        WorldFake {
            party: PartyState {
                mirror: std::sync::Mutex::new(vec![party::GroupRoster {
                    group_id: 1,
                    leader_guid: 1,
                    members: party_members(&[1, 2]),
                    ..Default::default()
                }]),
                ..base.party
            },
            ..base
        }
    };
    place(&store, 2, caster());
    let (mut client, mut c_enc, mut c_dec, server) = enter_world(std::sync::Arc::new(store), 1);

    wow_world_messages::vanilla::CMSG_REQUEST_PARTY_MEMBER_STATS { guid: Guid::new(2) }
        .write_encrypted_client(&mut client, &mut c_enc)
        .unwrap();

    assert_eq!(
        read_raw_frame(&mut client, &mut c_dec),
        (0x02F2, caster_full_body(2))
    );
    drop(client);
    let _ = server.join();
}

#[test]
fn a_stats_request_that_cannot_be_read_does_not_end_the_session() {
    let store = {
        let base = quest_store();
        WorldFake {
            party: PartyState {
                mirror: std::sync::Mutex::new(vec![party::GroupRoster {
                    group_id: 1,
                    leader_guid: 1,
                    members: party_members(&[1, 2]),
                    ..Default::default()
                }]),
                ..base.party
            },
            topology: TopologyState {
                world_shard_set_error: Some(
                    "instances has no healthy Coordinator subscription".into(),
                ),
                ..base.topology
            },
            ..base
        }
    };
    let (mut client, mut c_enc, mut c_dec, server) = enter_world(std::sync::Arc::new(store), 1);

    for guid in [2, 99] {
        wow_world_messages::vanilla::CMSG_REQUEST_PARTY_MEMBER_STATS {
            guid: Guid::new(guid),
        }
        .write_encrypted_client(&mut client, &mut c_enc)
        .unwrap();
    }

    assert_eq!(read_raw_frame(&mut client, &mut c_dec), full_offline(99)[0]);
    drop(client);
    let _ = server.join();
}

#[test]
fn world_entry_forgets_member_stats_sent_before_the_party_frame() {
    let view = std::sync::Arc::new(crate::stdb::world_view::WorldView::new(true));
    let (realm, _world, _instances, calls) = party_topology();
    let session_shard = std::sync::Arc::new(WorldFake {
        topology: TopologyState {
            shard: "world".into(),
            calls,
            realm: Some(realm.clone()),
            ..Default::default()
        },
        session: SessionState {
            username: "TESTER".into(),
            session: Some(WorldSession {
                account_id: 7,
                session_key: K,
            }),
            login_entity: Some(warrior_entity()),
            live_guids: vec![GINGER, VIM],
            relay_view: Some(view.clone()),
            member_stats_before_party_frame: Some(VIM),
            ..Default::default()
        },
        characters: vec![character(GINGER, "Ginger"), character(VIM, "Vim")],
        ..Default::default()
    });
    *session_shard.topology.peers.lock().unwrap() = vec![session_shard.clone()];
    place(&session_shard, VIM, caster());
    {
        let mut party = realm.party.party.lock().unwrap();
        party.next_group_id = 5;
        party.groups.push((5, GINGER, 3, 2, 0));
        party.members.push((5, GINGER));
        party.members.push((5, VIM));
    }
    let (mut client, server_end) = world_session_socket_pair();
    let server_store = session_shard.clone();
    let server = std::thread::spawn(move || {
        let _ = run_world_session(server_end, server_store.clone());
    });
    let (mut c_enc, mut c_dec) = client_handshake(&mut client, "TESTER", K);
    CMSG_PLAYER_LOGIN {
        guid: Guid::new(GINGER),
    }
    .write_encrypted_client(&mut client, &mut c_enc)
    .unwrap();
    wow_world_messages::vanilla::CMSG_REQUEST_PARTY_MEMBER_STATS {
        guid: Guid::new(99),
    }
    .write_encrypted_client(&mut client, &mut c_enc)
    .unwrap();
    let mut saw_party_frame = false;
    loop {
        let (opcode, _) = read_raw_frame(&mut client, &mut c_dec);
        saw_party_frame |= opcode == 0x007D;
        if opcode == 0x02F2 {
            break;
        }
    }
    assert!(saw_party_frame, "world entry renders the party frame");

    let record = view
        .viewer_of_owner(crate::stdb::world_view::OwnerGuid(GINGER))
        .expect("world entry registers the viewer")
        .member_stats
        .clone();
    let packets = tick(&session_shard, &[GINGER], &mut record.lock());
    assert_eq!(packets, vec![(0x007E, caster_first_send_body(VIM as u8))]);
    drop(client);
    let _ = server.join();
}

#[test]
fn a_member_with_two_buffs_and_one_debuff_sends_both_aura_blocks() {
    let (_realm, world, _instances) = ginger_and_trin();
    let mut snapshots = Snapshots::new();
    tick(&world, &[GINGER], &mut snapshots);

    place(&world, TRIN, caster_with_auras());
    let packets = tick(&world, &[GINGER], &mut snapshots);

    let mut expected = vec![0x01, TRIN as u8, 0x00, 0x06, 0x00, 0x00];
    expected.extend(caster_aura_bytes());
    assert_eq!(packets, vec![(0x007E, expected)]);
}

#[test]
fn removing_one_buff_sends_only_its_bit_with_id_zero() {
    let (_realm, world, _instances) = ginger_and_trin();
    place(&world, TRIN, caster_with_auras());
    let mut snapshots = Snapshots::new();
    tick(&world, &[GINGER], &mut snapshots);

    let mut one_buff_left = caster_with_auras();
    one_buff_left.auras.retain(|aura| aura.slot != 0);
    place(&world, TRIN, one_buff_left);

    let packets = tick(&world, &[GINGER], &mut snapshots);
    let expected = vec![
        0x01, TRIN as u8, 0x00, 0x02, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00,
    ];
    assert_eq!(packets, vec![(0x007E, expected)]);
}

#[test]
fn a_live_pet_sends_its_block_then_dismissing_it_zeroes_the_bits_once() {
    let (_realm, world, _instances) = ginger_and_trin();
    let mut snapshots = Snapshots::new();
    place(
        &world,
        TRIN,
        codec::MemberEntity {
            pet: Some(hunter_pet()),
            ..caster()
        },
    );

    let mut first_send = vec![0x01, TRIN as u8, 0xFF, 0xFF, 0x1F, 0x00];
    first_send.extend(caster_base_bytes());
    first_send.extend(empty_positive_aura_bytes());
    first_send.extend(empty_negative_aura_bytes());
    first_send.extend(hunter_pet_bytes());
    first_send.extend(full_positive_aura_bytes(2, 400));
    first_send.extend(empty_negative_aura_bytes());
    assert_eq!(
        tick(&world, &[GINGER], &mut snapshots),
        vec![(0x007E, first_send)]
    );

    place(&world, TRIN, caster());
    let dismissed = vec![
        0x01, TRIN as u8, 0x00, 0xF8, 0x0F, 0x00, 0, 0, 0, 0, 0, 0, 0, 0, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x04, 0x00, 0x00, 0x00, 0x00, 0x00,
    ];
    assert_eq!(
        tick(&world, &[GINGER], &mut snapshots),
        vec![(0x007E, dismissed)]
    );

    assert!(
        tick(&world, &[GINGER], &mut snapshots).is_empty(),
        "a pet-less member that stays pet-less sends nothing more about it"
    );
}

#[test]
fn a_member_without_a_pet_gets_a_full_answer_with_no_pet_bits() {
    let (_realm, world, instances, _) = party_topology();
    form_split_party(&world, &instances);
    place(&instances, VIM, caster());

    let packets = answer(request(&world, Some(GINGER), VIM));

    assert_eq!(packets, vec![(0x02F2, caster_full_body(VIM as u8))]);
    let mask = u32::from_le_bytes(packets[0].1[3..7].try_into().unwrap());
    assert_eq!(
        mask & codec::GroupUpdateMask::PET.bits(),
        0,
        "no pet bit set"
    );
}

#[test]
fn a_created_mate_with_auras_and_a_pet_still_gets_nothing() {
    let (_realm, world, _instances) = ginger_and_trin();
    place(
        &world,
        TRIN,
        codec::MemberEntity {
            pet: Some(hunter_pet()),
            ..caster_with_auras()
        },
    );
    let mut snapshots = Snapshots::new();

    assert!(tick(&world, &[GINGER, TRIN], &mut snapshots).is_empty());
    assert!(snapshots.is_empty());
}
