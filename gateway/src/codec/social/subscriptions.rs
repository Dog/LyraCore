use crate::codec::{self};
use crate::stdb::bindings::*;
use crate::stdb::world_index::{CellKey, EntityLayer, ShardId};
use crate::stdb::world_view::*;
use crate::stdb::Coordinator;
use crate::world::{CharacterStore, Outbound, QuestActionStore, SessionStore, ShardRoutingStore};
use spacetimedb_sdk::Table;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, OnceLock};
use wow_world_messages::vanilla::opcodes::ServerOpcodeMessage;

pub(crate) const SAY_RANGE_YD: f32 = 25.0;

pub(crate) const YELL_RANGE_YD: f32 = 300.0;

#[cfg(test)]
pub(crate) const SAY_RANGE_SQ: f32 = SAY_RANGE_YD * SAY_RANGE_YD;

#[cfg(test)]
pub(crate) const YELL_RANGE_SQ: f32 = YELL_RANGE_YD * YELL_RANGE_YD;

pub(crate) fn chat_range_yd(chat_type: u8) -> f32 {
    if matches!(
        chat_type,
        lyracore_shared::chat::broadcast_chat::YELL
            | lyracore_shared::chat::broadcast_chat::CREATURE_BOSS_EMOTE
    ) {
        YELL_RANGE_YD
    } else {
        SAY_RANGE_YD
    }
}

#[allow(clippy::too_many_lines)]
pub(crate) fn group_event_outbound<
    St: CharacterStore + QuestActionStore + SessionStore + ShardRoutingStore + ?Sized,
>(
    store: &St,
    self_guid: u64,
    row: &GroupEvent,
) -> Vec<Outbound> {
    use lyracore_shared::group::event_kind as group_kind;
    use lyracore_shared::loot_roll::event_kind as roll_kind;
    use lyracore_shared::meeting_stone::event_kind as stone_kind;
    use lyracore_shared::quest::share_event_kind as quest_share_kind;
    let msg = match row.kind {
        group_kind::INVITE => Some(ServerOpcodeMessage::SMSG_GROUP_INVITE(Box::new(
            codec::build_group_invite(
                other_character_name(store, row, "INVITE").unwrap_or_default(),
            ),
        ))),
        group_kind::LIST => match lyracore_shared::group::RosterPayload::decode(&row.payload) {
            Some(roster) => {
                let mut packets = vec![crate::world::party::render_list(store, self_guid, &roster)];
                if !roster.target_icons.is_empty() {
                    let (opcode, body) = codec::build_target_icon_list_raw(&roster.target_icons);
                    packets.push(Outbound::Raw { opcode, body });
                }
                return packets;
            }
            None => {
                log::warn!(
                    "group LIST relay: unparseable roster payload {:?} (event {})",
                    row.payload,
                    row.id
                );
                None
            }
        },
        group_kind::DECLINE => Some(ServerOpcodeMessage::SMSG_GROUP_DECLINE(Box::new(
            codec::build_group_decline(
                other_character_name(store, row, "DECLINE").unwrap_or_default(),
            ),
        ))),
        group_kind::DESTROYED => Some(ServerOpcodeMessage::SMSG_GROUP_DESTROYED),
        group_kind::UNINVITED => Some(ServerOpcodeMessage::SMSG_GROUP_UNINVITE),
        group_kind::SET_LEADER => other_character_name(store, row, "SET_LEADER").map(|name| {
            ServerOpcodeMessage::SMSG_GROUP_SET_LEADER(Box::new(codec::build_group_set_leader(
                name,
            )))
        }),
        roll_kind::ROLL_START => match lyracore_shared::loot_roll::decode_start(&row.payload) {
            Some((corpse_guid, slot, item_entry, countdown_ms, random_property_id)) => Some(
                ServerOpcodeMessage::SMSG_LOOT_START_ROLL(Box::new(codec::build_loot_start_roll(
                    corpse_guid,
                    slot,
                    item_entry,
                    countdown_ms,
                    random_property_id,
                ))),
            ),
            None => {
                log::warn!(
                    "loot ROLL_START relay: unparseable payload {:?} (event {})",
                    row.payload,
                    row.id
                );
                None
            }
        },
        roll_kind::ROLL_VOTE => match lyracore_shared::loot_roll::decode_vote(&row.payload) {
            Some((corpse, slot, entry, number, vote, _auto_pass, property_id)) => Some(
                ServerOpcodeMessage::SMSG_LOOT_ROLL(Box::new(codec::build_loot_roll(
                    corpse,
                    slot,
                    row.other_guid,
                    entry,
                    number,
                    vote,
                    property_id,
                ))),
            ),
            None => {
                log::warn!(
                    "loot ROLL_VOTE relay: unparseable payload {:?} (event {})",
                    row.payload,
                    row.id
                );
                None
            }
        },
        roll_kind::ROLL_WON => match lyracore_shared::loot_roll::decode_won(&row.payload) {
            Some((corpse, slot, entry, number, vote, property_id)) => Some(
                ServerOpcodeMessage::SMSG_LOOT_ROLL_WON(Box::new(codec::build_loot_roll_won(
                    corpse,
                    slot,
                    entry,
                    row.other_guid,
                    number,
                    vote,
                    property_id,
                ))),
            ),
            None => {
                log::warn!(
                    "loot ROLL_WON relay: unparseable payload {:?} (event {})",
                    row.payload,
                    row.id
                );
                None
            }
        },
        roll_kind::MASTER_LIST => {
            match lyracore_shared::loot_roll::decode_master_list(&row.payload) {
                Some((_corpse_guid, eligible)) => Some(ServerOpcodeMessage::SMSG_LOOT_MASTER_LIST(
                    Box::new(codec::build_loot_master_list(&eligible)),
                )),
                None => {
                    log::warn!(
                        "loot MASTER_LIST relay: unparseable payload {:?} (event {})",
                        row.payload,
                        row.id
                    );
                    None
                }
            }
        }

        roll_kind::MONEY_SHARE => {
            match lyracore_shared::loot_roll::decode_money_share(&row.payload) {
                Some(share) => Some(ServerOpcodeMessage::SMSG_LOOT_MONEY_NOTIFY(
                    codec::build_loot_money_notify(share),
                )),
                None => {
                    log::warn!(
                        "loot MONEY_SHARE relay: unparseable payload {:?} (event {})",
                        row.payload,
                        row.id
                    );
                    None
                }
            }
        }
        quest_share_kind::QUEST_SHARE => match row.payload.parse::<u32>() {
            Ok(quest_id) => match store.quest_detail_view(quest_id) {
                Ok(Some(detail)) => {
                    let (opcode, body) = codec::build_quest_details_raw(row.other_guid, &detail);
                    return vec![Outbound::Raw { opcode, body }];
                }
                Ok(None) => {
                    log::warn!(
                        "quest QUEST_SHARE relay: quest {quest_id} not loaded (event {})",
                        row.id
                    );
                    None
                }
                Err(e) => {
                    log::warn!(
                        "quest QUEST_SHARE relay: quest_detail lookup failed (event {}): {e}",
                        row.id
                    );
                    None
                }
            },
            Err(_) => {
                log::warn!(
                    "quest QUEST_SHARE relay: unparseable payload {:?} (event {})",
                    row.payload,
                    row.id
                );
                None
            }
        },
        quest_share_kind::QUEST_PUSH_RESULT => match row.payload.parse::<u8>() {
            Ok(code) => Some(ServerOpcodeMessage::MSG_QUEST_PUSH_RESULT(Box::new(
                codec::build_quest_push_result(row.other_guid, code),
            ))),
            Err(_) => {
                log::warn!(
                    "quest QUEST_PUSH_RESULT relay: unparseable payload {:?} (event {})",
                    row.payload,
                    row.id
                );
                None
            }
        },
        group_kind::READY_CHECK => Some(ServerOpcodeMessage::MSG_RAID_READY_CHECK(Box::new(
            codec::build_ready_check(),
        ))),
        group_kind::READY_CHECK_ANSWER => broadcast_packet(
            row,
            lyracore_shared::group::decode_ready_check_answer(&row.payload),
            |state| {
                ServerOpcodeMessage::MSG_RAID_READY_CHECK(Box::new(
                    codec::build_ready_check_answer(row.other_guid, state),
                ))
            },
        ),
        group_kind::TARGET_ICON_UPDATE => broadcast_packet(
            row,
            lyracore_shared::group::TargetIcon::decode(&row.payload)
                .and_then(codec::build_target_icon_update),
            |update| ServerOpcodeMessage::MSG_RAID_TARGET_UPDATE(Box::new(update)),
        ),
        group_kind::TARGET_ICON_LIST => {
            match lyracore_shared::group::decode_target_icons(&row.payload) {
                Some(icons) => {
                    let (opcode, body) = codec::build_target_icon_list_raw(&icons);
                    return vec![Outbound::Raw { opcode, body }];
                }
                None => {
                    log_unparseable_broadcast(row);
                    None
                }
            }
        }
        group_kind::MINIMAP_PING => broadcast_packet(
            row,
            lyracore_shared::group::decode_minimap_ping(&row.payload),
            |(x, y)| {
                ServerOpcodeMessage::MSG_MINIMAP_PING(Box::new(codec::build_minimap_ping(
                    row.other_guid,
                    x,
                    y,
                )))
            },
        ),
        group_kind::RANDOM_ROLL => broadcast_packet(
            row,
            lyracore_shared::group::decode_random_roll(&row.payload),
            |(min, max, result)| {
                ServerOpcodeMessage::MSG_RANDOM_ROLL(Box::new(codec::build_random_roll(
                    row.other_guid,
                    min,
                    max,
                    result,
                )))
            },
        ),
        stone_kind::QUEUE => {
            let packet = lyracore_shared::meeting_stone::decode_queue(&row.payload)
                .and_then(|(area_id, status)| codec::build_meetingstone_setqueue(area_id, status));
            if packet.is_none() {
                log::warn!(
                    "meeting stone QUEUE relay: unencodable payload {:?} (event {})",
                    row.payload,
                    row.id
                );
            }
            packet.map(ServerOpcodeMessage::SMSG_MEETINGSTONE_SETQUEUE)
        }
        stone_kind::MEMBER_ADDED => Some(ServerOpcodeMessage::SMSG_MEETINGSTONE_MEMBER_ADDED(
            codec::build_meetingstone_member_added(row.other_guid),
        )),
        stone_kind::IN_PROGRESS => Some(ServerOpcodeMessage::SMSG_MEETINGSTONE_IN_PROGRESS),
        stone_kind::COMPLETE => Some(ServerOpcodeMessage::SMSG_MEETINGSTONE_COMPLETE),
        other => {
            log::warn!("group event relay: unknown kind {other} (id {})", row.id);
            None
        }
    };
    match msg {
        Some(m) => vec![Outbound::One(m)],
        None => Vec::new(),
    }
}

pub(crate) fn other_character_name<St: CharacterStore + ShardRoutingStore + ?Sized>(
    store: &St,
    row: &GroupEvent,
    relay: &str,
) -> Option<String> {
    match crate::world::presence::character_anywhere(store, row.other_guid) {
        Ok(Some(character)) => Some(character.name),
        Ok(None) => {
            log::warn!(
                "group {relay} relay: no shard names {} (event {})",
                row.other_guid,
                row.id
            );
            None
        }
        Err(e) => {
            log::warn!(
                "group {relay} relay: name lookup for {} failed (event {}): {e:#}",
                row.other_guid,
                row.id
            );
            None
        }
    }
}

pub(crate) fn broadcast_packet<T>(
    row: &GroupEvent,
    decoded: Option<T>,
    packet: impl FnOnce(T) -> ServerOpcodeMessage,
) -> Option<ServerOpcodeMessage> {
    if decoded.is_none() {
        log_unparseable_broadcast(row);
    }
    decoded.map(packet)
}

pub(crate) fn log_unparseable_broadcast(row: &GroupEvent) {
    log::warn!(
        "group broadcast relay: unparseable kind {} payload {:?} (event {})",
        row.kind,
        row.payload,
        row.id
    );
}

pub(crate) fn private_recipient_audience(row_recipient_guid: u64, viewer_guid: u64) -> bool {
    row_recipient_guid != 0 && row_recipient_guid == viewer_guid
}

pub(crate) fn realm_chat_outbound(row: &RealmChatEvent) -> Vec<Outbound> {
    codec::build_realm_chat_line(
        row.kind,
        row.speaker_guid,
        row.language,
        row.chat_tag,
        row.channel_name.clone(),
        row.message.clone(),
    )
    .map(|message| Outbound::One(ServerOpcodeMessage::SMSG_MESSAGECHAT(Box::new(message))))
    .into_iter()
    .collect()
}

pub(crate) fn system_message_event_outbound(row: &SystemMessageEvent) -> Vec<Outbound> {
    let message = codec::build_gm_system_message(row.message.clone());
    vec![Outbound::One(ServerOpcodeMessage::SMSG_MESSAGECHAT(
        Box::new(message),
    ))]
}

pub(crate) fn chat_event_outbound(
    coord: &Coordinator,
    self_guid: u64,
    row: &ChatEvent,
) -> Vec<Outbound> {
    if row.chat_type == lyracore_shared::chat::broadcast_chat::CREATURE_WHISPER
        && row.target_guid != self_guid
    {
        return Vec::new();
    }
    let guard = coord.0.coord();
    let entities = guard.conn.db.game_world_entity();
    let speaker = entities.guid().find(&row.sender_guid);
    if row.sender_guid != self_guid {
        let Some(speaker) = &speaker else {
            return Vec::new();
        };
        let Some(listener) = entities.guid().find(&self_guid) else {
            return Vec::new();
        };
        if !creature_chat_reaches(row.chat_type, speaker, &listener) {
            return Vec::new();
        }
        if row.chat_type == lyracore_shared::chat::broadcast_chat::EMOTE
            && !lyracore_shared::faction::same_team(
                (speaker.unit_bytes_0 & 0xFF) as u8,
                (listener.unit_bytes_0 & 0xFF) as u8,
            )
        {
            return Vec::new();
        }
    }
    let sender_name = if matches!(row.sender_guid >> 48, 0xF130 | 0xF140) {
        let Some(speaker) = &speaker else {
            return Vec::new();
        };
        let Some(template) = guard
            .conn
            .db
            .game_creature_template()
            .entry()
            .find(&speaker.entry)
        else {
            return Vec::new();
        };
        Some(template.name)
    } else {
        None
    };
    vec![chat_event_message(
        row,
        sender_name,
        nearby_chat_tag(speaker.as_ref()),
    )]
}

fn creature_chat_reaches(chat_type: u8, speaker: &WorldEntity, listener: &WorldEntity) -> bool {
    if chat_type == lyracore_shared::chat::broadcast_chat::CREATURE_WHISPER {
        speaker.map_id == listener.map_id && speaker.instance_id == listener.instance_id
    } else if lyracore_shared::chat::broadcast_chat::is_zone_wide(chat_type) {
        speaker.map_id == listener.map_id
            && speaker.instance_id == listener.instance_id
            && speaker.zone_id != 0
            && speaker.zone_id == listener.zone_id
    } else {
        let range = chat_range_yd(chat_type);
        chat_in_range(
            speaker.map_id,
            speaker.instance_id,
            speaker.x,
            speaker.y,
            listener.map_id,
            listener.instance_id,
            listener.x,
            listener.y,
            range * range,
        )
    }
}

#[cfg(test)]
mod creature_chat_tests {
    use super::*;
    use crate::codec::entity::subscriptions::tests::player_entity;
    use lyracore_shared::chat::broadcast_chat::*;

    #[test]
    fn zone_speech_reaches_distant_listeners_only_in_the_same_zone_and_instance() {
        let speaker = WorldEntity {
            map_id: 0,
            zone_id: 12,
            instance_id: 0,
            ..player_entity()
        };
        let mut listener = WorldEntity {
            x: 5_000.0,
            ..speaker.clone()
        };
        for mode in [CREATURE_ZONE_YELL, CREATURE_ZONE_EMOTE] {
            assert!(creature_chat_reaches(mode, &speaker, &listener));
            listener.zone_id = 14;
            assert!(!creature_chat_reaches(mode, &speaker, &listener));
            listener.zone_id = 12;
            listener.instance_id = 2;
            assert!(!creature_chat_reaches(mode, &speaker, &listener));
            listener.instance_id = 0;
            listener.map_id = 1;
            assert!(!creature_chat_reaches(mode, &speaker, &listener));
            listener.map_id = 0;
        }
        assert!(!creature_chat_reaches(YELL, &speaker, &listener));
        let unresolved = WorldEntity {
            zone_id: 0,
            ..speaker
        };
        listener.zone_id = 0;
        assert!(!creature_chat_reaches(
            CREATURE_ZONE_YELL,
            &unresolved,
            &listener
        ));
    }

    #[test]
    fn boss_emotes_use_yell_distance() {
        let speaker = WorldEntity {
            x: 0.0,
            y: 0.0,
            ..player_entity()
        };
        let mut listener = WorldEntity {
            x: 250.0,
            ..speaker.clone()
        };
        assert!(creature_chat_reaches(
            CREATURE_BOSS_EMOTE,
            &speaker,
            &listener
        ));
        assert!(!creature_chat_reaches(
            CREATURE_TEXT_EMOTE,
            &speaker,
            &listener
        ));
        listener.x = 301.0;
        assert!(!creature_chat_reaches(
            CREATURE_BOSS_EMOTE,
            &speaker,
            &listener
        ));
    }
}

pub(crate) fn nearby_chat_tag(speaker: Option<&WorldEntity>) -> u8 {
    match speaker {
        Some(entity)
            if entity.type_mask & lyracore_shared::constants::type_mask::PLAYER_BIT != 0 =>
        {
            lyracore_shared::chat::chat_tag_for(entity.player_flags)
        }
        _ => lyracore_shared::chat::chat_tag::NONE,
    }
}

pub(crate) fn chat_event_message(
    row: &ChatEvent,
    sender_name: Option<String>,
    chat_tag: u8,
) -> Outbound {
    let message = codec::build_chat_message_to(
        row.sender_guid,
        sender_name,
        row.target_guid,
        row.chat_type,
        row.language,
        row.message.clone(),
        chat_tag,
    );
    Outbound::One(ServerOpcodeMessage::SMSG_MESSAGECHAT(Box::new(message)))
}

pub(crate) fn emote_event_outbound(
    coord: &Coordinator,
    self_guid: u64,
    row: &EmoteEvent,
) -> Vec<Outbound> {
    if row.sender_guid != self_guid {
        let guard = coord.0.coord();
        let speaker = match guard
            .conn
            .db
            .game_world_entity()
            .guid()
            .find(&row.sender_guid)
        {
            Some(e) => e,
            None => return Vec::new(),
        };
        let listener = match guard.conn.db.game_world_entity().guid().find(&self_guid) {
            Some(e) => e,
            None => return Vec::new(),
        };
        let range_yd = lyracore_shared::chat::broadcast_chat::TEXT_EMOTE_RANGE_YD;
        if !chat_in_range(
            speaker.map_id,
            speaker.instance_id,
            speaker.x,
            speaker.y,
            listener.map_id,
            listener.instance_id,
            listener.x,
            listener.y,
            range_yd * range_yd,
        ) {
            return Vec::new();
        }
    }
    let target_name = if row.target_guid != 0 {
        let guard = coord.0.coord();
        let db = &guard.conn.db;
        db.game_character()
            .guid()
            .find(&row.target_guid)
            .map(|c| c.name)
            .or_else(|| {
                db.game_world_entity()
                    .guid()
                    .find(&row.target_guid)
                    .and_then(|e| {
                        db.game_creature_template()
                            .entry()
                            .find(&e.entry)
                            .map(|c| c.name)
                    })
            })
    } else {
        None
    };
    let mut out = Vec::new();
    if let Some(m) =
        codec::build_text_emote(row.sender_guid, row.text_emote, row.emote_anim, target_name)
    {
        out.push(Outbound::One(ServerOpcodeMessage::SMSG_TEXT_EMOTE(
            Box::new(m),
        )));
    }
    if let Some(a) = codec::build_emote_anim(row.sender_guid, row.emote_anim) {
        out.push(Outbound::One(ServerOpcodeMessage::SMSG_EMOTE(Box::new(a))));
    }
    out
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn chat_in_range(
    speaker_map: u32,
    speaker_instance: u64,
    speaker_x: f32,
    speaker_y: f32,
    listener_map: u32,
    listener_instance: u64,
    listener_x: f32,
    listener_y: f32,
    range_sq: f32,
) -> bool {
    if speaker_map != listener_map || speaker_instance != listener_instance {
        return false;
    }
    let dx = speaker_x - listener_x;
    let dy = speaker_y - listener_y;
    dx * dx + dy * dy <= range_sq
}

pub(crate) fn instance_removal_started(
    view: &Arc<WorldView>,
    shard: ShardId,
    row: &InstanceRemoval,
) {
    let Some(viewer) = view.viewer_of_owner_on_shard(shard, OwnerGuid(row.character_guid)) else {
        return;
    };
    let (view, due) = (view.clone(), row.scheduled_at);
    enqueue(viewer, move |viewer| {
        let now = crate::codec::movement::subscriptions::unix_now_micros();
        instance_removal_time_left_ms(&due, now)
            .map(|left| show_countdown(&view, viewer.self_guid, left))
            .into_iter()
            .collect()
    });
}

pub(crate) fn instance_removal_ended(view: &Arc<WorldView>, shard: ShardId, row: &InstanceRemoval) {
    let Some(viewer) = view.viewer_of_owner_on_shard(shard, OwnerGuid(row.character_guid)) else {
        return;
    };
    let view = view.clone();
    enqueue(viewer, move |viewer| {
        vec![show_countdown(&view, viewer.self_guid, 0)]
    });
}

pub(crate) fn show_countdown(view: &WorldView, character_guid: u64, timer_ms: u32) -> Outbound {
    view.note_countdown(character_guid, timer_ms > 0);
    Outbound::One(ServerOpcodeMessage::SMSG_RAID_GROUP_ONLY(
        crate::codec::build_raid_group_only(timer_ms),
    ))
}

pub(crate) fn resident_countdown_ms(
    due: Option<&spacetimedb_sdk::ScheduleAt>,
    shown: bool,
    now_micros: u64,
) -> Option<u32> {
    match due {
        Some(due) => instance_removal_time_left_ms(due, now_micros),
        None => shown.then_some(0),
    }
}

pub(crate) fn instance_removal_time_left_ms(
    due: &spacetimedb_sdk::ScheduleAt,
    now_micros: u64,
) -> Option<u32> {
    let spacetimedb_sdk::ScheduleAt::Time(at) = due else {
        return None;
    };
    let due_micros = u64::try_from(at.to_micros_since_unix_epoch()).ok()?;
    let left_micros = due_micros
        .checked_sub(now_micros)
        .filter(|left| *left > 0)?;
    let left_ms = left_micros.div_ceil(1_000);
    Some(
        u32::try_from(left_ms).map_or(lyracore_shared::instance::INSTANCE_REMOVAL_MS, |left| {
            left.min(lyracore_shared::instance::INSTANCE_REMOVAL_MS)
        }),
    )
}

pub(crate) fn resident_instance_removal_outbound(
    view: &WorldView,
    coord: &Coordinator,
    character_guid: u64,
) -> Vec<Outbound> {
    let due = coord
        .0
        .coord()
        .conn
        .db
        .game_instance_removal()
        .character_guid()
        .find(&character_guid)
        .map(|row| row.scheduled_at);
    let now = crate::codec::movement::subscriptions::unix_now_micros();
    resident_countdown_ms(due.as_ref(), view.countdown_shown(character_guid), now)
        .map(|timer_ms| show_countdown(view, character_guid, timer_ms))
        .into_iter()
        .collect()
}

pub(crate) fn realm_chat_appeared(view: &WorldView, row: &RealmChatEvent) {
    let row = Arc::new(row.clone());
    for &recipient in &row.recipients {
        let Some(viewer) = view
            .session_of_owner(recipient)
            .and_then(|session| view.viewer(session))
        else {
            continue;
        };
        if !crate::codec::social::subscriptions::private_recipient_audience(
            recipient,
            viewer.self_guid,
        ) {
            continue;
        }
        if row.ignorable && viewer.ignores(row.speaker_guid) {
            continue;
        }
        let row = row.clone();
        enqueue(viewer.clone(), move |_| {
            crate::codec::social::subscriptions::realm_chat_outbound(&row)
        });
    }
}

pub(crate) fn contact_changed(view: &WorldView, shard: ShardId, row: &ContactEntry, present: bool) {
    let Some(viewer) = view.viewer_of_owner_on_shard(shard, OwnerGuid(row.owner_guid)) else {
        return;
    };
    if row.is_ignore {
        viewer.set_ignored(row.target_guid, present);
    } else {
        viewer.set_friend(row.target_guid, present);
    }
}

// Resubscribe replays inserts but not missed deletes; rebuild both contact sets.
pub(crate) fn replace_contact_sets(view: &WorldView, shard: ShardId, contacts: &[ContactEntry]) {
    let mut ignored: HashMap<u64, HashSet<u64>> = HashMap::new();
    let mut friends: HashMap<u64, HashSet<u64>> = HashMap::new();
    for row in contacts {
        let by_owner = if row.is_ignore {
            &mut ignored
        } else {
            &mut friends
        };
        by_owner
            .entry(row.owner_guid)
            .or_default()
            .insert(row.target_guid);
    }
    for viewer in view.viewers_on_shard(shard) {
        *viewer.ignored.lock().unwrap_or_else(|p| p.into_inner()) =
            ignored.get(&viewer.self_guid).cloned().unwrap_or_default();
        *viewer.friends.lock().unwrap_or_else(|p| p.into_inner()) =
            friends.get(&viewer.self_guid).cloned().unwrap_or_default();
    }
}

pub(crate) fn friend_notice_candidates(view: &WorldView, subject_guid: u64) -> Vec<Arc<Viewer>> {
    view.all_viewers()
        .into_iter()
        .filter(|viewer| viewer.self_guid != subject_guid && viewer.is_friend(subject_guid))
        .collect()
}

pub(crate) fn character_home_changed(
    view: &WorldView,
    shard: ShardId,
    old: &Character,
    new: &Character,
) {
    let home = (
        new.home_x,
        new.home_y,
        new.home_z,
        new.home_map,
        new.home_zone,
    );
    if home
        == (
            old.home_x,
            old.home_y,
            old.home_z,
            old.home_map,
            old.home_zone,
        )
    {
        return;
    }
    let Some(viewer) = view.viewer_of_owner_on_shard(shard, OwnerGuid(new.guid)) else {
        return;
    };
    enqueue(viewer, move |_| {
        let mut body = Vec::with_capacity(20);
        for coordinate in [home.0, home.1, home.2] {
            body.extend_from_slice(&coordinate.to_le_bytes());
        }
        body.extend_from_slice(&home.3.to_le_bytes());
        body.extend_from_slice(&home.4.to_le_bytes());
        vec![Outbound::Raw {
            opcode: 0x0155,
            body,
        }]
    });
}

pub(crate) fn character_online_changed(view: &WorldView, old: &Character, new: &Character) {
    if new.session_start_micros == 0 || old.session_start_micros == new.session_start_micros {
        return;
    }
    debug_assert!(
        new.online,
        "player_login always sets online alongside session_start_micros"
    );
    view.mark_character_online(new.guid);
    let team = lyracore_shared::faction::team_for_race(new.race);
    let online_fields = crate::codec::FriendOnline {
        away: crate::world::presence::AwayStatus::None,
        zone_id: new.zone_id,
        level: new.level,
        class: new.class,
    };
    let subject_guid = new.guid;
    for viewer in friend_notice_candidates(view, subject_guid) {
        if viewer.team != team {
            continue;
        }
        let online_fields = online_fields.clone();
        enqueue(viewer, move |_| {
            let (opcode, body) = crate::codec::build_friend_status_raw(
                wow_world_base::shared::friend_result_vanilla_tbc::FriendResult::Online,
                subject_guid,
                Some(online_fields),
            );
            vec![Outbound::Raw { opcode, body }]
        });
    }
}

pub(crate) fn claim_closed(old: Option<&AccountClaim>, new: &AccountClaim) -> Option<u64> {
    let old = old?;
    let replaced = old.generation != new.generation || old.character_guid != new.character_guid;
    let was_live = !old.closed;
    (was_live && (replaced || new.closed)).then_some(old.character_guid)
}

pub(crate) fn claim_close_team(coord: &Coordinator, subject_guid: u64) -> Option<u32> {
    let identity = crate::world::presence::character_identity_anywhere(coord, subject_guid)
        .ok()
        .flatten()?;
    Some(lyracore_shared::faction::team_for_race(identity.race))
}

pub(crate) fn account_claim_changed(
    view: &WorldView,
    coord: &Coordinator,
    old: Option<&AccountClaim>,
    new: &AccountClaim,
) {
    let Some(subject_guid) = claim_closed(old, new) else {
        return;
    };
    if !view.take_character_online(subject_guid) {
        return;
    }
    let candidates = friend_notice_candidates(view, subject_guid);
    if candidates.is_empty() {
        return;
    }
    let team: Arc<OnceLock<Option<u32>>> = Arc::new(OnceLock::new());
    for viewer in candidates {
        let (coord, team) = (coord.clone(), team.clone());
        let viewer_team = viewer.team;
        enqueue(viewer, move |_| {
            let Some(subject_team) = *team.get_or_init(|| claim_close_team(&coord, subject_guid))
            else {
                return Vec::new();
            };
            if subject_team != viewer_team {
                return Vec::new();
            }
            let (opcode, body) = crate::codec::build_friend_status_raw(
                wow_world_base::shared::friend_result_vanilla_tbc::FriendResult::Offline,
                subject_guid,
                None,
            );
            vec![Outbound::Raw { opcode, body }]
        });
    }
}

pub(crate) fn system_message_appeared(view: &WorldView, row: &SystemMessageEvent) {
    let Some(session) = view.session_of_owner(row.recipient_guid) else {
        return;
    };
    let Some(viewer) = view.viewer(session) else {
        return;
    };
    if !crate::codec::social::subscriptions::private_recipient_audience(
        row.recipient_guid,
        viewer.self_guid,
    ) {
        return;
    }
    let row = row.clone();
    enqueue(viewer.clone(), move |_| {
        crate::codec::social::subscriptions::system_message_event_outbound(&row)
    });
}

pub(crate) fn group_event_appeared(
    view: &WorldView,
    coord: &Coordinator,
    source: GroupEventSource,
    row: &GroupEvent,
    snapshot: &mut GroupEventSnapshot,
    read_table: impl FnOnce() -> Vec<GroupEvent>,
) {
    let Some(session) = view.session_of_owner(row.recipient_guid) else {
        return;
    };
    let Some(viewer) = view.viewer(session) else {
        return;
    };
    if !crate::codec::social::subscriptions::private_recipient_audience(
        row.recipient_guid,
        viewer.self_guid,
    ) {
        return;
    }
    let rows = snapshot.take(&viewer.group_events, source, row, read_table);
    if rows.is_empty() {
        return;
    }
    let coord = coord.clone();
    let self_guid = viewer.self_guid;
    enqueue(viewer.clone(), move |viewer| {
        let mut packets = Vec::new();
        for row in &rows {
            packets.extend(crate::codec::social::subscriptions::group_event_outbound(
                &coord, self_guid, row,
            ));
            if row.kind == lyracore_shared::group::event_kind::LIST {
                viewer.member_stats.forget_all();
            }
        }
        packets
    });
}

pub(crate) fn group_member_mirror_changed(
    view: &WorldView,
    coord: &Coordinator,
    shard: ShardId,
    row: &GroupMember,
) {
    let Some(viewer) = view.viewer_of_owner_on_shard(shard, OwnerGuid(row.character_guid)) else {
        return;
    };
    let coord = coord.clone();
    enqueue(viewer.clone(), move |viewer| {
        crate::codec::entity::subscriptions::loot_tag_flags_after_membership_change(&coord, &viewer)
    });
}

pub(crate) fn emote_audience(
    view: &WorldView,
    shard: ShardId,
    row: &EmoteEvent,
) -> Vec<Arc<Viewer>> {
    let key = CellKey::at(row.map_id, row.instance_id, row.grid_x, row.grid_y);
    view.cell_audience(
        shard,
        Some(key),
        cells_within(lyracore_shared::chat::broadcast_chat::TEXT_EMOTE_RANGE_YD),
        &[row.sender_guid, row.target_guid],
    )
}

pub(crate) fn emote_appeared(
    view: &WorldView,
    coord: &Coordinator,
    shard: ShardId,
    row: &EmoteEvent,
) {
    let row = Arc::new(row.clone());
    for viewer in emote_audience(view, shard, &row) {
        let (row, coord) = (row.clone(), coord.clone());
        let self_guid = viewer.self_guid;
        enqueue(viewer.clone(), move |_| {
            crate::codec::social::subscriptions::emote_event_outbound(&coord, self_guid, &row)
        });
    }
}

pub(crate) fn chat_audience(view: &WorldView, shard: ShardId, row: &ChatEvent) -> Vec<Arc<Viewer>> {
    if row.chat_type == lyracore_shared::chat::broadcast_chat::CREATURE_WHISPER {
        return view
            .viewers_on_shard(shard)
            .into_iter()
            .filter(|viewer| viewer.self_guid == row.target_guid)
            .collect();
    }
    let key = view
        .spatial
        .entity_cell_on_shard(EntityLayer::WorldEntity, row.sender_guid, shard);
    if lyracore_shared::chat::broadcast_chat::is_zone_wide(row.chat_type) {
        return view.viewers_on_shard(shard);
    }
    let span = cells_within(crate::codec::social::subscriptions::chat_range_yd(
        row.chat_type,
    ));
    view.cell_audience(shard, key, span, &[row.sender_guid, row.target_guid])
}

pub(crate) fn chat_appeared(
    view: &WorldView,
    coord: &Coordinator,
    shard: ShardId,
    row: &ChatEvent,
) {
    let row = Arc::new(row.clone());
    for viewer in chat_audience(view, shard, &row) {
        let (row, coord) = (row.clone(), coord.clone());
        let self_guid = viewer.self_guid;
        enqueue(viewer.clone(), move |_| {
            crate::codec::social::subscriptions::chat_event_outbound(&coord, self_guid, &row)
        });
    }
}

crate::stdb::relays::declare_relays! {
    game_instance_removal [false] {
        insert(Snapshot) => |context, _event, row| instance_removal_started(&context.view, context.shard, row);
        delete(Snapshot) => |context, _event, row| instance_removal_ended(&context.view, context.shard, row);
    }
    game_system_message_event [false] {
        insert(Live) => |context, _event, row| system_message_appeared(&context.view, row);
    }
    game_realm_chat_event [true] {
        insert(Live) => |context, _event, row| realm_chat_appeared(&context.view, row);
    }
    game_character_contact [false] {
        insert(Snapshot) => |context, _event, row| contact_changed(&context.view, context.shard, row, true);
        delete(Snapshot) => |context, _event, row| contact_changed(&context.view, context.shard, row, false);
    }
    game_account_claim [true] {
        update(Snapshot) => |context, _event, old, new| account_claim_changed(&context.view, &context.coord, Some(old), new);
    }
    game_character [false] {
        update(Snapshot) => |context, _event, old, new| {
            character_home_changed(&context.view, context.shard, old, new);
            character_online_changed(&context.view, old, new);
        };
    }
    game_group_member [false] {
        insert(Snapshot) => |context, _event, row| group_member_mirror_changed(&context.view, &context.coord, context.shard, row);
        delete(Snapshot) => |context, _event, row| group_member_mirror_changed(&context.view, &context.coord, context.shard, row);
    }
    game_emote_event [false] {
        insert(Snapshot) => |context, _event, row| emote_appeared(&context.view, &context.coord, context.shard, row);
    }
    game_chat_event [false] {
        insert(Snapshot) => |context, _event, row| chat_appeared(&context.view, &context.coord, context.shard, row);
    }
    game_group_event [true] {
        insert(Live) => group_event_callback();
    }
}

fn group_event_callback(
) -> impl FnMut(&crate::stdb::relays::RelayContext, &EventContext, &GroupEvent) + Send {
    let mut snapshot = GroupEventSnapshot::default();
    move |context, event, row| {
        group_event_appeared(
            &context.view,
            &context.coord,
            context.source,
            row,
            &mut snapshot,
            || event.db.game_group_event().iter().collect(),
        );
    }
}

#[cfg(test)]
mod packet_tests;

#[cfg(test)]
mod audience_tests;

#[cfg(test)]
mod realm_chat_relay_tests;

#[cfg(test)]
mod account_claim_relay_tests;
