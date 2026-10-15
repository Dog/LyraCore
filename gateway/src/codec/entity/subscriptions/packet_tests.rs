use super::*;
use crate::codec;
use crate::codec::combat::subscriptions::tests::aura;
use crate::codec::combat::subscriptions::A_STEALTH;
use crate::codec::entity::subscriptions::tests::{creature_entity, player_entity};
use crate::codec::CreateKind;
use crate::stdb::aoi::ViewerGates;
use crate::stdb::bindings::{EncounterEquip, WorldEntity};
use crate::stdb::views::entity_view;
use crate::stdb::world_view;
use crate::stdb::world_view::Viewer;
use crate::world::{Outbound, SessionTx};
use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use wow_world_messages::vanilla::opcodes::ServerOpcodeMessage;

fn viewer_with_created(creature_guid: u64) -> Viewer {
    let (tx, _rx) = SessionTx::with_depth(0);
    Viewer {
        active: std::sync::atomic::AtomicBool::new(true),
        session: 1,
        self_guid: 7,
        bound_identity: spacetimedb_sdk::Identity::from_byte_array([0; 32]),
        map_id: 0,
        instance_id: 0,
        zone_id: 0.into(),
        tx,
        created: Arc::new(Mutex::new(HashSet::from([creature_guid]))),
        gates: Arc::new(ViewerGates::default()),
        skill_slots: Arc::new(Mutex::new((std::collections::HashMap::new(), 0))),
        explored: Mutex::new(world_view::ExplorationReplay::default()),
        motion_pending: Arc::new(world_view::MotionPending::default()),
        member_stats: Default::default(),
        ignored: Mutex::default(),
        friends: Mutex::default(),
        team: lyracore_shared::faction::TEAM_ALLIANCE,
        group_events: Default::default(),
    }
}

#[test]
fn encounter_equipment_reaches_live_and_late_viewers_in_wire_order() {
    let creature_guid = 0xF130_0000_0000_002A;
    let viewer = viewer_with_created(creature_guid);
    let equipment = EncounterEquip {
        creature_guid,
        instance_id: 0,
        main_hand: 2_196,
        off_hand: 2_716,
        ranged: 0,
    };

    for (row, cleared, expected) in [
        (equipment.clone(), false, (2_196, 2_716, 0)),
        (
            EncounterEquip {
                main_hand: 2_716,
                off_hand: 0,
                ranged: 2_196,
                ..equipment.clone()
            },
            false,
            (2_716, 0, 2_196),
        ),
        (equipment.clone(), true, (0, 0, 0)),
    ] {
        let out = encounter_equip_outbound(&viewer, &row, cleared);
        let (expected_opcode, expected_body) =
            codec::build_virtual_item_values(creature_guid, expected.0, expected.1, expected.2);
        assert!(matches!(
            out.as_slice(),
            [Outbound::Raw { opcode, body }]
                if *opcode == expected_opcode && *body == expected_body
        ));
    }

    let creature = codec::EntityView {
        guid: creature_guid,
        type_mask: lyracore_shared::constants::type_mask::CREATURE,
        entry: 42,
        scale_x: 1.0,
        health: 1,
        max_health: 1,
        level: 1,
        run_speed_mult_bp: 10_000,
        unit_bytes_0: 0x0101,
        display_id: 1,
        native_display_id: 1,
        ..Default::default()
    };
    let create = codec::build_create_object(&creature, CreateKind::Peer, &[], &[]).unwrap();
    let mut late = vec![Outbound::One(ServerOpcodeMessage::SMSG_UPDATE_OBJECT(
        Box::new(create),
    ))];
    append_encounter_equip_after_create(&mut late, &viewer, &equipment);
    assert!(matches!(
        late.as_slice(),
        [
            Outbound::One(ServerOpcodeMessage::SMSG_UPDATE_OBJECT(_)),
            Outbound::Raw { .. }
        ]
    ));
}

fn created_player(out: &[Outbound]) -> wow_world_messages::vanilla::UpdatePlayer {
    use wow_world_messages::vanilla::{Object, UpdateMask};
    let Some(Outbound::One(ServerOpcodeMessage::SMSG_UPDATE_OBJECT(create))) = out.first() else {
        panic!("a peer CREATE comes first");
    };
    let [Object::CreateObject2 {
        mask2: UpdateMask::Player(player),
        ..
    }] = create.objects.as_slice()
    else {
        panic!("the CREATE must carry a player mask");
    };
    player.clone()
}

fn human_warrior() -> WorldEntity {
    WorldEntity {
        unit_bytes_0: 1 | (1 << 8) | (1 << 24),
        ..player_entity()
    }
}

#[test]
fn a_peer_player_create_carries_its_guild_projection() {
    let row = human_warrior();
    let view = peer_entity_view(&row, 0, |guid| {
        assert_eq!(guid, row.guid);
        (7, 3)
    });
    let player = created_player(&peer_create_outbound(&view, &[], &[], None).unwrap());
    assert_eq!(player.player_guildid(), Some(7));
    assert_eq!(player.player_guildrank(), Some(3));
}

#[test]
fn a_peer_outside_any_guild_is_created_with_an_explicit_zero() {
    let row = human_warrior();
    let player = created_player(
        &peer_create_outbound(&peer_entity_view(&row, 0, |_| (0, 0)), &[], &[], None).unwrap(),
    );
    assert_eq!(player.player_guildid(), Some(0));
    assert_eq!(player.player_guildrank(), Some(0));
}

#[test]
fn a_creature_create_reads_no_guild() {
    let view = peer_entity_view(&creature_entity(), 0, |_| panic!("a creature has no Guild"));
    assert_eq!((view.guild_id, view.guild_rank), (0, 0));
}

#[test]
fn no_change_emits_nothing() {
    let e = player_entity();
    assert!(entity_update_to_outbound(&e, &e).is_empty());
}

#[test]
fn single_health_change_emits_one() {
    let old = player_entity();
    let mut new = old.clone();
    new.health = 50;
    let out = entity_update_to_outbound(&old, &new);
    assert_eq!(
        out,
        vec![ServerOpcodeMessage::SMSG_UPDATE_OBJECT(Box::new(
            codec::build_health_values(new.guid, 50)
        ))]
    );
}

#[test]
fn sheath_change_relays_and_a_no_op_stays_silent() {
    let old = player_entity();
    let mut new = old.clone();
    new.unit_bytes_2 = 1;
    assert_eq!(
        entity_update_to_outbound(&old, &new),
        vec![ServerOpcodeMessage::SMSG_UPDATE_OBJECT(Box::new(
            codec::build_sheath_values(new.guid, 1)
        ))]
    );
    assert!(
        entity_update_to_outbound(&old, &old.clone()).is_empty(),
        "an unchanged row must not relay a sheath packet"
    );
}

#[test]
fn independent_field_changes_each_emit() {
    let old = player_entity();
    let mut new = old.clone();
    new.health = 50;
    new.dynamic_flags = 1;
    new.power = 30;
    let power_b = (new.unit_bytes_0 >> 24) as u8;
    let out = entity_update_to_outbound(&old, &new);
    assert_eq!(
        out,
        vec![
            ServerOpcodeMessage::SMSG_UPDATE_OBJECT(Box::new(codec::build_health_values(
                new.guid, 50
            ))),
            ServerOpcodeMessage::SMSG_UPDATE_OBJECT(Box::new(codec::build_dynamic_flags_values(
                new.guid, 1
            ))),
            ServerOpcodeMessage::SMSG_UPDATE_OBJECT(Box::new(codec::build_power_values(
                new.guid, power_b, 30
            ))),
        ]
    );
}

#[test]
fn dynamic_flag_values_uses_the_viewer_projection() {
    let mut old = creature_entity();
    old.dynamic_flags = 0;
    let mut new = old.clone();
    new.dynamic_flags = lyracore_shared::constants::unit_dynamic_flags::TAPPED;
    let projected =
        new.dynamic_flags | lyracore_shared::constants::unit_dynamic_flags::TAPPED_BY_PLAYER;
    assert_eq!(
        entity_update_to_outbound_with_dynamic_flags(&old, &new, projected),
        vec![ServerOpcodeMessage::SMSG_UPDATE_OBJECT(Box::new(
            codec::build_dynamic_flags_values(new.guid, projected)
        ))]
    );
}

#[test]
fn levelup_is_exclusive_with_field_diffs() {
    let old = player_entity();
    let mut new = old.clone();
    new.level = 2;
    new.health = 120;
    new.max_health = 120;
    new.xp = 10;
    assert_eq!(entity_update_to_outbound(&old, &new).len(), 1);
}

#[test]
fn a_stand_state_change_relays_unit_bytes_1_to_every_viewer() {
    let old = player_entity();
    let mut new = old.clone();
    new.unit_bytes_1 = (old.unit_bytes_1 & !0xFF) | 1;
    let out = entity_update_to_outbound(&old, &new);
    let [ServerOpcodeMessage::SMSG_UPDATE_OBJECT(update)] = out.as_slice() else {
        panic!("expected one VALUES update, got {out:?}");
    };
    let mut wire = Vec::new();
    wow_world_messages::vanilla::ServerMessage::write_unencrypted_server(&**update, &mut wire)
        .unwrap();
    let values = lyracore_shared::values_mask::parse_values_updates(&wire[4..]);
    assert_eq!(values.len(), 1);
    assert!(
        values[0].fields.contains(&(138, new.unit_bytes_1)),
        "UNIT_FIELD_BYTES_1 missing from {:?}",
        values[0].fields
    );
}

#[test]
fn an_afk_toggle_relays_player_flags_to_every_viewer() {
    let old = player_entity();
    for (flags, label) in [(0x02, "AFK"), (0x04, "DND"), (0x00, "cleared")] {
        let mut new = old.clone();
        new.player_flags = flags;
        let before = if flags == 0 {
            let mut away = old.clone();
            away.player_flags = 0x02;
            away
        } else {
            old.clone()
        };
        let out = entity_update_to_outbound(&before, &new);
        let [ServerOpcodeMessage::SMSG_UPDATE_OBJECT(update)] = out.as_slice() else {
            panic!("{label}: expected one VALUES update, got {out:?}");
        };
        let mut wire = Vec::new();
        wow_world_messages::vanilla::ServerMessage::write_unencrypted_server(&**update, &mut wire)
            .unwrap();
        let values = lyracore_shared::values_mask::parse_values_updates(&wire[4..]);
        assert_eq!(values.len(), 1, "{label}");
        assert_eq!(values[0].guid, 1, "{label}");
        assert!(
            values[0].fields.contains(&(190, flags)),
            "{label}: PLAYER_FLAGS missing from {:?}",
            values[0].fields
        );
        assert!(
            !values[0].fields.iter().any(|&(index, _)| index == 2),
            "{label}: OBJECT_FIELD_TYPE leaked into a partial update"
        );
    }
}

#[test]
fn player_only_fields_are_gated_for_creatures() {
    let mut old = player_entity();
    old.type_mask = 0;
    let mut new = old.clone();
    new.health = 50;
    new.dynamic_flags = 1;
    new.xp = 10;
    new.money = 10;
    new.power = 30;
    new.player_flags = 0x10;
    let out = entity_update_to_outbound(&old, &new);
    assert_eq!(
        out,
        vec![
            ServerOpcodeMessage::SMSG_UPDATE_OBJECT(Box::new(codec::build_health_values(
                new.guid, 50
            ))),
            ServerOpcodeMessage::SMSG_UPDATE_OBJECT(Box::new(codec::build_dynamic_flags_values(
                new.guid, 1
            ))),
        ]
    );
}

#[test]
fn creature_levelup_does_not_take_the_exclusive_branch() {
    let mut old = player_entity();
    old.type_mask = 0;
    let mut new = old.clone();
    new.level = 5;
    new.health = 50;
    let out = entity_update_to_outbound(&old, &new);
    assert_eq!(
        out,
        vec![ServerOpcodeMessage::SMSG_UPDATE_OBJECT(Box::new(
            codec::build_health_values(new.guid, new.health)
        ))]
    );
}

#[test]
fn target_change_emits_one() {
    let old = player_entity();
    let mut new = old.clone();
    new.target_guid = 0xDEAD_BEEF;
    let out = entity_update_to_outbound(&old, &new);
    assert_eq!(
        out,
        vec![ServerOpcodeMessage::SMSG_UPDATE_OBJECT(Box::new(
            codec::build_target_values(new.guid, 0xDEAD_BEEF)
        ))]
    );
}

#[test]
fn bank_bag_slot_purchase_relays_and_the_rest_byte_survives() {
    // Preserve all PLAYER_BYTES_2 bytes; a zero rest-state byte crashes build 5875's XP bar.
    let mut old = player_entity();
    old.player_bytes_2 = lyracore_shared::packing::player_bytes_2_with_rest(7, 0, false);
    let mut new = old.clone();
    new.player_bytes_2 = lyracore_shared::packing::with_bank_bag_slots(old.player_bytes_2, 1);
    let out = entity_update_to_outbound(&old, &new);
    assert_eq!(
        out,
        vec![ServerOpcodeMessage::SMSG_UPDATE_OBJECT(Box::new(
            codec::build_bank_bag_slots_values(new.guid, new.player_bytes_2)
        ))]
    );
    let (facial_hair, _, bank_slots, rest) = lyracore_shared::packing::unpack4(new.player_bytes_2);
    assert_eq!(
        facial_hair, 7,
        "facial hair (byte 0) must survive the purchase"
    );
    assert_eq!(bank_slots, 1);
    assert_eq!(
        rest,
        lyracore_shared::packing::REST_STATE_NORMAL,
        "rest state (byte 3) must stay a valid non-zero value"
    );
}

#[test]
fn a_rest_state_flip_alone_does_not_relay_the_bank_slot_packet() {
    let old = player_entity();
    let mut new = old.clone();
    new.player_bytes_2 = lyracore_shared::packing::player_bytes_2_with_rest(0, 0, true);
    assert!(entity_update_to_outbound(&old, &new).is_empty());
}

#[test]
fn max_vitals_change_emits_one() {
    let old = player_entity();
    let mut new = old.clone();
    new.max_health = 150;
    let power_b = (new.unit_bytes_0 >> 24) as u8;
    let out = entity_update_to_outbound(&old, &new);
    assert_eq!(
        out,
        vec![ServerOpcodeMessage::SMSG_UPDATE_OBJECT(Box::new(
            codec::build_max_vitals_values(new.guid, 150, power_b, new.max_power)
        ))]
    );
}

#[test]
fn ghost_transition_edge_detects_the_spirit_healer_reveal() {
    const GHOST: u32 = lyracore_shared::constants::player_flags::GHOST;
    assert_eq!(ghost_transition(0, GHOST, GHOST), Some(true));
    assert_eq!(ghost_transition(GHOST, 0, GHOST), Some(false));
    assert_eq!(ghost_transition(0, 0, GHOST), None);
    assert_eq!(ghost_transition(GHOST, GHOST, GHOST), None);
    assert_eq!(ghost_transition(0x4 | GHOST, 0x8 | GHOST, GHOST), None);
    assert_eq!(ghost_transition(0x4, 0x8, GHOST), None);
}

#[test]
fn in_combat_flag_change_emits_one() {
    let old = player_entity();
    let mut new = old.clone();
    new.unit_flags |= lyracore_shared::constants::unit_flags::IN_COMBAT;
    let out = entity_update_to_outbound(&old, &new);
    assert_eq!(
        out,
        vec![ServerOpcodeMessage::SMSG_UPDATE_OBJECT(Box::new(
            codec::build_unit_flags_values(new.guid, new.unit_flags)
        ))]
    );
}

#[test]
fn taxi_mount_and_flight_flag_change_emit_one_atomic_presentation_values() {
    let old = player_entity();
    let mut new = old.clone();
    new.mount_display_id = 1147;
    new.unit_flags |= lyracore_shared::constants::unit_flags::TAXI_FLIGHT;
    assert_eq!(
        entity_update_to_outbound(&old, &new),
        vec![ServerOpcodeMessage::SMSG_UPDATE_OBJECT(Box::new(
            codec::build_taxi_presentation_values(new.guid, new.mount_display_id, new.unit_flags,)
        ))]
    );
}

#[test]
fn taxi_landing_emits_one_atomic_presentation_update_not_a_land_mount_duplicate() {
    let mut old = player_entity();
    old.mount_display_id = 1147;
    old.unit_flags |= lyracore_shared::constants::unit_flags::TAXI_FLIGHT;
    let mut new = old.clone();
    new.mount_display_id = 0;
    new.unit_flags &= !lyracore_shared::constants::unit_flags::TAXI_FLIGHT;
    assert_eq!(
        entity_update_to_outbound(&old, &new),
        vec![ServerOpcodeMessage::SMSG_UPDATE_OBJECT(Box::new(
            codec::build_taxi_presentation_values(new.guid, new.mount_display_id, new.unit_flags,)
        ))]
    );
}

#[test]
fn land_mount_display_change_emits_the_standalone_builder_not_taxi_presentation() {
    let old = player_entity();
    let mut new = old.clone();
    new.mount_display_id = 1147;
    assert_eq!(
        entity_update_to_outbound(&old, &new),
        vec![ServerOpcodeMessage::SMSG_UPDATE_OBJECT(Box::new(
            codec::build_mount_display_values(new.guid, new.mount_display_id)
        ))]
    );
}

#[test]
fn land_dismount_display_change_emits_the_standalone_builder_with_zero() {
    let mut old = player_entity();
    old.mount_display_id = 1147;
    let mut new = old.clone();
    new.mount_display_id = 0;
    assert_eq!(
        entity_update_to_outbound(&old, &new),
        vec![ServerOpcodeMessage::SMSG_UPDATE_OBJECT(Box::new(
            codec::build_mount_display_values(new.guid, 0)
        ))]
    );
}

#[test]
fn a_land_mount_display_change_alongside_an_unrelated_flag_change_emits_both_packets() {
    let old = player_entity();
    let mut new = old.clone();
    new.mount_display_id = 1147;
    new.unit_flags |= lyracore_shared::constants::unit_flags::IN_COMBAT;
    assert_eq!(
        entity_update_to_outbound(&old, &new),
        vec![
            ServerOpcodeMessage::SMSG_UPDATE_OBJECT(Box::new(codec::build_mount_display_values(
                new.guid,
                new.mount_display_id
            ))),
            ServerOpcodeMessage::SMSG_UPDATE_OBJECT(Box::new(codec::build_unit_flags_values(
                new.guid,
                new.unit_flags
            ))),
        ]
    );
}

#[test]
fn run_speed_mult_bp_change_emits_force_run_speed_change_with_the_derived_speed() {
    let old = player_entity();
    let mut new = old.clone();
    new.run_speed_mult_bp = 30_000;
    let out = entity_update_to_outbound(&old, &new);
    assert_eq!(
        out,
        vec![ServerOpcodeMessage::SMSG_FORCE_RUN_SPEED_CHANGE(Box::new(
            codec::build_force_run_speed(new.guid, 21.0)
        ))]
    );
}

#[test]
fn appearance_change_relays_the_wisp_and_the_restored_native_display() {
    let alive = player_entity();
    let mut wisp = alive.clone();
    wisp.display_id = 1825;
    for (old, new) in [(&alive, &wisp), (&wisp, &alive)] {
        assert_eq!(
            entity_update_to_outbound(old, new),
            vec![ServerOpcodeMessage::SMSG_UPDATE_OBJECT(Box::new(
                codec::build_display_values(new.guid, new.display_id),
            ))]
        );
        assert_eq!(new.native_display_id, old.native_display_id);
    }
}

#[test]
fn run_speed_mult_bp_unchanged_emits_nothing_extra() {
    let old = player_entity();
    let mut new = old.clone();
    new.health = 50;
    let out = entity_update_to_outbound(&old, &new);
    assert!(!out
        .iter()
        .any(|m| matches!(m, ServerOpcodeMessage::SMSG_FORCE_RUN_SPEED_CHANGE(_))));
}

#[test]
fn creature_relays_target_and_max_vitals() {
    let mut old = player_entity();
    old.type_mask = 0;
    let mut new = old.clone();
    new.target_guid = 7;
    new.max_power = 50;
    let power_b = (new.unit_bytes_0 >> 24) as u8;
    let out = entity_update_to_outbound(&old, &new);
    assert_eq!(
        out,
        vec![
            ServerOpcodeMessage::SMSG_UPDATE_OBJECT(Box::new(codec::build_target_values(
                new.guid, 7
            ))),
            ServerOpcodeMessage::SMSG_UPDATE_OBJECT(Box::new(codec::build_max_vitals_values(
                new.guid,
                new.max_health,
                power_b,
                50
            ))),
        ]
    );
}

#[test]
fn peer_reveal_restores_remaining_auras_after_create() {
    use codec::update_mask::idx;
    use wow_world_messages::vanilla::{Object, UpdateMask, VisibleItemIndex};

    for mut entity in [player_entity(), creature_entity()] {
        entity.unit_bytes_0 = 0x0000_0101;
        let mut remaining = aura(entity.guid, 0xA1, 1, 10, 0, i64::MAX);
        remaining.spell_id = 1459;
        remaining.level = 40;
        remaining.flags = 9;
        let index = world_view::AuraIndex::default();
        index.upsert(0, &remaining);
        let mut other_shard = remaining.clone();
        other_shard.spell_id = 9999;
        index.upsert(1, &other_shard);

        let mut stealth = aura(entity.guid, A_STEALTH, 0, 0, 0, i64::MAX);
        stealth.id = 2;
        stealth.slot = 0;
        let mut second_stealth = stealth.clone();
        second_stealth.id = 3;
        second_stealth.slot = 1;
        index.upsert(0, &stealth);
        index.upsert(0, &second_stealth);
        index.remove(0, &stealth);
        assert_eq!(index.stealth_count(0, entity.guid), 1);
        index.remove(0, &second_stealth);
        assert_eq!(index.stealth_count(0, entity.guid), 0);

        let view = entity_view(entity, 0);
        let out = peer_create_outbound(
            &view,
            &[codec::ItemInstanceView {
                guid: 700,
                entry: 1337,
                enchantment: 823,
                ..Default::default()
            }],
            &index.on_target(0, view.guid),
            None,
        )
        .unwrap();
        let [Outbound::One(ServerOpcodeMessage::SMSG_UPDATE_OBJECT(create)), Outbound::Raw { opcode, body }] =
            out.as_slice()
        else {
            panic!("a peer needs CREATE followed by its remaining aura array");
        };
        let [Object::CreateObject2 { guid3, mask2, .. }] = create.objects.as_slice() else {
            panic!("the first packet must create the peer");
        };
        assert_eq!(guid3.guid(), view.guid);
        if let UpdateMask::Player(player) = mask2 {
            let visible = player
                .player_visible_item(VisibleItemIndex::Index0)
                .unwrap();
            assert_eq!((visible.item, visible.enchants), (1337, [823, 0]));
        }
        assert_eq!(*opcode, 0x00A9);
        let decoded = lyracore_shared::values_mask::parse_values_updates(body);
        assert_eq!(decoded.len(), 1);
        assert_eq!(decoded[0].guid, view.guid);
        let fields: std::collections::HashMap<_, _> = decoded[0].fields.iter().copied().collect();
        for slot in 0..idx::AURA_SLOTS {
            assert_eq!(
                fields[&(idx::UNIT_AURA + slot)],
                if slot == 3 { 1459 } else { 0 }
            );
        }
        assert_eq!(fields[&idx::UNIT_AURAFLAGS], 0x9000);
        assert_eq!(fields[&idx::UNIT_AURALEVELS], 0x2800_0000);
        assert!(!fields.contains_key(&idx::OBJECT_TYPE));
    }
}

#[test]
fn peer_create_without_auras_needs_no_followup() {
    let mut entity = player_entity();
    entity.unit_bytes_0 = 0x0000_0101;
    let out = peer_create_outbound(&entity_view(entity, 0), &[], &[], None).unwrap();
    assert!(matches!(
        out.as_slice(),
        [Outbound::One(ServerOpcodeMessage::SMSG_UPDATE_OBJECT(_))]
    ));
}

#[test]
fn peer_create_restores_the_wisp_appearance_and_movement_speeds() {
    use lyracore_shared::spell::taxonomy::{A_MOD_SPEED, SPEED_MOVE, SPEED_SWIM};
    use wow_world_messages::vanilla::{Object, UpdateMask};
    let mut entity = player_entity();
    entity.display_id = 1825;
    entity.unit_bytes_0 = 0x0000_0104;
    entity.run_speed_mult_bp = 10_000;
    let view = entity_view(entity, 0);
    let auras = [
        aura(
            view.guid,
            A_MOD_SPEED,
            i32::from(SPEED_MOVE),
            25,
            0,
            i64::MAX,
        ),
        aura(
            view.guid,
            A_MOD_SPEED,
            i32::from(SPEED_MOVE),
            50,
            0,
            i64::MAX,
        ),
        aura(
            view.guid,
            A_MOD_SPEED,
            i32::from(SPEED_SWIM),
            25,
            0,
            i64::MAX,
        ),
        aura(
            view.guid,
            A_MOD_SPEED,
            i32::from(SPEED_SWIM),
            50,
            0,
            i64::MAX,
        ),
    ];
    let out = peer_create_outbound(&view, &[], &auras, None).unwrap();
    let Outbound::One(ServerOpcodeMessage::SMSG_UPDATE_OBJECT(create)) = &out[0] else {
        panic!("peer CREATE");
    };
    let Object::CreateObject2 {
        movement2,
        mask2: UpdateMask::Player(player),
        ..
    } = &create.objects[0]
    else {
        panic!("Character CREATE");
    };
    assert_eq!(player.unit_displayid(), Some(1825));
    let wow_world_messages::vanilla::MovementBlock_UpdateFlag_Living::Living {
        running_speed,
        swimming_speed,
        ..
    } = movement2.update_flag.get_living().unwrap()
    else {
        panic!("living movement");
    };
    assert_eq!(*running_speed, 10.5);
    assert!((*swimming_speed - lyracore_shared::constants::speeds::SWIM * 1.5).abs() < 0.000001);
}

#[test]
fn peer_create_uses_the_remaining_ghost_speed_and_restores_baseline_after_removal() {
    use lyracore_shared::spell::taxonomy::{A_MOD_SPEED, SPEED_MOVE, SPEED_SWIM};
    use wow_world_messages::vanilla::{MovementBlock_UpdateFlag_Living, Object};
    let mut entity = player_entity();
    entity.unit_bytes_0 = 0x0000_0101;
    entity.run_speed_mult_bp = 10_000;
    let view = entity_view(entity, 0);
    let ghost = vec![
        aura(
            view.guid,
            A_MOD_SPEED,
            i32::from(SPEED_MOVE),
            25,
            0,
            i64::MAX,
        ),
        aura(
            view.guid,
            A_MOD_SPEED,
            i32::from(SPEED_SWIM),
            25,
            0,
            i64::MAX,
        ),
    ];
    for (auras, expected_run, expected_swim) in
        [(ghost, 8.75, 5.902_777_7), (vec![], 7.0, 4.722_222)]
    {
        let out = peer_create_outbound(&view, &[], &auras, None).unwrap();
        let Outbound::One(ServerOpcodeMessage::SMSG_UPDATE_OBJECT(create)) = &out[0] else {
            panic!("peer CREATE");
        };
        let Object::CreateObject2 { movement2, .. } = &create.objects[0] else {
            panic!("Character CREATE");
        };
        let MovementBlock_UpdateFlag_Living::Living {
            running_speed,
            swimming_speed,
            ..
        } = movement2.update_flag.get_living().unwrap()
        else {
            panic!("living movement");
        };
        assert_eq!(*running_speed, expected_run);
        assert!((*swimming_speed - expected_swim).abs() < 0.000001);
    }
}

// A 40% slow: 7.0 * 0.6 = 4.2 forward and 4.5 * 0.6 = 2.7 backward.
#[test]
fn peer_create_carries_a_slowed_backward_speed() {
    use lyracore_shared::spell::taxonomy::{A_MOD_SPEED, SPEED_MOVE};
    use wow_world_messages::vanilla::{MovementBlock_UpdateFlag_Living, Object};
    let mut entity = player_entity();
    entity.unit_bytes_0 = 0x0000_0101;
    let view = entity_view(entity, 0);
    let slow = aura(
        view.guid,
        A_MOD_SPEED,
        i32::from(SPEED_MOVE),
        -40,
        0,
        i64::MAX,
    );
    let out = peer_create_outbound(&view, &[], &[slow], None).unwrap();
    let Outbound::One(ServerOpcodeMessage::SMSG_UPDATE_OBJECT(create)) = &out[0] else {
        panic!("peer CREATE");
    };
    let Object::CreateObject2 { movement2, .. } = &create.objects[0] else {
        panic!("Character CREATE");
    };
    let MovementBlock_UpdateFlag_Living::Living {
        running_speed,
        backwards_running_speed,
        ..
    } = movement2.update_flag.get_living().unwrap()
    else {
        panic!("living movement");
    };
    assert!((*running_speed - 4.2).abs() < 1e-5);
    assert!((*backwards_running_speed - 2.7).abs() < 1e-5);
}

#[test]
fn peer_create_preserves_gm_run_speed_without_movement_auras() {
    use wow_world_messages::vanilla::{MovementBlock_UpdateFlag_Living, Object};
    let mut entity = player_entity();
    entity.unit_bytes_0 = 0x0000_0101;
    entity.run_speed_mult_bp = 30_000;
    let view = entity_view(entity, 0);
    let out = peer_create_outbound(&view, &[], &[], None).unwrap();
    let Outbound::One(ServerOpcodeMessage::SMSG_UPDATE_OBJECT(create)) = &out[0] else {
        panic!("peer CREATE");
    };
    let Object::CreateObject2 { movement2, .. } = &create.objects[0] else {
        panic!("Character CREATE");
    };
    let MovementBlock_UpdateFlag_Living::Living { running_speed, .. } =
        movement2.update_flag.get_living().unwrap()
    else {
        panic!("living movement");
    };
    assert_eq!(*running_speed, 21.0);
}

#[test]
fn peer_create_applies_always_and_not_stack_run_bonuses() {
    use lyracore_shared::spell::taxonomy::{A_MOD_SPEED, SPEED_MOVE_ALWAYS, SPEED_MOVE_NOT_STACK};
    use wow_world_messages::vanilla::{MovementBlock_UpdateFlag_Living, Object};
    let mut entity = player_entity();
    entity.unit_bytes_0 = 0x0000_0101;
    let view = entity_view(entity, 0);
    for speed_kind in [SPEED_MOVE_ALWAYS, SPEED_MOVE_NOT_STACK] {
        let auras = [aura(
            view.guid,
            A_MOD_SPEED,
            i32::from(speed_kind),
            50,
            0,
            i64::MAX,
        )];
        let out = peer_create_outbound(&view, &[], &auras, None).unwrap();
        let Outbound::One(ServerOpcodeMessage::SMSG_UPDATE_OBJECT(create)) = &out[0] else {
            panic!("peer CREATE");
        };
        let Object::CreateObject2 { movement2, .. } = &create.objects[0] else {
            panic!("Character CREATE");
        };
        let MovementBlock_UpdateFlag_Living::Living { running_speed, .. } =
            movement2.update_flag.get_living().unwrap()
        else {
            panic!("living movement");
        };
        assert_eq!(*running_speed, 10.5);
    }
}

#[test]
fn peer_create_gate_excludes_self_regardless_of_other_fields() {
    assert!(!peer_create_gate(1, 1, 0, 0, 0, true, false));
}

#[test]
fn peer_create_gate_excludes_a_different_instance_work_item_190() {
    assert!(peer_create_gate(2, 1, 0, 0, 0, false, false));
    assert!(peer_create_gate(2, 1, 7, 7, 0, false, false));
    assert!(!peer_create_gate(2, 1, 7, 0, 0, false, false));
    assert!(!peer_create_gate(2, 1, 3, 7, 0, false, false));
}

#[test]
fn peer_create_gate_spirithealer_and_stealth_gates_unaffected_by_the_refactor() {
    const SPIRITHEALER: u32 = lyracore_shared::constants::npc_flags::SPIRITHEALER;
    assert!(!peer_create_gate(2, 1, 0, 0, SPIRITHEALER, false, false));
    assert!(peer_create_gate(2, 1, 0, 0, SPIRITHEALER, true, false));
    assert!(peer_create_gate(2, 1, 0, 0, 0, false, false));
    assert!(!peer_create_gate(2, 1, 0, 0, 0, false, true));
}

#[test]
fn reentry_after_leaving_aoi_recreates_exactly_once_work_item_144() {
    let self_guid = 1u64;
    let peer = 42u64;
    let mut created: HashSet<u64> = HashSet::from([self_guid, peer]);

    assert!(
        created.remove(&peer),
        "on_delete must evict the leaving peer from `created`"
    );
    assert!(!created.contains(&peer));

    assert!(is_update_reentry(peer, self_guid, created.contains(&peer)));

    assert!(peer_create_gate(peer, self_guid, 0, 0, 0, false, false));
    assert!(
        created.insert(peer),
        "the re-entry offer must (re)insert into `created`"
    );

    assert!(!is_update_reentry(peer, self_guid, created.contains(&peer)));

    assert!(!is_update_reentry(self_guid, self_guid, false));
}

#[test]
fn login_resident_sweep_creates_every_pre_existing_peer_exactly_once_work_item_145() {
    let self_guid = 1u64;
    let residents = [2u64, 3, 4];
    let mut created: HashSet<u64> = HashSet::from([self_guid]);

    let mut creates_sent = 0;
    for guid in residents {
        if peer_create_gate(guid, self_guid, 0, 0, 0, false, false) && created.insert(guid) {
            creates_sent += 1;
        }
    }
    assert_eq!(
        creates_sent, 3,
        "every pre-existing resident must get a CREATE at login, not just self (the login-sweep bug: \
         only self + gameobjects appeared)"
    );
    assert!(residents.iter().all(|g| created.contains(g)));

    creates_sent = 0;
    for guid in residents {
        if peer_create_gate(guid, self_guid, 0, 0, 0, false, false) && created.insert(guid) {
            creates_sent += 1;
        }
    }
    assert_eq!(
        creates_sent, 0,
        "a second sweep over already-created residents must be a no-op"
    );
}

#[test]
fn login_resident_sweep_excludes_cross_instance_residents_work_item_190() {
    let self_guid = 1u64;
    let viewer_instance_id = 0u64;
    let same_instance_peer = 2u64;
    let other_instance_peer = 3u64;
    let mut created: HashSet<u64> = HashSet::from([self_guid]);

    for (guid, row_instance) in [(same_instance_peer, 0u64), (other_instance_peer, 7u64)] {
        if peer_create_gate(
            guid,
            self_guid,
            row_instance,
            viewer_instance_id,
            0,
            false,
            false,
        ) {
            created.insert(guid);
        }
    }
    assert!(created.contains(&same_instance_peer));
    assert!(
        !created.contains(&other_instance_peer),
        "a cross-instance resident must stay excluded"
    );
}

#[test]
fn corpse_and_gameobject_creates_relay_only_within_the_viewers_instance_work_item_190_slice_2() {
    assert!(instance_relay_gate(0, Some(0)));
    assert!(instance_relay_gate(7, Some(7)));
    assert!(
        !instance_relay_gate(7, Some(0)),
        "an open-world viewer must not see instance 7's rows"
    );
    assert!(
        !instance_relay_gate(7, Some(9)),
        "another party's instance must not see them either"
    );
    assert!(!instance_relay_gate(0, Some(7)));
    assert!(!instance_relay_gate(0, None));
    assert!(!instance_relay_gate(7, None));
}
