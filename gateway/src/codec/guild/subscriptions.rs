//! Table Relays for this Protocol Family.

use crate::codec::{self};
use crate::stdb::bindings::*;
use crate::stdb::world_index::EntityLayer;
use crate::stdb::world_view::*;
use crate::stdb::Coordinator;
use crate::world::Outbound;
use std::sync::{Arc, OnceLock};
use wow_world_messages::vanilla::opcodes::ServerOpcodeMessage;

/// The Guild Projection of `character_guid`, read from the Realm-core cache. `(0, 0)` when
/// Realm-core cannot answer: the next membership relay corrects it.
pub(crate) fn guild_projection_of(coord: &Coordinator, character_guid: u64) -> (u32, u32) {
    coord
        .realm_core()
        .map_or((0, 0), |realm| realm.guild_projection(character_guid))
}

/// Relay the current Guild Projection of `character_guid` to a viewer that holds its entity: the
/// member itself or a viewer whose `created` set has the guid. `projection` is read when the job
/// runs, so a delayed job never writes a value older than the cache.
pub(crate) fn guild_values_outbound(
    viewer: &Viewer,
    character_guid: u64,
    projection: (u32, u32),
) -> Vec<Outbound> {
    if viewer.self_guid != character_guid
        && !viewer.created.lock().unwrap().contains(&character_guid)
    {
        return Vec::new();
    }
    let (guild_id, rank_id) = projection;
    let (opcode, body) = codec::build_guild_values(character_guid, guild_id, rank_id);
    vec![Outbound::Raw { opcode, body }]
}

/// Render one Guild Event row. The row carries its final strings. Kinds 0x40 (INVITE) and 0x41
/// (DECLINE) are membership offers with their own wire shape; every other kind not built from a
/// shared snapshot (see `GuildEventSnapshot`) rides the generic SMSG_GUILD_EVENT builder.
pub(crate) fn guild_event_outbound(row: &GuildEvent) -> Vec<Outbound> {
    match row.kind {
        lyracore_shared::guild::event_kind::INVITE => {
            let actor_name = row.strings.first().cloned().unwrap_or_default();
            let guild_name = row.strings.get(1).cloned().unwrap_or_default();
            vec![Outbound::One(ServerOpcodeMessage::SMSG_GUILD_INVITE(
                Box::new(codec::build_guild_invite(actor_name, guild_name)),
            ))]
        }
        lyracore_shared::guild::event_kind::DECLINE => {
            let actor_name = row.strings.first().cloned().unwrap_or_default();
            let (opcode, body) = codec::build_guild_decline_raw(&actor_name);
            vec![Outbound::Raw { opcode, body }]
        }
        _ => {
            let (opcode, body) =
                codec::build_guild_event_raw(row.kind, &row.strings, row.subject_guid);
            vec![Outbound::Raw { opcode, body }]
        }
    }
}

/// Render one petition Guild Event (`0x70..=0x7F`). The Charter is in `other_guid` and the
/// signer or decliner in `subject_guid`. `petition` is the Petition of that Charter, read when the
/// job runs; only PETITION_OFFERED and PETITION_CHANGED need it, and a Petition that is gone by
/// then renders nothing.
pub(crate) fn petition_event_outbound(
    row: &GuildEvent,
    petition: Option<&codec::PetitionView>,
) -> Vec<Outbound> {
    use lyracore_shared::guild::event_kind;
    use wow_world_messages::vanilla::{
        GuildCommand, GuildCommandResult, PetitionResult, MSG_PETITION_DECLINE,
    };
    let sign_results = |result| {
        vec![Outbound::One(
            ServerOpcodeMessage::SMSG_PETITION_SIGN_RESULTS(Box::new(
                codec::build_petition_sign_results(row.other_guid, row.subject_guid, result),
            )),
        )]
    };
    match row.kind {
        event_kind::PETITION_OFFERED => petition
            .map(|petition| {
                Outbound::One(ServerOpcodeMessage::SMSG_PETITION_SHOW_SIGNATURES(
                    Box::new(codec::build_petition_show_signatures(petition)),
                ))
            })
            .into_iter()
            .collect(),
        event_kind::PETITION_SIGNED => sign_results(PetitionResult::Ok),
        event_kind::PETITION_ALREADY_SIGNED => sign_results(PetitionResult::AlreadySigned),
        event_kind::PETITION_DECLINED => vec![Outbound::One(
            ServerOpcodeMessage::MSG_PETITION_DECLINE(MSG_PETITION_DECLINE {
                petition: wow_world_messages::Guid::new(row.subject_guid),
            }),
        )],
        // Result 0 is success; gtker names the zero value PLAYER_NO_MORE_IN_GUILD.
        event_kind::FOUNDER => vec![Outbound::One(
            ServerOpcodeMessage::SMSG_GUILD_COMMAND_RESULT(Box::new(
                codec::build_guild_command_result(
                    GuildCommand::Founder,
                    row.strings.first().cloned().unwrap_or_default(),
                    GuildCommandResult::PlayerNoMoreInGuild,
                ),
            )),
        )],
        event_kind::PETITION_CHANGED => petition
            .map(|petition| {
                Outbound::One(ServerOpcodeMessage::SMSG_PETITION_QUERY_RESPONSE(Box::new(
                    codec::build_petition_query_response(petition),
                )))
            })
            .into_iter()
            .collect(),
        _ => Vec::new(),
    }
}

pub(crate) fn guild_membership_read(realm: &Coordinator) -> GuildMembershipRead {
    let realm = realm.clone();
    Arc::new(move |character_guid| {
        realm
            .0
            .coord()
            .conn
            .db
            .game_guild_member()
            .character_guid()
            .find(&character_guid)
            .map(|member| (member.guild_id, member.rank_id))
    })
}

pub(crate) fn guild_roster_snapshot_read(realm: &Coordinator) -> GuildRosterSnapshotRead {
    let realm = realm.clone();
    Arc::new(move |guild_id, kind| crate::world::GuildEventSnapshot::build(&realm, guild_id, kind))
}

pub(crate) fn petition_read(realm: &Coordinator) -> PetitionRead {
    let realm = realm.clone();
    Arc::new(move |charter_item_guid| realm.guild_petition_of_charter(charter_item_guid))
}

/// A membership row changed: re-send the member's Guild Projection to the member and to every
/// viewer that holds its entity. The member is always addressed, as `cell_audience` keeps its owner
/// leg, so an entity the cell index has not seen yet cannot hide a removal from the member's own
/// client. The job reads membership when it runs, so after a delete it sends 0 and 0.
pub(crate) fn guild_membership_changed(
    view: &WorldView,
    membership: &GuildMembershipRead,
    character_guid: u64,
) {
    let mut sessions = view
        .spatial
        .shard_of(EntityLayer::WorldEntity, character_guid)
        .and_then(|shard| {
            view.spatial
                .entity_cell_on_shard(EntityLayer::WorldEntity, character_guid, shard)
                .map(|key| view.world_entity_recipients(shard, character_guid, key))
        })
        .unwrap_or_default();
    if let Some(owner) = view.session_of_owner(character_guid) {
        if !sessions.contains(&owner) {
            sessions.push(owner);
        }
    }
    for session in sessions {
        let Some(viewer) = view.viewer(session) else {
            continue;
        };
        let membership = membership.clone();
        enqueue(viewer, move |viewer| {
            let projection = membership(character_guid).unwrap_or((0, 0));
            crate::codec::guild::subscriptions::guild_values_outbound(
                &viewer,
                character_guid,
                projection,
            )
        });
    }
}

/// A Guild Event landed. An addressed row reaches its recipient; a broadcast row reaches every
/// online member on this Gateway. A member signing on does not hear its own SIGNED_ON: mangos
/// broadcasts it before the Character is in the world (`cm:CharacterHandler.cpp:787`).
///
/// `TABARD_CHANGED`, `ROSTER_TO_ACTOR` and `ROSTER_REFRESH` also carry a query response, a roster,
/// or both; the first recipient's job reads the Guild (and, for the two roster kinds, its roster)
/// once through `snapshot_read` and every other recipient's job reuses that one snapshot, so a
/// broadcast to a large Guild costs one Realm-core read, not one per online member.
pub(crate) fn guild_event_appeared(
    view: &WorldView,
    membership: &GuildMembershipRead,
    snapshot_read: &GuildRosterSnapshotRead,
    row: &GuildEvent,
) {
    let audience: Vec<Arc<Viewer>> = if row.recipient_guid != 0 {
        view.session_of_owner(row.recipient_guid)
            .and_then(|session| view.viewer(session))
            .filter(|viewer| {
                crate::codec::social::subscriptions::private_recipient_audience(
                    row.recipient_guid,
                    viewer.self_guid,
                )
            })
            .into_iter()
            .collect()
    } else {
        online_audience(view, membership, row.guild_id)
    };
    let row = Arc::new(row.clone());
    let shared_snapshot: Arc<OnceLock<crate::world::GuildEventSnapshot>> =
        Arc::new(OnceLock::new());
    for viewer in audience {
        if row.recipient_guid == 0
            && row.kind == lyracore_shared::guild::event_kind::SIGNED_ON
            && viewer.self_guid == row.subject_guid
        {
            continue;
        }
        let (row, membership, snapshot_read, shared_snapshot) = (
            row.clone(),
            membership.clone(),
            snapshot_read.clone(),
            shared_snapshot.clone(),
        );
        enqueue(viewer, move |viewer| {
            use lyracore_shared::guild::event_kind;
            match row.kind {
                event_kind::TABARD_CHANGED => {
                    let snapshot =
                        shared_snapshot.get_or_init(|| snapshot_read(row.guild_id, row.kind));
                    let mut out = crate::codec::guild::subscriptions::guild_event_outbound(&row);
                    out.extend(snapshot.query_response().map(Outbound::One));
                    out
                }
                event_kind::ROSTER_TO_ACTOR => {
                    let Some((_, rank_id)) =
                        membership(viewer.self_guid).filter(|(id, _)| *id == row.guild_id)
                    else {
                        return Vec::new();
                    };
                    let snapshot =
                        shared_snapshot.get_or_init(|| snapshot_read(row.guild_id, row.kind));
                    snapshot
                        .roster_for_rank(rank_id)
                        .into_iter()
                        .map(Outbound::One)
                        .collect()
                }
                event_kind::ROSTER_REFRESH => {
                    let Some((_, rank_id)) =
                        membership(viewer.self_guid).filter(|(id, _)| *id == row.guild_id)
                    else {
                        return Vec::new();
                    };
                    let snapshot =
                        shared_snapshot.get_or_init(|| snapshot_read(row.guild_id, row.kind));
                    let mut out: Vec<Outbound> = snapshot
                        .query_response()
                        .map(Outbound::One)
                        .into_iter()
                        .collect();
                    out.extend(snapshot.roster_for_rank(rank_id).map(Outbound::One));
                    out
                }
                _ => crate::codec::guild::subscriptions::guild_event_outbound(&row),
            }
        });
    }
}

/// A petition Guild Event landed: it reaches its one addressed recipient. The two kinds that show
/// a Petition read it when the job runs, once per event.
pub(crate) fn petition_event_appeared(
    view: &WorldView,
    petitions: &PetitionRead,
    row: &GuildEvent,
) {
    let Some(viewer) = view
        .session_of_owner(row.recipient_guid)
        .and_then(|session| view.viewer(session))
        .filter(|viewer| {
            crate::codec::social::subscriptions::private_recipient_audience(
                row.recipient_guid,
                viewer.self_guid,
            )
        })
    else {
        return;
    };
    let (row, petitions) = (row.clone(), petitions.clone());
    enqueue(viewer, move |_viewer| {
        use lyracore_shared::guild::event_kind;
        let petition = match row.kind {
            event_kind::PETITION_OFFERED | event_kind::PETITION_CHANGED => {
                petitions(row.other_guid)
            }
            _ => None,
        };
        crate::codec::guild::subscriptions::petition_event_outbound(&row, petition.as_ref())
    });
}

/// A Petition opened: its owner's Guild Charter shows the Petition id in ITEM_FIELD_ENCHANTMENT,
/// where the client reads it back for its petition queries (`cm:PetitionsHandler.cpp:141`).
pub(crate) fn charter_petition_opened(view: &WorldView, row: &GuildPetition) {
    let Some(viewer) = view
        .session_of_owner(row.owner_guid)
        .and_then(|session| view.viewer(session))
        .filter(|viewer| viewer.self_guid == row.owner_guid)
    else {
        return;
    };
    let (charter_item_guid, petition_id) = (row.charter_item_guid, row.petition_id);
    enqueue(viewer, move |_viewer| {
        let (opcode, body) =
            crate::codec::build_charter_petition_values(charter_item_guid, petition_id);
        vec![Outbound::Raw { opcode, body }]
    });
}

crate::stdb::relays::declare_relays! {
    game_guild_event [true] {
        insert(Live) => |context, _event, row| { if lyracore_shared::guild::event_kind::is_petition(row.kind) { petition_event_appeared(&context.view, &petition_read(&context.realm), row); } else { guild_event_appeared(&context.view, &guild_membership_read(&context.realm), &guild_roster_snapshot_read(&context.realm), row); } };
    }
    game_guild_petition [true] {
        insert(Snapshot) => |context, _event, row| charter_petition_opened(&context.view, row);
    }
    game_guild_member [true] {
        insert(Snapshot) => |context, _event,  row| guild_membership_changed(&context.view, &guild_membership_read(&context.realm), row.character_guid);
        update(Snapshot) => |context, _event,  _old, row| guild_membership_changed(&context.view, &guild_membership_read(&context.realm), row.character_guid);
        delete(Snapshot) => |context, _event,  row| guild_membership_changed(&context.view, &guild_membership_read(&context.realm), row.character_guid);
    }
}
