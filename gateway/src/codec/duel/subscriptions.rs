use crate::codec::{self};
use crate::stdb::bindings::*;
use crate::stdb::world_index::{CellKey, ShardId};
use crate::stdb::world_view::*;
use crate::stdb::Coordinator;
use crate::world::Outbound;
use lyracore_shared::spatial::BOX_HALF_SPAN;
use std::sync::Arc;
use std::time::Duration;
use wow_world_messages::vanilla::opcodes::ServerOpcodeMessage;

pub(crate) fn duel_event_outbound(
    row: &DuelEvent,
    template: Option<&codec::GameObjectTemplateView>,
) -> Vec<Outbound> {
    use lyracore_shared::duel::{completion_kind, event_kind};
    use wow_world_messages::vanilla::{
        SMSG_DUEL_COMPLETE, SMSG_DUEL_COUNTDOWN, SMSG_DUEL_REQUESTED,
    };

    let raw_values = |guid, arbiter, team| {
        let (opcode, body) = codec::build_duel_player_values(guid, arbiter, team);
        Outbound::Raw { opcode, body }
    };

    match row.kind {
        event_kind::REQUESTED => {
            let Some(template) = template else {
                log::warn!(
                    "duel relay: missing duel-flag template {} (event {})",
                    row.flag_entry,
                    row.id
                );
                return Vec::new();
            };
            let flag = codec::GameObjectView {
                guid: row.flag_guid,
                template_entry: row.flag_entry,
                x: row.flag_x,
                y: row.flag_y,
                z: row.flag_z,
                orientation: row.flag_orientation,
                state: 1,
                type_id: template.type_id,
                display_id: template.display_id,
                rotation_0: 0.0,
                rotation_1: 0.0,
                rotation_2: 0.0,
                rotation_3: 0.0,
                size: 1.0,
            };
            let (rotation_opcode, rotation_body) = codec::build_gameobject_rotation_values(&flag);
            vec![
                Outbound::One(ServerOpcodeMessage::SMSG_UPDATE_OBJECT(Box::new(
                    codec::build_gameobject_create_object(&flag),
                ))),
                Outbound::Raw {
                    opcode: rotation_opcode,
                    body: rotation_body,
                },
                raw_values(row.initiator_guid, Some(row.flag_guid), None),
                raw_values(row.challenged_guid, Some(row.flag_guid), None),
                Outbound::One(ServerOpcodeMessage::SMSG_DUEL_REQUESTED(Box::new(
                    SMSG_DUEL_REQUESTED {
                        // The protocol calls this `initiator`; vanilla uses it as the arbiter GO.
                        initiator: wow_world_messages::Guid::new(row.flag_guid),
                        target: wow_world_messages::Guid::new(row.initiator_guid),
                    },
                ))),
            ]
        }
        event_kind::COUNTDOWN => vec![Outbound::One(ServerOpcodeMessage::SMSG_DUEL_COUNTDOWN(
            SMSG_DUEL_COUNTDOWN {
                // The binding writes as_secs(), but vanilla reads the value as milliseconds.
                time: Duration::from_secs(3_000),
            },
        ))],
        event_kind::ACTIVE => vec![
            raw_values(row.initiator_guid, None, Some(1)),
            raw_values(row.challenged_guid, None, Some(2)),
        ],
        event_kind::OUT_OF_BOUNDS => {
            vec![Outbound::One(ServerOpcodeMessage::SMSG_DUEL_OUTOFBOUNDS)]
        }
        event_kind::IN_BOUNDS => vec![Outbound::One(ServerOpcodeMessage::SMSG_DUEL_INBOUNDS)],
        event_kind::COMPLETE => {
            let mut outbound = vec![Outbound::One(ServerOpcodeMessage::SMSG_DUEL_COMPLETE(
                SMSG_DUEL_COMPLETE {
                    ended_without_interruption: row.completion_kind != completion_kind::INTERRUPTED,
                },
            ))];
            outbound.extend(duel_winner_outbound(row));
            outbound.extend([
                raw_values(row.initiator_guid, Some(0), Some(0)),
                raw_values(row.challenged_guid, Some(0), Some(0)),
                Outbound::One(ServerOpcodeMessage::SMSG_DESTROY_OBJECT(
                    codec::build_destroy_object(row.flag_guid),
                )),
            ]);
            outbound
        }
        other => {
            log::warn!("duel relay: unknown kind {other} (event {})", row.id);
            Vec::new()
        }
    }
}

pub(crate) fn duel_winner_outbound(row: &DuelEvent) -> Vec<Outbound> {
    use lyracore_shared::duel::completion_kind;
    use wow_world_base::shared::duel_winner_reason_vanilla_tbc_wrath::DuelWinnerReason;
    use wow_world_messages::vanilla::SMSG_DUEL_WINNER;

    if row.winner_guid == 0
        || row.loser_guid == 0
        || row.winner_name.is_empty()
        || row.loser_name.is_empty()
    {
        return Vec::new();
    }
    let reason = match row.completion_kind {
        completion_kind::WON => DuelWinnerReason::Won,
        completion_kind::FLED => DuelWinnerReason::Fled,
        _ => return Vec::new(),
    };
    // The first CString, named opponent_name by the binding, carries the winner.
    vec![Outbound::One(ServerOpcodeMessage::SMSG_DUEL_WINNER(
        Box::new(SMSG_DUEL_WINNER {
            reason,
            opponent_name: row.winner_name.clone(),
            initiator_name: row.loser_name.clone(),
        }),
    ))]
}

pub(crate) fn duel_event_appeared(
    view: &WorldView,
    coord: &Coordinator,
    shard: ShardId,
    row: &DuelEvent,
) {
    if let Some(viewer) = view
        .viewer_of_owner_on_shard(shard, OwnerGuid(row.recipient_guid))
        .filter(|viewer| {
            crate::codec::social::subscriptions::private_recipient_audience(
                row.recipient_guid,
                viewer.self_guid,
            )
        })
    {
        let row = row.clone();
        let coord = coord.clone();
        enqueue(viewer.clone(), move |_| {
            let template = (row.kind == lyracore_shared::duel::event_kind::REQUESTED)
                .then(|| coord.gameobject_template(row.flag_entry).ok().flatten())
                .flatten();
            crate::codec::duel::subscriptions::duel_event_outbound(&row, template.as_ref())
        });
    }

    if row.kind != lyracore_shared::duel::event_kind::COMPLETE
        || row.recipient_guid != row.initiator_guid
        || row.winner_guid == 0
    {
        return;
    }
    let key = CellKey::of_position(row.map_id, row.instance_id, row.flag_x, row.flag_y);
    let row = Arc::new(row.clone());
    for viewer in view.cell_audience(shard, Some(key), BOX_HALF_SPAN, &[]) {
        if !duel_winner_audience(viewer.self_guid, row.initiator_guid, row.challenged_guid) {
            continue;
        }
        let row = row.clone();
        enqueue(viewer.clone(), move |_| {
            crate::codec::duel::subscriptions::duel_winner_outbound(&row)
        });
    }
}

pub(crate) fn duel_winner_audience(
    viewer_guid: u64,
    initiator_guid: u64,
    challenged_guid: u64,
) -> bool {
    viewer_guid != 0 && viewer_guid != initiator_guid && viewer_guid != challenged_guid
}

crate::stdb::relays::declare_relays! {
    game_duel_event [false] {
        insert(Live) => |context, _event, row| duel_event_appeared(&context.view, &context.coord, context.shard, row);
    }
}

#[cfg(test)]
mod packet_tests;

#[cfg(test)]
mod audience_tests;
