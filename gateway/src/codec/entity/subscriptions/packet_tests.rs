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

/// A Human Warrior: the CREATE encoder refuses race 0.
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
    // Decode + pin: guid and the exact health value ride the health-only VALUES packet.
    assert_eq!(
        out,
        vec![ServerOpcodeMessage::SMSG_UPDATE_OBJECT(Box::new(
            codec::build_health_values(new.guid, 50)
        ))]
    );
}

/// a sheath change must relay, or drawing a weapon is invisible to everyone but the player
/// who did it. The state lives in BYTE 0 of `unit_bytes_2` — pin the decoded packet, and pin that
/// an unchanged row stays silent (the client re-sends its current state on every weapon swap, and
/// a relay per no-op is a broadcast amplifier on a busy cell).
#[test]
fn sheath_change_relays_and_a_no_op_stays_silent() {
    let old = player_entity();
    let mut new = old.clone();
    new.unit_bytes_2 = 1; // SHEATH_STATE_MELEE — weapons drawn
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
    // health (any unit) + dynamic_flags (any unit) + power (player) = three separate packets,
    // in the relay's fixed emission order — decoded and pinned by guid + value, not just count.
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
    // A ding collapses level + vitals + xp into ONE combined VALUES block, NOT separate health/xp
    // packets — even though health and xp also changed this tick.
    let old = player_entity();
    let mut new = old.clone();
    new.level = 2;
    new.health = 120;
    new.max_health = 120;
    new.xp = 10;
    assert_eq!(entity_update_to_outbound(&old, &new).len(), 1);
}

/// `/afk` sets PLAYER_FLAGS_AFK (0x02, descriptor index 190). Self and every observer run this
/// same diff, so each client holding the object gets one VALUES update carrying the new word.
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
    // A creature (no PLAYER bit) relays health + dynamic_flags but NOT xp/coinage/ghost/power —
    // decoded and pinned as exactly those two packets, not just a count of two.
    let mut old = player_entity();
    old.type_mask = 0;
    let mut new = old.clone();
    new.health = 50; // emits (any unit)
    new.dynamic_flags = 1; // emits (any unit)
    new.xp = 10; // gated out (player-only)
    new.money = 10; // gated out
    new.power = 30; // gated out
    new.player_flags = 0x10; // gated out
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
    // The level-up branch is player-only; a creature whose level changed still diffs health
    // normally. A bare length check can't tell the two branches apart here — the player-only
    // ding ALSO emits exactly one packet — so decode the one packet and pin it as the plain
    // Unit-mask health update (build_health_values), not the Player-mask combined ding
    // (build_levelup_values) a regressed player-gate would otherwise produce with the same count.
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
    // UNIT_FIELD_TARGET relay — any unit, one packet, guid + target value pinned.
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
    // A bought bank bag slot lands in byte 2 of PLAYER_BYTES_2; the relay must carry the FULL
    // field (a partial VALUES overwrites the whole u32) so byte 0 (facial hair) and byte 3 (rest
    // state — HAZARD: a zero byte 3 crashes the 5875 client's XP bar) ride along unchanged.
    let mut old = player_entity();
    old.player_bytes_2 = lyracore_shared::packing::player_bytes_2_with_rest(7, 0, false); // facial hair 7, 0 slots, NORMAL
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
    // Byte 3 changing (rest flip) must NOT trip the byte-2-scoped bank slot relay — that byte
    // already relays through its own dedicated `game_rest_state_event` path.
    let old = player_entity();
    let mut new = old.clone();
    new.player_bytes_2 = lyracore_shared::packing::player_bytes_2_with_rest(0, 0, true); // RESTED
    assert!(entity_update_to_outbound(&old, &new).is_empty());
}

#[test]
fn max_vitals_change_emits_one() {
    // A non-level-up max_health change (gear/aura) — any unit, one packet, guid + max_health/
    // power values pinned (max_power carries through unchanged from `player_entity()`).
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
    const GHOST: u32 = lyracore_shared::constants::player_flags::GHOST; // 0x10
                                                                        // alive→ghost (Release Spirit): REVEAL the in-scope spirit healers (Some(true)).
    assert_eq!(ghost_transition(0, GHOST, GHOST), Some(true));
    // ghost→alive (reclaim corpse / spirit-healer res): DESTROY them (Some(false)).
    assert_eq!(ghost_transition(GHOST, 0, GHOST), Some(false));
    // No GHOST-bit change → no reveal work, even when OTHER player-flags toggle this update
    // (the common heartbeat / unrelated flag write must NOT re-fire the reveal).
    assert_eq!(ghost_transition(0, 0, GHOST), None);
    assert_eq!(ghost_transition(GHOST, GHOST, GHOST), None);
    assert_eq!(ghost_transition(0x4 | GHOST, 0x8 | GHOST, GHOST), None); // ghost stays set, others flip
    assert_eq!(ghost_transition(0x4, 0x8, GHOST), None); // ghost stays clear, others flip
}

#[test]
fn in_combat_flag_change_emits_one() {
    // UNIT_FLAG_IN_COMBAT toggling on unit_flags — any unit, one VALUES packet, guid + the
    // exact post-toggle flags word pinned.
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
    // A land mount changes ONLY `mount_display_id` — TAXI_FLIGHT is untouched — so it must relay
    // through the decoupled single-field builder, not the taxi-coupled one.
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
    new.mount_display_id = 0; // dismount — TAXI_FLIGHT was never set for a land mount
    assert_eq!(
        entity_update_to_outbound(&old, &new),
        vec![ServerOpcodeMessage::SMSG_UPDATE_OBJECT(Box::new(
            codec::build_mount_display_values(new.guid, 0)
        ))]
    );
}

#[test]
fn a_land_mount_display_change_alongside_an_unrelated_flag_change_emits_both_packets() {
    // The flag change is real but NOT the taxi flight bit — both relay, separately, neither
    // coupled through the taxi builder.
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
    // GM playtest `.speed`: a run_speed_mult_bp change relays
    // SMSG_FORCE_RUN_SPEED_CHANGE with speed = BASE_RUN_SPEED (7.0) * bp/10000 — pin the derived
    // value end-to-end (this is the "codec pin for the speed message" this feature calls for).
    let old = player_entity();
    let mut new = old.clone();
    new.run_speed_mult_bp = 30_000; // .speed 3 -> 3x
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
    new.health = 50; // an unrelated change must not spuriously trigger the speed relay
    let out = entity_update_to_outbound(&old, &new);
    assert!(!out
        .iter()
        .any(|m| matches!(m, ServerOpcodeMessage::SMSG_FORCE_RUN_SPEED_CHANGE(_))));
}

#[test]
fn creature_relays_target_and_max_vitals() {
    // The target + max-vitals arms are NOT player-gated: a creature relays both, decoded and
    // pinned in emission order (target before max-vitals) with their exact values.
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

// ---- AOI re-entry, initial-apply, instance isolation --------------
//
// `offer_peer_create`/`is_update_reentry` themselves need a live SDK connection (`RemoteTables`
// is only constructible off a real subscription) to exercise end-to-end, so — per this repo's
// established convention (`ghost_transition`, `stealth_action`, `entity_update_to_outbound`) —
// the DECISION logic they're built from is extracted pure (`peer_create_gate`,
// `is_update_reentry`) and tested directly here; both are the ACTUAL functions
// `offer_peer_create`/the `on_update` callback call, not a parallel re-implementation.

#[test]
fn peer_create_gate_excludes_self_regardless_of_other_fields() {
    // Self is never offered a CREATE of its own row (the dedup set is pre-seeded with
    // self_guid instead) — true even if every other gate would otherwise pass.
    assert!(!peer_create_gate(1, 1, 0, 0, 0, true, false));
}

#[test]
fn peer_create_gate_excludes_a_different_instance_work_item_190() {
    // Same instance (both open-world 0, or both some future dungeon instance N) -> visible.
    assert!(peer_create_gate(2, 1, 0, 0, 0, false, false));
    assert!(peer_create_gate(2, 1, 7, 7, 0, false, false));
    // Different instance (e.g. a dungeon party's row vs. an open-world viewer, or two
    // different parties' instances of the same dungeon) -> excluded. This is the gate the
    // The login sweep and the re-entry path BOTH sit behind — a cross-instance row must
    // never be marked `created`, even transiently, by either path.
    assert!(!peer_create_gate(2, 1, 7, 0, 0, false, false));
    assert!(!peer_create_gate(2, 1, 3, 7, 0, false, false));
}

#[test]
fn peer_create_gate_spirithealer_and_stealth_gates_unaffected_by_the_refactor() {
    const SPIRITHEALER: u32 = lyracore_shared::constants::npc_flags::SPIRITHEALER;
    // A spirit healer is invisible to a living viewer...
    assert!(!peer_create_gate(2, 1, 0, 0, SPIRITHEALER, false, false));
    assert!(peer_create_gate(2, 1, 0, 0, SPIRITHEALER, true, false));
    // A non-spirit-healer entity is unaffected by the viewer's ghost state.
    assert!(peer_create_gate(2, 1, 0, 0, 0, false, false));
    // A currently-stealthed peer is excluded regardless of everything else.
    assert!(!peer_create_gate(2, 1, 0, 0, 0, false, true));
}

#[test]
fn reentry_after_leaving_aoi_recreates_exactly_once_work_item_144() {
    // The exact re-entry scenario: observer sees peer (CREATE) -> peer leaves the AOI box
    // (on_delete evicts it from `created`, sends DESTROY) -> peer returns, delivered as an
    // UPDATE of the SDK's still-cached row (not a fresh on_insert) -> the observer must
    // re-CREATE it, not silently drop it forever.
    let self_guid = 1u64;
    let peer = 42u64;
    let mut created: HashSet<u64> = HashSet::from([self_guid, peer]); // currently visible

    // Leave: on_delete's `if del_created.lock().unwrap().remove(&row.guid)` — must actually
    // evict, or the later re-entry check below sees a false "already shown".
    assert!(
        created.remove(&peer),
        "on_delete must evict the leaving peer from `created`"
    );
    assert!(!created.contains(&peer));

    // Re-entry arrives as an on_update, not on_insert (the SDK's refcounted cache still holds
    // the row). The callback's actual dispatch predicate must say "treat as re-entry".
    assert!(is_update_reentry(peer, self_guid, created.contains(&peer)));

    // `offer_peer_create`'s gate passes (same instance, not stealthed, not a spirit healer) ->
    // the dedup insert succeeds -> exactly one CREATE goes out.
    assert!(peer_create_gate(peer, self_guid, 0, 0, 0, false, false));
    assert!(
        created.insert(peer),
        "the re-entry offer must (re)insert into `created`"
    );

    // A THIRD sighting of the same still-visible peer (an ordinary field update, e.g. a
    // heartbeat) must NOT be treated as another re-entry — it's already shown, so it diffs
    // normally instead of re-CREATEing (which would desync the client).
    assert!(!is_update_reentry(peer, self_guid, created.contains(&peer)));

    // Self never triggers the re-entry branch, even if (hypothetically) it were ever evicted.
    assert!(!is_update_reentry(self_guid, self_guid, false));
}

#[test]
fn login_resident_sweep_creates_every_pre_existing_peer_exactly_once_work_item_145() {
    // The login-sweep scenario: a fresh login's AOI box already contains peers/creatures (rows
    // resident in the connection's cache from the just-applied subscription) whose
    // `on_insert` may or may not have fired. The sweep in `subscribe_player_events` offers
    // every resident row through this SAME gate + dedup — model it directly.
    let self_guid = 1u64;
    let residents = [2u64, 3, 4]; // pre-existing peers already in the login AOI box
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

    // Idempotency (the sweep's own correctness claim): if the SDK ALSO fired an apply-time
    // on_insert for one resident before the sweep runs, the sweep must not double-create it.
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
    // A row resident in the connection's cache from a DIFFERENT instance than the viewer
    // (always a no-op today since every entity is instance 0, but the
    // gate must already hold the line) must never be swept into `created`.
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
    // Open-world rows to an open-world viewer: byte-identical to the previous behavior (always relayed).
    assert!(instance_relay_gate(0, Some(0)));
    // A per-instance GO copy / in-instance corpse reaches ONLY viewers inside that instance…
    assert!(instance_relay_gate(7, Some(7)));
    assert!(
        !instance_relay_gate(7, Some(0)),
        "an open-world viewer must not see instance 7's rows"
    );
    assert!(
        !instance_relay_gate(7, Some(9)),
        "another party's instance must not see them either"
    );
    // …and an instanced viewer no longer receives the static instance-0 originals (they are the
    // COPY SOURCES on the dungeon map — the copies are what render inside the run).
    assert!(!instance_relay_gate(0, Some(7)));
    // Unknown viewer instance (entity not resident — the AOI-on login window) relays NOTHING,
    // not even instance 0: suppress-then-sweep, never guess.
    assert!(!instance_relay_gate(0, None));
    assert!(!instance_relay_gate(7, None));
}
