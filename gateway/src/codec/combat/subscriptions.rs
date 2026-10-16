use lyracore_shared::spell::taxonomy::{A_MOD_RESISTANCE, A_MOD_SPEED, SPEED_SWIM};

use crate::codec::entity::subscriptions::build_peer_create;
use crate::codec::{self};
use crate::stdb::bindings::*;
use crate::stdb::world_index::{CellKey, EntityLayer, ShardId};
use crate::stdb::world_view::*;
use crate::stdb::Coordinator;
use crate::world::Outbound;
use lyracore_shared::spatial::BOX_HALF_SPAN;
use spacetimedb_sdk::Table;
use std::collections::HashSet;
use std::sync::{Arc, Mutex, OnceLock};
use wow_world_messages::vanilla::opcodes::ServerOpcodeMessage;

pub(crate) use lyracore_shared::spell::taxonomy::A_STEALTH;

/// SDK callbacks see the whole row delta, so multiple stealth changes share one count.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum StealthAction {
    Hide,
    Reveal,
    None,
}

pub(crate) fn stealth_action(is_insert: bool, post_change_count: usize) -> StealthAction {
    match (is_insert, post_change_count) {
        (true, n) if n >= 1 => StealthAction::Hide,
        (false, 0) => StealthAction::Reveal,
        _ => StealthAction::None,
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum EventAudience {
    Everyone,
    Source,
    Peers,
}

impl EventAudience {
    fn includes(self, is_source: bool) -> bool {
        match self {
            Self::Everyone => true,
            Self::Source => is_source,
            Self::Peers => !is_source,
        }
    }
}

#[derive(Default)]
struct SharedEventPackets(
    OnceLock<std::io::Result<Vec<(EventAudience, crate::world::egress::Packet)>>>,
);

impl SharedEventPackets {
    fn outbound(
        &self,
        viewer: &Viewer,
        source_guid: u64,
        messages: impl FnOnce() -> Vec<(EventAudience, Outbound)>,
    ) -> Vec<Outbound> {
        let encoded = self.0.get_or_init(|| {
            let mut packets = Vec::new();
            for (audience, message) in messages() {
                match crate::world::egress::encode(vec![message]) {
                    Ok(encoded) => {
                        packets.extend(encoded.into_iter().map(|packet| (audience, packet)))
                    }
                    Err(error) => {
                        log::error!(
                            "Relay packet encoding failed for source {source_guid}: {error}"
                        );
                        return Err(error);
                    }
                }
            }
            Ok(packets)
        });
        match encoded {
            Ok(packets) => packets
                .iter()
                .filter(|(audience, _)| audience.includes(viewer.self_guid == source_guid))
                .map(|(_, packet)| packet.outbound())
                .collect(),
            Err(_) => {
                viewer.tx.close();
                Vec::new()
            }
        }
    }
}

#[cfg(test)]
fn combat_event_outbound(
    created: &Mutex<HashSet<u64>>,
    self_guid: u64,
    row: &CombatEvent,
) -> Vec<Outbound> {
    if !created.lock().unwrap().contains(&row.attacker_guid) {
        return Vec::new();
    }
    combat_event_messages(row)
        .into_iter()
        .filter(|(audience, _)| audience.includes(self_guid == row.attacker_guid))
        .map(|(_, out)| out)
        .collect()
}

fn combat_event_messages(row: &CombatEvent) -> Vec<(EventAudience, Outbound)> {
    let mut out = Vec::new();
    // A ranged launch uses SPELL_GO; ATTACKERSTATEUPDATE would animate a melee swing.
    if row.ranged_spell_id != 0 {
        // 24 is INVTYPE_AMMO; a zero ammo display leaves wand bolts without the AMMO flag.
        let ammo = if row.ammo_display_id != 0 {
            Some((row.ammo_display_id, 24))
        } else {
            None
        };
        let miss = row.hit_info == 2;
        let go = codec::build_spell_go_outcome(
            row.attacker_guid,
            row.ranged_spell_id,
            row.target_guid,
            ammo,
            miss,
        );
        out.push((
            EventAudience::Everyone,
            Outbound::One(ServerOpcodeMessage::SMSG_SPELL_GO(Box::new(go))),
        ));
    } else if !row.spell_swing {
        let m = codec::build_attacker_state_update(
            row.attacker_guid,
            row.target_guid,
            row.damage,
            row.hit_info,
            row.blocked_amount,
            0,
        );
        out.push((
            EventAudience::Everyone,
            Outbound::One(ServerOpcodeMessage::SMSG_ATTACKERSTATEUPDATE(Box::new(m))),
        ));
    }
    if row.killing_blow && row.ranged_spell_id == 0 {
        let stop = codec::build_attack_stop(row.attacker_guid, row.target_guid);
        out.push((
            EventAudience::Everyone,
            Outbound::One(ServerOpcodeMessage::SMSG_ATTACKSTOP(Box::new(stop))),
        ));
    }
    // Killer only; docs/benilla-compatibility.md "Combat feedback" lists the missing audiences.
    if row.killing_blow && !is_creature_guid(row.attacker_guid) {
        use wow_world_messages::vanilla::SMSG_PARTYKILLLOG;
        out.push((
            EventAudience::Source,
            Outbound::One(ServerOpcodeMessage::SMSG_PARTYKILLLOG(Box::new(
                SMSG_PARTYKILLLOG {
                    player_with_killing_blow: wow_world_messages::Guid::new(row.attacker_guid),
                    victim: wow_world_messages::Guid::new(row.target_guid),
                },
            ))),
        ));
    }
    out
}

pub(crate) fn melee_engage_outbound(
    created: &Mutex<HashSet<u64>>,
    row: &MeleeAttack,
    live: bool,
) -> Vec<Outbound> {
    if row.ranged_spell_id != 0 {
        return Vec::new();
    }
    if !created.lock().unwrap().contains(&row.attacker_guid) {
        return Vec::new();
    }
    let mut out = Vec::new();
    // Live inserts only: a reconnect replays the stance, not the aggro sound.
    if live && is_creature_guid(row.attacker_guid) {
        out.push(Outbound::One(ServerOpcodeMessage::SMSG_AI_REACTION(
            Box::new(codec::build_ai_reaction_hostile(row.attacker_guid)),
        )));
    }
    let m = codec::build_attack_start(row.attacker_guid, row.target_guid);
    out.push(Outbound::One(ServerOpcodeMessage::SMSG_ATTACKSTART(
        Box::new(m),
    )));
    out
}

/// `HIGHGUID_UNIT` 0xF130 or `HIGHGUID_PET` 0xF140.
fn is_creature_guid(guid: u64) -> bool {
    matches!(guid >> 48, 0xF130 | 0xF140)
}

pub(crate) fn melee_disengage_outbound(self_guid: u64, row: &MeleeAttack) -> Vec<Outbound> {
    if row.ranged_spell_id != 0 {
        if row.attacker_guid == self_guid {
            return vec![Outbound::One(ServerOpcodeMessage::SMSG_CANCEL_AUTO_REPEAT)];
        }
        return Vec::new();
    }
    let m = codec::build_attack_stop(row.attacker_guid, row.target_guid);
    vec![Outbound::One(ServerOpcodeMessage::SMSG_ATTACKSTOP(
        Box::new(m),
    ))]
}

#[cfg(test)]
fn cast_event_outbound(self_guid: u64, row: &SpellCastEvent) -> Vec<Outbound> {
    cast_event_messages(row)
        .into_iter()
        .filter(|(audience, _)| audience.includes(self_guid == row.caster_guid))
        .map(|(_, out)| out)
        .collect()
}

fn cast_event_messages(row: &SpellCastEvent) -> Vec<(EventAudience, Outbound)> {
    let mut out = Vec::new();
    let Some(kind) = SpellCastEventKind::decode(row) else {
        return out;
    };
    if kind == SpellCastEventKind::Interrupt {
        // Character SPELL_FAILURE is caster-private so bystanders keep their own cast state.
        let failure_audience = if is_creature_guid(row.caster_guid) {
            EventAudience::Everyone
        } else {
            EventAudience::Source
        };
        out.push((
            failure_audience,
            Outbound::One(ServerOpcodeMessage::SMSG_SPELL_FAILURE(Box::new(
                codec::build_spell_failure(row.caster_guid, row.spell_id),
            ))),
        ));
        if failure_audience == EventAudience::Source {
            out.push((
                EventAudience::Peers,
                Outbound::One(ServerOpcodeMessage::SMSG_SPELL_FAILED_OTHER(Box::new(
                    codec::build_spell_failed_other(row.caster_guid, row.spell_id),
                ))),
            ));
        }
        // Teardown must precede the failed cast result to clear both cast and melee-spell state.
        if let Some(reason) = codec::cast_failure_reason_for_code(row.failure_reason) {
            out.push((
                EventAudience::Source,
                Outbound::Raw {
                    opcode: 0x0130,
                    body: codec::build_cast_result_failed(row.spell_id, reason),
                },
            ));
        }
        return out;
    }
    if kind == SpellCastEventKind::Pushback {
        let m = codec::build_spell_delayed(row.caster_guid, row.delay_ms);
        out.push((
            EventAudience::Everyone,
            Outbound::One(ServerOpcodeMessage::SMSG_SPELL_DELAYED(Box::new(m))),
        ));
        return out;
    }
    if kind == SpellCastEventKind::ProcLog {
        if row.damage > 0 {
            let log = codec::build_spell_non_melee_damage_log(
                row.target_guid,
                row.caster_guid,
                row.spell_id,
                row.damage,
                row.school,
                row.is_crit,
                row.resisted,
                row.absorbed,
            );
            out.push((
                EventAudience::Everyone,
                Outbound::One(ServerOpcodeMessage::SMSG_SPELLNONMELEEDAMAGELOG(Box::new(
                    log,
                ))),
            ));
        }
        return out;
    }
    if kind == SpellCastEventKind::PeriodicLog {
        out.extend(periodic_log_outbound(row).map(|out| (EventAudience::Everyone, out)));
        return out;
    }
    if kind == SpellCastEventKind::Start {
        let start =
            codec::build_spell_start(row.caster_guid, row.spell_id, row.cast_time_ms, 0, None);
        out.push((
            EventAudience::Everyone,
            Outbound::One(ServerOpcodeMessage::SMSG_SPELL_START(Box::new(start))),
        ));
        return out;
    }
    let launch_audience = if row.client_initiated {
        EventAudience::Peers
    } else {
        EventAudience::Everyone
    };
    if !row.is_completion {
        let start = codec::build_spell_start(row.caster_guid, row.spell_id, 0, 0, None);
        out.push((
            launch_audience,
            Outbound::One(ServerOpcodeMessage::SMSG_SPELL_START(Box::new(start))),
        ));
    }
    // Timed completions need caster-only CAST_RESULT before GO; instant casts already sent it.
    if row.is_completion && !row.client_initiated {
        out.push((
            EventAudience::Source,
            Outbound::Raw {
                opcode: 0x0130,
                body: codec::build_cast_result_ok(row.spell_id),
            },
        ));
    }
    let mut go = codec::build_spell_go(row.caster_guid, row.spell_id, row.target_guid, None);
    if row.is_completion && row.damage == 0 {
        use wow_world_messages::vanilla::{SpellMiss, SpellMissInfo};
        let miss_info = match row.swing_hit_info {
            2 => Some(SpellMissInfo::Miss),
            3 => Some(SpellMissInfo::Dodge),
            4 => Some(SpellMissInfo::Parry),
            _ => None,
        };
        if let Some(mi) = miss_info {
            go.hits.clear();
            go.misses = vec![SpellMiss {
                target: wow_world_messages::Guid::new(row.target_guid),
                miss_info: mi,
            }];
        }
    }
    if let Some(mi) = cast_miss_info(row.miss_info) {
        use wow_world_messages::vanilla::SpellMiss;
        go.hits.clear();
        go.misses = vec![SpellMiss {
            target: wow_world_messages::Guid::new(row.target_guid),
            miss_info: mi,
        }];
    }
    out.push((
        launch_audience,
        Outbound::One(ServerOpcodeMessage::SMSG_SPELL_GO(Box::new(go))),
    ));
    if row.client_initiated {
        if let Some(mi) = cast_miss_info(row.miss_info) {
            // The cast handler already sent this caster a GO listing the target as hit.
            out.push((
                EventAudience::Source,
                Outbound::Raw {
                    opcode: codec::SMSG_SPELLLOGMISS_OPCODE,
                    body: codec::build_spell_log_miss(
                        row.caster_guid,
                        row.spell_id,
                        row.target_guid,
                        mi.as_int(),
                    ),
                },
            ));
        }
    }

    if row.damage > 0 {
        let log = codec::build_spell_non_melee_damage_log(
            row.target_guid,
            row.caster_guid,
            row.spell_id,
            row.damage,
            row.school,
            row.is_crit,
            row.resisted,
            row.absorbed,
        );
        out.push((
            EventAudience::Everyone,
            Outbound::One(ServerOpcodeMessage::SMSG_SPELLNONMELEEDAMAGELOG(Box::new(
                log,
            ))),
        ));
    }
    if row.healed > 0 {
        use wow_world_messages::vanilla::SMSG_SPELLHEALLOG;
        use wow_world_messages::Guid;
        out.push((
            EventAudience::Everyone,
            Outbound::One(ServerOpcodeMessage::SMSG_SPELLHEALLOG(Box::new(
                SMSG_SPELLHEALLOG {
                    victim: Guid::new(row.target_guid),
                    caster: Guid::new(row.caster_guid),
                    id: row.spell_id,
                    damage: row.healed,
                    critical: row.is_crit,
                },
            ))),
        ));
    }

    if row.energized > 0 {
        out.push((
            EventAudience::Everyone,
            Outbound::Raw {
                opcode: codec::SMSG_SPELLENERGIZELOG_OPCODE,
                body: codec::build_spell_energize_log(
                    row.target_guid,
                    row.caster_guid,
                    row.spell_id,
                    row.power_type,
                    row.energized,
                ),
            },
        ));
    }

    // SMSG_SPELL_GO releases pending casts. A zero-duration cooldown can leave the button stuck.
    if row.cooldown_ms > 0 {
        let cd = codec::build_spell_cooldown(row.caster_guid, row.spell_id, row.cooldown_ms);
        out.push((
            EventAudience::Everyone,
            Outbound::One(ServerOpcodeMessage::SMSG_SPELL_COOLDOWN(Box::new(cd))),
        ));
    }
    out
}

/// Codes are lockstep with `CAST_MISS_*` in `module/src/spell/tables.rs`.
fn cast_miss_info(code: u8) -> Option<wow_world_messages::vanilla::SpellMissInfo> {
    use wow_world_messages::vanilla::SpellMissInfo;
    const CAST_MISS_MISS: u8 = 1;
    const CAST_MISS_RESIST: u8 = 2;
    match code {
        CAST_MISS_MISS => Some(SpellMissInfo::Miss),
        CAST_MISS_RESIST => Some(SpellMissInfo::Resist),
        _ => None,
    }
}

fn periodic_log_outbound(row: &SpellCastEvent) -> Option<Outbound> {
    let tick = if row.healed > 0 {
        codec::PeriodicTick::Heal { amount: row.healed }
    } else if row.energized > 0 {
        codec::PeriodicTick::Energize {
            power_type: row.power_type,
            amount: row.energized,
        }
    } else if row.damage > 0 || row.absorbed > 0 || row.resisted > 0 {
        codec::PeriodicTick::Damage {
            damage: row.damage,
            school: row.school,
            absorbed: row.absorbed,
            resisted: row.resisted,
        }
    } else {
        return None;
    };
    Some(Outbound::Raw {
        opcode: codec::SMSG_PERIODICAURALOG_OPCODE,
        body: codec::build_periodic_aura_log(row.target_guid, row.caster_guid, row.spell_id, tick),
    })
}

pub(crate) fn aura_sync(
    auras: impl Iterator<Item = crate::stdb::bindings::Aura>,
    target_guid: u64,
) -> Outbound {
    let slots: Vec<codec::update_mask::AuraSlot> = auras
        .filter(|a| a.target_guid == target_guid)
        .map(|a| codec::update_mask::AuraSlot {
            slot: a.slot,
            spell_id: a.spell_id,
            flags: a.flags,
            level: a.level,
        })
        .collect();
    let mask = codec::update_mask::full_aura_mask(&slots);
    let (opcode, body) = codec::build_values_update_raw(target_guid, &mask);
    Outbound::Raw { opcode, body }
}

pub(crate) fn armor_packet(
    coord: &Coordinator,
    changed: &Aura,
    self_guid: u64,
) -> Option<Outbound> {
    const RESISTANCE_MASK: u32 = 0x7f;
    if changed.target_guid != self_guid
        || changed.eff_kind != A_MOD_RESISTANCE
        || (changed.eff_p_0 as u32 & RESISTANCE_MASK) == 0
    {
        return None;
    }
    let guard = coord.0.coord();
    let eff = crate::stdb::armor::effective_resistances(&guard, self_guid);
    let pos = crate::stdb::armor::aura_armor_positive(&guard, self_guid);
    Some(Outbound::One(ServerOpcodeMessage::SMSG_UPDATE_OBJECT(
        Box::new(codec::build_armor_values(self_guid, eff, pos)),
    )))
}

pub(crate) fn sheet_packet(
    coord: &Coordinator,
    changed: &Aura,
    self_guid: u64,
) -> Option<Outbound> {
    let moves_sheet = lyracore_shared::aura::moves_sheet(changed.eff_kind, changed.eff_p_0);
    if changed.target_guid != self_guid || !moves_sheet {
        return None;
    }
    let guard = coord.0.coord();
    let db = &guard.conn.db;
    let st = crate::stdb::armor::sheet_stats(db, self_guid);
    drop(guard);
    st.map(|st| {
        Outbound::One(ServerOpcodeMessage::SMSG_UPDATE_OBJECT(Box::new(
            codec::build_sheet_stats_values(self_guid, &st),
        )))
    })
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn stealth_visibility(
    post_change_stealth_count: usize,
    view: &WorldView,
    shard: crate::stdb::world_index::ShardId,
    session: u64,
    coord: &Coordinator,
    created: &Arc<Mutex<HashSet<u64>>>,
    changed: &Aura,
    self_guid: u64,
    is_insert: bool,
) -> Vec<Outbound> {
    if changed.eff_kind != A_STEALTH || changed.target_guid == self_guid {
        return Vec::new();
    }
    match stealth_action(is_insert, post_change_stealth_count) {
        StealthAction::Hide => {
            if created.lock().unwrap().remove(&changed.target_guid) {
                vec![Outbound::One(ServerOpcodeMessage::SMSG_DESTROY_OBJECT(
                    codec::build_destroy_object(changed.target_guid),
                ))]
            } else {
                Vec::new()
            }
        }
        StealthAction::Reveal => {
            if !created.lock().unwrap().insert(changed.target_guid) {
                return Vec::new();
            }

            if !view
                .spatial
                .can_see(EntityLayer::WorldEntity, session, changed.target_guid)
            {
                created.lock().unwrap().remove(&changed.target_guid);
                return Vec::new();
            }
            match coord
                .0
                .coord()
                .conn
                .db
                .game_world_entity()
                .guid()
                .find(&changed.target_guid)
            {
                Some(row) => match build_peer_create(coord, view, shard, self_guid, &row) {
                    Some(out) => out,
                    None => {
                        // Undo dedup on failure so CREATE can retry.
                        created.lock().unwrap().remove(&changed.target_guid);
                        Vec::new()
                    }
                },
                None => {
                    created.lock().unwrap().remove(&changed.target_guid);
                    Vec::new()
                }
            }
        }
        StealthAction::None => Vec::new(),
    }
}

pub(crate) fn spell_channel_outbound(
    coord: &Coordinator,
    self_guid: u64,
    row: &Aura,
    removed: bool,
) -> Option<Outbound> {
    use wow_world_messages::vanilla::{MSG_CHANNEL_START_Server, MSG_CHANNEL_UPDATE_Server};
    if row.caster_guid != self_guid {
        return None;
    }
    let guard = coord.0.coord();
    let db = &guard.conn.db;
    let is_channel = |aura: &Aura| {
        let flags = db
            .game_spell()
            .spell_id()
            .find(&aura.spell_id)
            .map_or(0, |spell| spell.cast_flags);
        lyracore_shared::spell::is_channel_aura(aura.eff_kind, flags)
    };
    if !is_channel(row) {
        return None;
    }
    let active = db
        .game_aura()
        .iter()
        .filter(|aura| aura.caster_guid == self_guid && is_channel(aura))
        .min_by_key(|aura| aura.id);
    let packet = if removed {
        if active.is_some() {
            return None;
        }
        ServerOpcodeMessage::MSG_CHANNEL_UPDATE(MSG_CHANNEL_UPDATE_Server { time: 0 })
    } else {
        if active.as_ref().is_none_or(|active| active.id != row.id) {
            return None;
        }
        let duration = ((row.expires_at.to_micros_since_unix_epoch()
            - row.applied_at.to_micros_since_unix_epoch())
            / 1000)
            .clamp(0, u32::MAX as i64) as u32;
        ServerOpcodeMessage::MSG_CHANNEL_START(MSG_CHANNEL_START_Server {
            spell: row.spell_id,
            duration,
        })
    };
    Some(Outbound::One(packet))
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn aura_insert_outbound(
    coord: &Coordinator,
    view: &WorldView,
    shard: crate::stdb::world_index::ShardId,
    session: u64,
    created: &Arc<Mutex<HashSet<u64>>>,
    self_guid: u64,
    row: &Aura,
    stealth_count: usize,
) -> Vec<Outbound> {
    let mut out = Vec::new();
    out.extend(spell_channel_outbound(coord, self_guid, row, false));
    // VALUES for a stealth-hidden, destroyed object can crash or desynchronize the client.
    let visible =
        row.target_guid == self_guid || created.lock().unwrap().contains(&row.target_guid);
    let current = visible.then(|| view.auras.on_target(shard, row.target_guid));
    if let Some(current) = &current {
        out.push(aura_sync(current.iter().cloned(), row.target_guid));
    }
    if let Some(o) = aura_duration_packet(row, self_guid) {
        out.push(o);
    }
    if let Some(current) = current {
        out.extend(movement_outbound(coord, &current, row, self_guid));
        if let Some(o) = swim_speed_packet(current.iter().cloned(), row, self_guid) {
            out.push(o);
        }
        if let Some(o) = run_speed_packet(current.into_iter(), row, self_guid) {
            out.push(o);
        }
        if let Some(o) = armor_packet(coord, row, self_guid) {
            out.push(o);
        }
        if let Some(o) = sheet_packet(coord, row, self_guid) {
            out.push(o);
        }
    }
    out.extend(stealth_visibility(
        stealth_count,
        view,
        shard,
        session,
        coord,
        created,
        row,
        self_guid,
        true,
    ));
    out
}

pub(crate) fn aura_update_outbound(
    coord: &Coordinator,
    view: &WorldView,
    shard: crate::stdb::world_index::ShardId,
    created: &Arc<Mutex<HashSet<u64>>>,
    self_guid: u64,
    row: &Aura,
    expires_changed: bool,
) -> Vec<Outbound> {
    let mut out = Vec::new();
    let visible =
        row.target_guid == self_guid || created.lock().unwrap().contains(&row.target_guid);
    let current = visible.then(|| view.auras.on_target(shard, row.target_guid));
    if let Some(current) = &current {
        out.push(aura_sync(current.iter().cloned(), row.target_guid));
    }
    if expires_changed {
        if let Some(o) = aura_duration_packet(row, self_guid) {
            out.push(o);
        }
    }
    if let Some(current) = current {
        out.extend(movement_outbound(coord, &current, row, self_guid));
        if let Some(o) = swim_speed_packet(current.iter().cloned(), row, self_guid) {
            out.push(o);
        }
        if let Some(o) = run_speed_packet(current.into_iter(), row, self_guid) {
            out.push(o);
        }
        if let Some(o) = armor_packet(coord, row, self_guid) {
            out.push(o);
        }
        if let Some(o) = sheet_packet(coord, row, self_guid) {
            out.push(o);
        }
    }
    out
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn aura_delete_outbound(
    coord: &Coordinator,
    view: &WorldView,
    shard: crate::stdb::world_index::ShardId,
    session: u64,
    created: &Arc<Mutex<HashSet<u64>>>,
    self_guid: u64,
    row: &Aura,
    stealth_count: usize,
) -> Vec<Outbound> {
    let mut out = Vec::new();
    out.extend(spell_channel_outbound(coord, self_guid, row, true));
    let visible =
        row.target_guid == self_guid || created.lock().unwrap().contains(&row.target_guid);
    if visible {
        let current = view.auras.on_target(shard, row.target_guid);
        out.push(aura_sync(current.iter().cloned(), row.target_guid));
        out.extend(movement_outbound(coord, &current, row, self_guid));
        if let Some(o) = swim_speed_packet(current.iter().cloned(), row, self_guid) {
            out.push(o);
        }
        if let Some(o) = run_speed_packet(current.into_iter(), row, self_guid) {
            out.push(o);
        }
        if let Some(o) = armor_packet(coord, row, self_guid) {
            out.push(o);
        }
        if let Some(o) = sheet_packet(coord, row, self_guid) {
            out.push(o);
        }
    }
    out.extend(stealth_visibility(
        stealth_count,
        view,
        shard,
        session,
        coord,
        created,
        row,
        self_guid,
        false,
    ));
    out
}

pub(crate) fn aura_snapshot_outbound(
    coord: &Coordinator,
    view: &WorldView,
    shard: crate::stdb::world_index::ShardId,
    viewer: &Viewer,
    target_guid: u64,
    previous: &[Aura],
) -> Vec<Outbound> {
    let current = view.auras.on_target(shard, target_guid);
    let stealth_count = current
        .iter()
        .filter(|row| row.eff_kind == A_STEALTH)
        .count();
    let mut out = Vec::new();
    if let Some(stealth) = current
        .iter()
        .chain(previous)
        .find(|row| row.eff_kind == A_STEALTH)
    {
        out.extend(stealth_visibility(
            stealth_count,
            view,
            shard,
            viewer.session,
            coord,
            &viewer.created,
            stealth,
            viewer.self_guid,
            stealth_count > 0,
        ));
        // Reveal includes the aura array; hide must not send VALUES after DESTROY.
        if !out.is_empty() {
            return out;
        }
    }
    if target_guid != viewer.self_guid && !viewer.created.lock().unwrap().contains(&target_guid) {
        return out;
    }
    out.push(aura_sync(current.iter().cloned(), target_guid));
    let changed: Vec<&Aura> = current.iter().chain(previous).collect();
    out.extend(
        crate::codec::movement::subscriptions::aura_movement_from_cache(
            coord,
            &current,
            &changed,
            target_guid,
            viewer.self_guid,
        ),
    );
    if target_guid != viewer.self_guid {
        return out;
    }
    out.extend(
        current
            .iter()
            .filter_map(|row| aura_duration_packet(row, viewer.self_guid)),
    );
    out.extend(
        current
            .iter()
            .chain(previous)
            .find_map(|row| run_speed_packet(current.iter().cloned(), row, viewer.self_guid)),
    );
    out.extend(
        current
            .iter()
            .chain(previous)
            .find_map(|row| swim_speed_packet(current.iter().cloned(), row, viewer.self_guid)),
    );
    out.extend(
        current
            .iter()
            .chain(previous)
            .find_map(|row| armor_packet(coord, row, viewer.self_guid)),
    );
    out.extend(
        current
            .iter()
            .chain(previous)
            .find_map(|row| sheet_packet(coord, row, viewer.self_guid)),
    );
    out
}

pub(crate) fn impact_event_outbound(row: &SpellImpactEvent) -> Vec<Outbound> {
    if row.damage == 0 {
        return Vec::new();
    }
    let log = codec::build_spell_non_melee_damage_log(
        row.target_guid,
        row.caster_guid,
        row.spell_id,
        row.damage,
        row.school,
        row.is_crit,
        row.resisted,
        row.absorbed,
    );
    vec![Outbound::One(
        ServerOpcodeMessage::SMSG_SPELLNONMELEEDAMAGELOG(Box::new(log)),
    )]
}

pub(crate) fn relay_dynobj_create(row: &DynamicObject) -> Vec<Outbound> {
    let m = codec::build_dynamicobject_create_object(
        row.guid,
        row.caster_guid,
        row.spell_id,
        row.x,
        row.y,
        row.z,
        row.radius_yd,
    );
    vec![Outbound::One(ServerOpcodeMessage::SMSG_UPDATE_OBJECT(
        Box::new(m),
    ))]
}

pub(crate) fn relay_destroy_object(guid: u64) -> Vec<Outbound> {
    vec![Outbound::One(ServerOpcodeMessage::SMSG_DESTROY_OBJECT(
        codec::build_destroy_object(guid),
    ))]
}

/// Aura-array fields omit duration; only the owner's aura slots accept timer packets.
pub(crate) fn aura_duration_packet(row: &Aura, self_guid: u64) -> Option<Outbound> {
    if row.target_guid != self_guid {
        return None;
    }
    // i64::MAX is the infinite-duration sentinel; no timer packet for toggle auras.
    if row.expires_at.to_micros_since_unix_epoch() == i64::MAX {
        return None;
    }
    let ms = (row.expires_at.to_micros_since_unix_epoch()
        - row.applied_at.to_micros_since_unix_epoch())
        / 1000;
    if ms <= 0 {
        return None;
    }
    Some(Outbound::One(
        ServerOpcodeMessage::SMSG_UPDATE_AURA_DURATION(codec::build_aura_duration(
            row.slot, ms as u32,
        )),
    ))
}

fn movement_outbound(
    coord: &Coordinator,
    current: &[Aura],
    changed: &Aura,
    self_guid: u64,
) -> Vec<Outbound> {
    crate::codec::movement::subscriptions::aura_movement_from_cache(
        coord,
        current,
        &[changed],
        changed.target_guid,
        self_guid,
    )
}

pub(crate) fn run_speed_packet(
    auras: impl Iterator<Item = crate::stdb::bindings::Aura>,
    changed: &Aura,
    self_guid: u64,
) -> Option<Outbound> {
    const BASE_RUN_SPEED: f32 = 7.0; // Vanilla base run speed, yards per second.
    if changed.target_guid != self_guid
        || !lyracore_shared::aura::moves_run_speed(changed.eff_kind, changed.eff_p_0)
    {
        return None;
    }
    let speed = lyracore_shared::aura::run_speed(
        BASE_RUN_SPEED,
        auras
            .filter(|aura| aura.target_guid == self_guid)
            .map(|aura| lyracore_shared::aura::AuraAmount {
                kind: aura.eff_kind,
                parameter: aura.eff_p_0,
                amount: aura.amount,
                stacks: aura.stacks,
            }),
    );
    Some(Outbound::One(
        ServerOpcodeMessage::SMSG_FORCE_RUN_SPEED_CHANGE(Box::new(codec::build_force_run_speed(
            self_guid, speed,
        ))),
    ))
}

pub(crate) fn swim_speed_packet(
    auras: impl Iterator<Item = crate::stdb::bindings::Aura>,
    changed: &Aura,
    self_guid: u64,
) -> Option<Outbound> {
    if changed.target_guid != self_guid
        || changed.eff_kind != A_MOD_SPEED
        || changed.eff_p_0 != i32::from(SPEED_SWIM)
    {
        return None;
    }
    let speed = lyracore_shared::aura::swim_speed(
        lyracore_shared::constants::speeds::SWIM,
        auras
            .filter(|aura| aura.target_guid == self_guid)
            .map(|aura| lyracore_shared::aura::AuraAmount {
                kind: aura.eff_kind,
                parameter: aura.eff_p_0,
                amount: aura.amount,
                stacks: aura.stacks,
            }),
    );
    Some(Outbound::One(
        ServerOpcodeMessage::SMSG_FORCE_SWIM_SPEED_CHANGE(Box::new(codec::build_force_swim_speed(
            self_guid, speed,
        ))),
    ))
}

pub(crate) fn dynobj_appeared(view: &WorldView, shard: ShardId, row: &DynamicObject) {
    let row = Arc::new(row.clone());
    for viewer in viewers_on_shard(view, shard) {
        if !crate::codec::entity::subscriptions::instance_relay_gate(
            row.instance_id,
            Some(viewer.instance_id),
        ) {
            continue;
        }
        let row = row.clone();
        enqueue(viewer.clone(), move |_| {
            crate::codec::combat::subscriptions::relay_dynobj_create(&row)
        });
    }
}

pub(crate) fn dynobj_vanished(view: &WorldView, shard: ShardId, row: &DynamicObject) {
    let guid = row.guid;
    for viewer in viewers_on_shard(view, shard) {
        enqueue(viewer.clone(), move |_| {
            crate::codec::combat::subscriptions::relay_destroy_object(guid)
        });
    }
}

pub(crate) fn combat_audience(
    view: &WorldView,
    shard: ShardId,
    row: &CombatEvent,
) -> Vec<Arc<Viewer>> {
    let key = CellKey::at(row.map_id, row.instance_id, row.grid_x, row.grid_y);
    view.cell_audience(
        shard,
        Some(key),
        BOX_HALF_SPAN,
        &[row.attacker_guid, row.target_guid],
    )
}

pub(crate) fn combat_event_appeared(view: &WorldView, shard: ShardId, row: &CombatEvent) {
    let row = Arc::new(row.clone());
    let packets = Arc::new(SharedEventPackets::default());
    for viewer in combat_audience(view, shard, &row) {
        let (row, packets) = (row.clone(), packets.clone());
        enqueue(viewer.clone(), move |viewer| {
            if !viewer.created.lock().unwrap().contains(&row.attacker_guid) {
                return Vec::new();
            }
            packets.outbound(&viewer, row.attacker_guid, || combat_event_messages(&row))
        });
    }
}

pub(crate) fn melee_audience(
    view: &WorldView,
    shard: ShardId,
    row: &MeleeAttack,
) -> Vec<Arc<Viewer>> {
    let key = view
        .spatial
        .entity_cell_on_shard(EntityLayer::WorldEntity, row.attacker_guid, shard);
    view.cell_audience(
        shard,
        key,
        BOX_HALF_SPAN,
        &[row.attacker_guid, row.target_guid],
    )
}

pub(crate) fn melee_engaged(view: &WorldView, shard: ShardId, row: &MeleeAttack, live: bool) {
    let row = Arc::new(row.clone());
    for viewer in melee_audience(view, shard, &row) {
        let row = row.clone();
        let created = viewer.created.clone();
        enqueue(viewer.clone(), move |_| {
            crate::codec::combat::subscriptions::melee_engage_outbound(&created, &row, live)
        });
    }
}

pub(crate) fn melee_disengaged(view: &WorldView, shard: ShardId, row: &MeleeAttack) {
    let row = Arc::new(row.clone());
    for viewer in melee_audience(view, shard, &row) {
        let row = row.clone();
        let self_guid = viewer.self_guid;
        enqueue(viewer.clone(), move |_| {
            crate::codec::combat::subscriptions::melee_disengage_outbound(self_guid, &row)
        });
    }
}

/// Swing Error codes are lockstep with `SWING_ERROR_*` in `module/src/combat/engage.rs`.
pub(crate) fn swing_error_outbound(old: &MeleeAttack, row: &MeleeAttack) -> Option<Outbound> {
    const NOT_IN_RANGE: u8 = 1;
    const BAD_FACING: u8 = 2;
    if old.swing_error == row.swing_error {
        return None;
    }
    match row.swing_error {
        NOT_IN_RANGE => Some(Outbound::One(
            ServerOpcodeMessage::SMSG_ATTACKSWING_NOTINRANGE,
        )),
        BAD_FACING => Some(Outbound::One(
            ServerOpcodeMessage::SMSG_ATTACKSWING_BADFACING,
        )),
        _ => None,
    }
}

pub(crate) fn melee_updated(
    view: &WorldView,
    shard: ShardId,
    old: &MeleeAttack,
    row: &MeleeAttack,
) {
    if swing_error_outbound(old, row).is_none() {
        return;
    }
    let Some(viewer) = view.viewer_of_owner_on_shard(shard, OwnerGuid(row.attacker_guid)) else {
        return;
    };
    let (old, row) = (old.clone(), row.clone());
    enqueue(viewer, move |_| {
        swing_error_outbound(&old, &row).into_iter().collect()
    });
}

pub(crate) fn aura_audience(view: &WorldView, shard: ShardId, row: &Aura) -> Vec<Arc<Viewer>> {
    let key = view
        .spatial
        .entity_cell_on_shard(EntityLayer::WorldEntity, row.target_guid, shard);
    view.cell_audience(
        shard,
        key,
        BOX_HALF_SPAN,
        &[row.target_guid, row.caster_guid],
    )
}

pub(crate) fn aura_applied(
    view: &Arc<WorldView>,
    coord: &Coordinator,
    shard: ShardId,
    row: &Aura,
    stealth_count: usize,
) {
    let row = Arc::new(row.clone());
    for viewer in aura_audience(view, shard, &row) {
        let (row, coord, view) = (row.clone(), coord.clone(), view.clone());
        enqueue(viewer.clone(), move |viewer| {
            crate::codec::combat::subscriptions::aura_insert_outbound(
                &coord,
                &view,
                shard,
                viewer.session,
                &viewer.created,
                viewer.self_guid,
                &row,
                stealth_count,
            )
        });
    }
}

pub(crate) fn aura_updated(view: &Arc<WorldView>, shard: ShardId, old: &Aura, row: &Aura) {
    view.auras.upsert(shard, row);
    let visible_changed = (old.spell_id, old.slot, old.flags, old.level)
        != (row.spell_id, row.slot, row.flags, row.level)
        || (old.eff_kind, old.eff_p_0, old.amount, old.stacks)
            != (row.eff_kind, row.eff_p_0, row.amount, row.stacks);
    let duration_changed = old.expires_at != row.expires_at || old.applied_at != row.applied_at;
    if !visible_changed {
        if duration_changed {
            if let Some(viewer) = view.viewer_of_owner_on_shard(shard, OwnerGuid(row.target_guid)) {
                let row = row.clone();
                enqueue(viewer, move |viewer| {
                    aura_duration_packet(&row, viewer.self_guid)
                        .into_iter()
                        .collect()
                });
            }
        }
        return;
    }
    let Some(coord) = view.shard(shard) else {
        return;
    };
    let row = Arc::new(row.clone());
    for viewer in aura_audience(view, shard, &row) {
        let (row, coord, view) = (row.clone(), coord.clone(), view.clone());
        enqueue(viewer.clone(), move |viewer| {
            crate::codec::combat::subscriptions::aura_update_outbound(
                &coord,
                &view,
                shard,
                &viewer.created,
                viewer.self_guid,
                &row,
                duration_changed,
            )
        });
    }
}

pub(crate) fn aura_removed(
    view: &Arc<WorldView>,
    coord: &Coordinator,
    shard: ShardId,
    row: &Aura,
    stealth_count: usize,
) {
    let row = Arc::new(row.clone());
    for viewer in aura_audience(view, shard, &row) {
        let (row, coord, view) = (row.clone(), coord.clone(), view.clone());
        enqueue(viewer.clone(), move |viewer| {
            crate::codec::combat::subscriptions::aura_delete_outbound(
                &coord,
                &view,
                shard,
                viewer.session,
                &viewer.created,
                viewer.self_guid,
                &row,
                stealth_count,
            )
        });
    }
}

pub(crate) fn cast_audience(
    view: &WorldView,
    shard: ShardId,
    row: &SpellCastEvent,
) -> Vec<Arc<Viewer>> {
    let key = CellKey::at(row.map_id, row.instance_id, row.grid_x, row.grid_y);
    view.cell_audience(
        shard,
        Some(key),
        BOX_HALF_SPAN,
        &[row.caster_guid, row.target_guid],
    )
}

pub(crate) fn cast_event_appeared(view: &WorldView, shard: ShardId, row: &SpellCastEvent) {
    let row = Arc::new(row.clone());
    let packets = Arc::new(SharedEventPackets::default());
    for viewer in cast_audience(view, shard, &row) {
        let (row, packets) = (row.clone(), packets.clone());
        enqueue(viewer.clone(), move |viewer| {
            packets.outbound(&viewer, row.caster_guid, || cast_event_messages(&row))
        });
    }
}

pub(crate) fn impact_audience(
    view: &WorldView,
    shard: ShardId,
    row: &SpellImpactEvent,
) -> Vec<Arc<Viewer>> {
    let key = CellKey::at(row.map_id, row.instance_id, row.grid_x, row.grid_y);
    view.cell_audience(
        shard,
        Some(key),
        BOX_HALF_SPAN,
        &[row.caster_guid, row.target_guid],
    )
}

pub(crate) fn impact_appeared(view: &WorldView, shard: ShardId, row: &SpellImpactEvent) {
    let row = Arc::new(row.clone());
    let packets = Arc::new(SharedEventPackets::default());
    for viewer in impact_audience(view, shard, &row) {
        let (row, packets) = (row.clone(), packets.clone());
        enqueue(viewer.clone(), move |viewer| {
            packets.outbound(&viewer, row.caster_guid, || {
                impact_event_outbound(&row)
                    .into_iter()
                    .map(|out| (EventAudience::Everyone, out))
                    .collect()
            })
        });
    }
}

crate::stdb::relays::declare_relays! {
    game_dynamic_object [false] {
        insert(Snapshot) => |context, _event, row| dynobj_appeared(&context.view, context.shard, row);
        delete(Snapshot) => |context, _event, row| dynobj_vanished(&context.view, context.shard, row);
    }
    game_combat_event_live [false] {
        insert(Snapshot) => |context, _event, row| combat_event_appeared(&context.view, context.shard, row);
    }
    game_melee_attack [false] {
        insert(Snapshot) => |context, event, row| melee_engaged(&context.view, context.shard, row, !matches!(event.event, spacetimedb_sdk::Event::SubscribeApplied));
        update(Snapshot) => |context, _event, old, row| melee_updated(&context.view, context.shard, old, row);
        delete(Snapshot) => |context, _event, row| melee_disengaged(&context.view, context.shard, row);
    }
    game_spell_cast_event_live [false] {
        insert(Snapshot) => |context, _event, row| cast_event_appeared(&context.view, context.shard, row);
    }
    game_spell_impact_event_live [false] {
        insert(Snapshot) => |context, _event, row| impact_appeared(&context.view, context.shard, row);
    }
    game_aura [false] {
        insert(Snapshot) => |context, _event, row| { context.view.auras.upsert(context.shard, row); let count = context.view.auras.stealth_count(context.shard, row.target_guid); aura_applied(&context.view, &context.coord, context.shard, row, count); };
        update(Snapshot) => |context, _event, old, row| aura_updated(&context.view, context.shard, old, row);
        delete(Snapshot) => |context, _event, row| { context.view.auras.remove(context.shard, row); let count = context.view.auras.stealth_count(context.shard, row.target_guid); aura_removed(&context.view, &context.coord, context.shard, row, count); };
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SpellCastEventKind {
    Start,
    Go,
    Interrupt,
    Pushback,
    ProcLog,
    PeriodicLog,
}

impl SpellCastEventKind {
    // Kind zero uses the old schema's field-based ordering; unknown nonzero kinds must drop.
    pub(crate) fn decode(row: &SpellCastEvent) -> Option<Self> {
        match row.kind {
            0 if row.is_interrupted => Some(Self::Interrupt),
            0 if row.delay_ms > 0 => Some(Self::Pushback),
            0 if row.is_proc_log => Some(Self::ProcLog),
            0 if row.cast_time_ms > 0 => Some(Self::Start),
            0 => Some(Self::Go),
            1 => Some(Self::Start),
            2 => Some(Self::Go),
            3 => Some(Self::Interrupt),
            4 => Some(Self::Pushback),
            5 => Some(Self::ProcLog),
            6 => Some(Self::PeriodicLog),
            unknown => {
                log::warn!(
                    "spell cast relay: unknown kind {unknown} (event {}, caster {}, spell {})",
                    row.id,
                    row.caster_guid,
                    row.spell_id
                );
                None
            }
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use lyracore_shared::spell::taxonomy::SPEED_MOVE;
    pub(crate) fn aura(
        target_guid: u64,
        eff_kind: u8,
        eff_p_0: i32,
        amount: i32,
        applied_at_micros: i64,
        expires_at_micros: i64,
    ) -> Aura {
        Aura {
            id: 1,
            target_guid,
            caster_guid: target_guid,
            spell_id: 6673,
            slot: 3,
            level: 1,
            flags: 0,
            applied_at: spacetimedb_sdk::Timestamp::from_micros_since_unix_epoch(applied_at_micros),
            expires_at: spacetimedb_sdk::Timestamp::from_micros_since_unix_epoch(expires_at_micros),
            effect_id: 1,
            eff_kind,
            amount,
            eff_p_0,
            eff_p_0_kind: 0,
            eff_p_1: 0,
            period_ms: 0,
            amount_remaining: 0,
            stacks: 1,
            next_tick_micros: 0,
            channel_target: 0,
            enters_combat: false,
            proc_flags: 0,
            proc_chance: 0,
            proc_ppm: 0.0,
            proc_ex: 0,
            proc_school_mask: 0,
            proc_family_name: 0,
            proc_family_flags: 0,
            proc_charges: 0,
            proc_icd_ms: 0,
            proc_ready_micros: 0,
        }
    }

    #[test]
    fn aura_duration_packet_pins_the_remaining_window_for_self_only() {
        let self_guid = 1u64;
        let mine = aura(self_guid, 0xA1, 1, 10, 0, 5_000_000);
        let out = aura_duration_packet(&mine, self_guid);
        let m = match out {
            Some(Outbound::One(ServerOpcodeMessage::SMSG_UPDATE_AURA_DURATION(m))) => m,
            _ => panic!("expected Some(Outbound::One(SMSG_UPDATE_AURA_DURATION))"),
        };
        assert_eq!(m, codec::build_aura_duration(3, 5000));
        assert!(aura_duration_packet(&mine, self_guid + 1).is_none());
        let toggle = aura(self_guid, 0xA1, 1, 10, 0, i64::MAX);
        assert!(aura_duration_packet(&toggle, self_guid).is_none());
        let expired = aura(self_guid, 0xA1, 1, 10, 5_000_000, 5_000_000);
        assert!(aura_duration_packet(&expired, self_guid).is_none());
    }

    #[test]
    fn aura_bookkeeping_updates_the_cache_without_observer_jobs() {
        use crate::stdb::world_view::test_support::registered_viewer;

        let view = Arc::new(WorldView::new(true));
        let (_, owner) = registered_viewer(&view, 0, 7);
        let (peer, observer) = registered_viewer(&view, 0, 8);
        peer.created.lock().unwrap().insert(7);
        let old = aura(7, 0, 0, 10, 0, 5_000_000);
        let mut new = old.clone();
        new.next_tick_micros = 1_000_000;
        new.amount_remaining = 7;
        new.proc_charges = 2;
        new.proc_ready_micros = 2_000_000;

        aura_updated(&view, 0, &old, &new);

        assert_eq!(view.auras.on_target(0, 7), vec![new]);
        assert!(owner.try_recv().is_err());
        assert!(observer.try_recv().is_err());
    }

    #[test]
    fn refreshing_an_unchanged_aura_updates_only_its_owners_duration() {
        use crate::stdb::world_view::test_support::{queued_job, registered_viewer};

        let view = Arc::new(WorldView::new(true));
        let (_, owner) = registered_viewer(&view, 0, 7);
        let (peer, observer) = registered_viewer(&view, 0, 8);
        let (_, other_shard) = registered_viewer(&view, 1, 9);
        peer.created.lock().unwrap().insert(7);
        let old = aura(7, A_MOD_SPEED, i32::from(SPEED_MOVE), 50, 0, 5_000_000);
        let mut new = old.clone();
        new.applied_at = spacetimedb_sdk::Timestamp::from_micros_since_unix_epoch(1_000_000);
        new.expires_at = spacetimedb_sdk::Timestamp::from_micros_since_unix_epoch(7_000_000);

        aura_updated(&view, 0, &old, &new);

        let packets = queued_job(&owner);
        let [Outbound::One(ServerOpcodeMessage::SMSG_UPDATE_AURA_DURATION(duration))] =
            packets.as_slice()
        else {
            panic!("a refresh must send only the owner duration");
        };
        assert_eq!(*duration, codec::build_aura_duration(3, 6000));
        assert!(owner.try_recv().is_err());
        assert!(observer.try_recv().is_err());
        assert!(other_shard.try_recv().is_err());
    }

    #[test]
    fn run_speed_packet_uses_the_strongest_move_bonus_on_self_only() {
        let self_guid = 1u64;
        let a1 = aura(self_guid, 0xA4, 0, 25, 0, i64::MAX);
        let a2 = aura(self_guid, 0xA4, 0, 50, 0, i64::MAX);
        let current = vec![a1.clone(), a2.clone()];
        let out = run_speed_packet(current.clone().into_iter(), &a1, self_guid);
        let m = match out {
            Some(Outbound::One(ServerOpcodeMessage::SMSG_FORCE_RUN_SPEED_CHANGE(m))) => m,
            _ => panic!("expected Some(Outbound::One(SMSG_FORCE_RUN_SPEED_CHANGE))"),
        };
        assert_eq!(*m, codec::build_force_run_speed(self_guid, 10.5));
        let unrelated = aura(self_guid, 0xA1, 1, 10, 0, i64::MAX);
        assert!(run_speed_packet(current.clone().into_iter(), &unrelated, self_guid).is_none());
        let peer_changed = aura(self_guid + 1, 0xA4, 0, 25, 0, i64::MAX);
        assert!(run_speed_packet(current.into_iter(), &peer_changed, self_guid).is_none());
    }

    #[test]
    fn run_speed_packet_refreshes_always_and_not_stack_bonuses_after_removal() {
        use lyracore_shared::spell::taxonomy::{SPEED_MOVE_ALWAYS, SPEED_MOVE_NOT_STACK};
        let guid = 7;
        for speed_kind in [SPEED_MOVE_ALWAYS, SPEED_MOVE_NOT_STACK] {
            let changed = aura(guid, A_MOD_SPEED, i32::from(speed_kind), 50, 0, i64::MAX);
            for (current, expected) in [(vec![changed.clone()], 10.5), (vec![], 7.0)] {
                let Some(Outbound::One(ServerOpcodeMessage::SMSG_FORCE_RUN_SPEED_CHANGE(packet))) =
                    run_speed_packet(current.into_iter(), &changed, guid)
                else {
                    panic!("run speed packet");
                };
                assert_eq!(*packet, codec::build_force_run_speed(guid, expected));
            }
        }
    }

    #[test]
    fn swim_speed_packet_updates_and_restores_the_owner_speed() {
        let guid = 7;
        let ghost = aura(guid, A_MOD_SPEED, i32::from(SPEED_SWIM), 25, 0, i64::MAX);
        let wisp = aura(guid, A_MOD_SPEED, i32::from(SPEED_SWIM), 50, 0, i64::MAX);
        let base = lyracore_shared::constants::speeds::SWIM;
        for (current, expected) in [
            (vec![ghost.clone(), wisp.clone()], base * 1.5),
            (vec![ghost.clone()], base * 1.25),
            (vec![], base),
        ] {
            let Some(Outbound::One(ServerOpcodeMessage::SMSG_FORCE_SWIM_SPEED_CHANGE(packet))) =
                swim_speed_packet(current.into_iter(), &wisp, guid)
            else {
                panic!("swim speed packet");
            };
            assert_eq!(packet.guid.guid(), guid);
            assert_eq!(packet.move_event, 0);
            assert!((packet.speed - expected).abs() < 0.000001);
        }
        assert!(swim_speed_packet([ghost.clone()].into_iter(), &ghost, guid + 1).is_none());
        let cast = aura(guid, A_MOD_SPEED, 2, 50, 0, i64::MAX);
        assert!(swim_speed_packet([cast.clone()].into_iter(), &cast, guid).is_none());
    }
    #[test]
    fn mounted_snare_apply_and_remove_preserve_the_mount_bonus() {
        use lyracore_shared::spell::taxonomy::{A_MOUNTED, SPEED_MOUNTED};
        let rider = 7;
        let mounted = aura(rider, A_MOUNTED, 1147, 0, 0, i64::MAX);
        let bonus = aura(
            rider,
            A_MOD_SPEED,
            i32::from(SPEED_MOUNTED),
            60,
            0,
            i64::MAX,
        );
        let snare = aura(rider, A_MOD_SPEED, i32::from(SPEED_MOVE), -30, 0, i64::MAX);
        for (current, expected) in [
            (vec![mounted.clone(), bonus.clone(), snare.clone()], 9.1),
            (vec![mounted.clone(), bonus.clone()], 11.2),
            (vec![bonus.clone(), snare.clone()], 4.9),
        ] {
            let Some(Outbound::One(ServerOpcodeMessage::SMSG_FORCE_RUN_SPEED_CHANGE(packet))) =
                run_speed_packet(current.into_iter(), &snare, rider)
            else {
                panic!("speed packet");
            };
            assert_eq!(*packet, codec::build_force_run_speed(rider, expected));
        }
    }

    #[test]
    fn stacked_snare_packet_uses_the_ten_percent_floor() {
        let mut snare = aura(7, A_MOD_SPEED, i32::from(SPEED_MOVE), -30, 0, i64::MAX);
        snare.stacks = 4;
        let Some(Outbound::One(ServerOpcodeMessage::SMSG_FORCE_RUN_SPEED_CHANGE(packet))) =
            run_speed_packet([snare.clone()].into_iter(), &snare, 7)
        else {
            panic!("speed packet");
        };
        assert_eq!(*packet, codec::build_force_run_speed(7, 0.7));
    }
}

#[cfg(test)]
mod log_tests;
#[cfg(test)]
mod packet_tests;
