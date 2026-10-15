use super::party::PartyOutcome;
use super::{
    party, presence, who, Actor, Outbound, ProtocolFamily, ProtocolReply, ProtocolRequest,
    ProtocolSession, WorldState,
};
use crate::codec;
use crate::world::{CharacterStore, GuildActionStore, PartyStore, SessionStore, ShardRoutingStore};
use anyhow::{anyhow, Result};
use lyracore_shared::group::{GroupRefusal, TARGET_ICON_LIST_REQUEST};
use lyracore_shared::social::ContactRefusal;
use wow_world_base::shared::friend_result_vanilla_tbc::FriendResult;
use wow_world_messages::vanilla::opcodes::{ClientOpcodeMessage, ServerOpcodeMessage};
use wow_world_messages::vanilla::{MSG_RAID_TARGET_UPDATE_Client, PartyOperation, PartyResult};
use wow_world_messages::Guid;

pub(crate) trait SocialStore: Send + Sync {
    fn character_identity(&self, guid: u64) -> Result<Option<presence::CharacterIdentity>>;

    fn live_entity(&self, guid: u64) -> Option<codec::MemberEntity>;

    fn character_in_transit(&self, guid: u64) -> bool;

    fn auto_reply_text(&self, guid: u64) -> Result<Option<String>>;

    fn every_shard_vouches_for_absence(&self) -> Result<()>;

    fn in_world_players(&self) -> Result<Vec<presence::RealmPresence>>;

    fn zone_name(&self, zone_id: u32) -> String;

    fn contact_lists(&self, self_guid: u64) -> Result<(Vec<u64>, Vec<u64>)>;

    fn ignored_guids(&self, owner_guid: u64) -> Result<Vec<u64>>;

    fn character_guid_by_name(&self, name: &str) -> Result<Option<u64>>;

    fn add_friend(&self, actor: Actor, target_guid: u64, target_race: u8)
        -> Result<ContactOutcome>;

    fn del_friend(&self, actor: Actor, target_guid: u64) -> Result<ContactOutcome>;

    fn add_ignore(&self, actor: Actor, target_guid: u64) -> Result<ContactOutcome>;

    fn del_ignore(&self, actor: Actor, target_guid: u64) -> Result<ContactOutcome>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ContactOutcome {
    Done,
    Refused(ContactRefusal),
}

impl From<ContactRefusal> for ContactOutcome {
    fn from(refusal: ContactRefusal) -> Self {
        Self::Refused(refusal)
    }
}

pub(crate) struct Social;

impl<
        St: CharacterStore
            + GuildActionStore
            + PartyStore
            + SessionStore
            + ShardRoutingStore
            + SocialStore
            + ?Sized,
    > ProtocolFamily<St> for Social
{
    #[allow(clippy::too_many_lines)]
    fn handle(
        store: &St,
        session: &mut ProtocolSession,
        request: ProtocolRequest,
    ) -> Result<ProtocolReply> {
        let msg = request.message()?;
        let mut outbound = Vec::new();
        match msg {
            ClientOpcodeMessage::CMSG_WHO(request) => {
                if let Some(me) = session.self_guid() {
                    if session.admit_who() {
                        if let Some(character) = store.character_by_guid(me)? {
                            if let Some((opcode, body)) =
                                who::respond(store, character.race, &request)?
                            {
                                outbound.push(Outbound::Raw { opcode, body });
                            }
                        }
                    }
                }
            }
            ClientOpcodeMessage::CMSG_FRIEND_LIST => {
                if let WorldState::InWorld(iw) = &session.state {
                    let (friend_guids, ignored) = store.contact_lists(iw.self_guid)?;
                    let own_team = store
                        .character_by_guid(iw.self_guid)?
                        .map_or(lyracore_shared::faction::TEAM_ALLIANCE, |character| {
                            lyracore_shared::faction::team_for_race(character.race)
                        });
                    let friends = friend_views(store, own_team, &friend_guids);
                    outbound.push(Outbound::One(ServerOpcodeMessage::SMSG_FRIEND_LIST(
                        Box::new(codec::build_friend_list_response(&friends)),
                    )));
                    outbound.push(Outbound::One(ServerOpcodeMessage::SMSG_IGNORE_LIST(
                        Box::new(codec::build_ignore_list_response(&ignored)),
                    )));
                }
            }
            ClientOpcodeMessage::CMSG_ADD_FRIEND(c) => {
                if let Some(actor) = session.actor() {
                    let (result, guid, online) = resolve_add_contact(store, actor, &c.name, false)?;
                    let (opcode, body) = codec::build_friend_status_raw(result, guid, online);
                    outbound.push(Outbound::Raw { opcode, body });
                }
            }
            ClientOpcodeMessage::CMSG_ADD_IGNORE(c) => {
                if let Some(actor) = session.actor() {
                    let (result, guid, online) = resolve_add_contact(store, actor, &c.name, true)?;
                    let (opcode, body) = codec::build_friend_status_raw(result, guid, online);
                    outbound.push(Outbound::Raw { opcode, body });
                }
            }
            ClientOpcodeMessage::CMSG_DEL_FRIEND(c) => {
                if let Some(actor) = session.actor() {
                    let (result, guid) = resolve_del_contact(store, actor, c.guid.guid(), false)?;
                    let (opcode, body) = codec::build_friend_status_raw(result, guid, None);
                    outbound.push(Outbound::Raw { opcode, body });
                }
            }
            ClientOpcodeMessage::CMSG_DEL_IGNORE(c) => {
                if let Some(actor) = session.actor() {
                    let (result, guid) = resolve_del_contact(store, actor, c.guid.guid(), true)?;
                    let (opcode, body) = codec::build_friend_status_raw(result, guid, None);
                    outbound.push(Outbound::Raw { opcode, body });
                }
            }
            ClientOpcodeMessage::CMSG_GROUP_INVITE(c) => {
                let result = match (session.actor(), presence::resolve_by_name(store, &c.name)?) {
                    (Some(me), Some(guid)) => {
                        party_result(party::run(store, me, party::Op::Invite(guid))?)
                    }
                    _ => PartyResult::BadPlayerName,
                };
                outbound.push(Outbound::One(
                    ServerOpcodeMessage::SMSG_PARTY_COMMAND_RESULT(Box::new(
                        codec::build_party_command_result(PartyOperation::Invite, c.name, result),
                    )),
                ));
            }
            ClientOpcodeMessage::CMSG_GROUP_ACCEPT => {
                run_unanswered(store, session, party::Op::Accept)?
            }
            ClientOpcodeMessage::CMSG_GROUP_DECLINE => {
                run_unanswered(store, session, party::Op::Decline)?
            }
            ClientOpcodeMessage::CMSG_GROUP_DISBAND => {
                outbound.extend(run_answering_refusal(store, session, party::Op::Leave)?)
            }
            ClientOpcodeMessage::CMSG_GROUP_UNINVITE(c) => {
                let result = match (session.actor(), presence::resolve_by_name(store, &c.name)?) {
                    (Some(me), Some(guid)) => {
                        party_result(party::run(store, me, party::Op::Uninvite(guid))?)
                    }
                    _ => PartyResult::BadPlayerName,
                };
                if result != PartyResult::Success {
                    outbound.push(Outbound::One(
                        ServerOpcodeMessage::SMSG_PARTY_COMMAND_RESULT(Box::new(
                            codec::build_party_command_result(
                                PartyOperation::Leave,
                                c.name,
                                result,
                            ),
                        )),
                    ));
                }
            }
            ClientOpcodeMessage::CMSG_LOOT_METHOD(c) => {
                let op = party::Op::LootMethod {
                    setting: c.loot_setting.as_int(),
                    master: c.loot_master.guid(),
                    threshold: c.loot_threshold.as_int(),
                };
                run_unanswered(store, session, op)?;
            }
            ClientOpcodeMessage::CMSG_GROUP_RAID_CONVERT => {
                outbound.extend(raid_convert(store, session)?)
            }
            ClientOpcodeMessage::CMSG_GROUP_SET_LEADER(c) => {
                run_unanswered(store, session, party::Op::SetLeader(c.guid.guid()))?
            }
            ClientOpcodeMessage::CMSG_GROUP_ASSISTANT_LEADER(c) => run_unanswered(
                store,
                session,
                party::Op::SetAssistant {
                    target: c.guid.guid(),
                    promote: c.set_assistant,
                },
            )?,
            ClientOpcodeMessage::CMSG_GROUP_UNINVITE_GUID(c)
                if session.self_guid() != Some(c.guid.guid()) =>
            {
                outbound.extend(run_answering_refusal(
                    store,
                    session,
                    party::Op::Uninvite(c.guid.guid()),
                )?)
            }
            ClientOpcodeMessage::CMSG_GROUP_UNINVITE_GUID(_) => {}
            ClientOpcodeMessage::CMSG_GROUP_CHANGE_SUB_GROUP(c) => {
                change_subgroup(store, session, &c.name, c.group_number)?
            }
            ClientOpcodeMessage::CMSG_GROUP_SWAP_SUB_GROUP(c) => {
                swap_subgroup(store, session, &c.name, &c.swap_with_name)?
            }
            ClientOpcodeMessage::MSG_RAID_READY_CHECK(c) => {
                let op = match c.answer {
                    None => party::Op::ReadyCheckStart,
                    Some(answer) => party::Op::ReadyCheckAnswer(answer.state),
                };
                run_group_broadcast(store, session, op);
            }
            ClientOpcodeMessage::MSG_RAID_TARGET_UPDATE(c) => {
                run_group_broadcast(store, session, target_icon_op(&c));
            }
            ClientOpcodeMessage::MSG_MINIMAP_PING(c) => {
                let op = party::Op::MinimapPing {
                    x: c.position_x,
                    y: c.position_y,
                };
                run_group_broadcast(store, session, op);
            }
            _ => return Err(anyhow!("opcode routed to the wrong Protocol Family")),
        }
        Ok(outbound.into())
    }
}

fn run_answering_refusal<
    St: CharacterStore + PartyStore + SessionStore + ShardRoutingStore + SocialStore + ?Sized,
>(
    store: &St,
    session: &ProtocolSession,
    op: party::Op,
) -> Result<Vec<Outbound>> {
    let Some(actor) = session.actor() else {
        return Ok(Vec::new());
    };
    match party::run(store, actor, op)? {
        PartyOutcome::Ran => Ok(Vec::new()),
        PartyOutcome::Refused(refusal) => Ok(vec![Outbound::One(
            ServerOpcodeMessage::SMSG_PARTY_COMMAND_RESULT(Box::new(
                codec::build_party_command_result(
                    PartyOperation::Leave,
                    String::new(),
                    party_result_for(refusal),
                ),
            )),
        )]),
    }
}

fn raid_convert<
    St: CharacterStore + PartyStore + SessionStore + ShardRoutingStore + SocialStore + ?Sized,
>(
    store: &St,
    session: &ProtocolSession,
) -> Result<Vec<Outbound>> {
    let Some(actor) = session.actor() else {
        return Ok(Vec::new());
    };
    match party::run(store, actor, party::Op::RaidConvert)? {
        PartyOutcome::Ran => Ok(vec![Outbound::One(
            ServerOpcodeMessage::SMSG_PARTY_COMMAND_RESULT(Box::new(
                codec::build_party_command_result(
                    PartyOperation::Invite,
                    String::new(),
                    PartyResult::Success,
                ),
            )),
        )]),
        PartyOutcome::Refused(refusal) => {
            log::debug!(
                "world: group_raid_convert refused (account {}): {refusal:?}",
                session.account_id
            );
            Ok(Vec::new())
        }
    }
}

fn run_unanswered<
    St: CharacterStore + PartyStore + SessionStore + ShardRoutingStore + SocialStore + ?Sized,
>(
    store: &St,
    session: &ProtocolSession,
    op: party::Op,
) -> Result<()> {
    let Some(me) = session.actor() else {
        return Ok(());
    };
    if let PartyOutcome::Refused(refusal) = party::run(store, me, op)? {
        log::debug!(
            "world: {op:?} refused (account {}): {refusal:?}",
            session.account_id
        );
    }
    Ok(())
}

fn change_subgroup<
    St: CharacterStore + PartyStore + SessionStore + ShardRoutingStore + SocialStore + ?Sized,
>(
    store: &St,
    session: &ProtocolSession,
    name: &str,
    subgroup: u8,
) -> Result<()> {
    let Some(me) = session.actor() else {
        return Ok(());
    };
    match party::resolve_roster_member_by_name(store, me.guid(), name)? {
        Some(target) => run_unanswered(
            store,
            session,
            party::Op::ChangeSubgroup { target, subgroup },
        ),
        None => {
            log::debug!(
                "world: group_change_sub_group named a Character outside the caller's own \
                 roster (account {})",
                session.account_id
            );
            Ok(())
        }
    }
}

fn swap_subgroup<
    St: CharacterStore + PartyStore + SessionStore + ShardRoutingStore + SocialStore + ?Sized,
>(
    store: &St,
    session: &ProtocolSession,
    name: &str,
    swap_with_name: &str,
) -> Result<()> {
    let Some(me) = session.actor() else {
        return Ok(());
    };
    let (first, second) =
        party::resolve_roster_members_by_name(store, me.guid(), name, swap_with_name)?;
    match (first, second) {
        (Some(first), Some(second)) => {
            run_unanswered(store, session, party::Op::SwapSubgroup { first, second })
        }
        _ => {
            log::debug!(
                "world: group_swap_sub_group named a Character outside the caller's own roster \
                 (account {})",
                session.account_id
            );
            Ok(())
        }
    }
}

pub(super) fn run_group_broadcast<
    St: CharacterStore + PartyStore + SessionStore + ShardRoutingStore + SocialStore + ?Sized,
>(
    store: &St,
    session: &mut ProtocolSession,
    op: party::Op,
) {
    let Some(me) = session.actor() else {
        return;
    };
    if !session.admit_group_broadcast(op) {
        log::debug!(
            "world: group broadcast {op:?} dropped inside its cooldown (account {})",
            session.account_id
        );
        return;
    }
    match party::run(store, me, op) {
        Ok(PartyOutcome::Ran) => {}
        Ok(PartyOutcome::Refused(refusal)) => log::debug!(
            "world: group broadcast {op:?} refused (account {}): {refusal:?}",
            session.account_id
        ),
        Err(error) => log::warn!(
            "world: group broadcast {op:?} lost (account {}): {error:#}",
            session.account_id
        ),
    }
}

fn target_icon_op(update: &MSG_RAID_TARGET_UPDATE_Client) -> party::Op {
    use MSG_RAID_TARGET_UPDATE_Client as Update;
    let (icon, target) = match *update {
        Update::RequestIcons => (TARGET_ICON_LIST_REQUEST, Guid::zero()),
        Update::Unknown0 { target } => (0, target),
        Update::Unknown1 { target } => (1, target),
        Update::Unknown2 { target } => (2, target),
        Update::Unknown3 { target } => (3, target),
        Update::Unknown4 { target } => (4, target),
        Update::Unknown5 { target } => (5, target),
        Update::Unknown6 { target } => (6, target),
        Update::Unknown7 { target } => (7, target),
        Update::Unknown8 { target } => (8, target),
    };
    party::Op::TargetIcon {
        icon,
        target: target.guid(),
    }
}

fn party_result(outcome: PartyOutcome) -> PartyResult {
    match outcome {
        PartyOutcome::Ran => PartyResult::Success,
        PartyOutcome::Refused(refusal) => party_result_for(refusal),
    }
}

fn party_result_for(refusal: GroupRefusal) -> PartyResult {
    match refusal {
        GroupRefusal::AlreadyInGroup => PartyResult::AlreadyInGroup,
        GroupRefusal::GroupFull => PartyResult::GroupFull,
        GroupRefusal::NotLeader => PartyResult::NotLeader,
        GroupRefusal::NotInGroup => PartyResult::NotInGroup,
        GroupRefusal::TargetNotInGroup => PartyResult::TargetNotInGroup,
        GroupRefusal::WrongFaction => PartyResult::PlayerWrongFaction,
        GroupRefusal::ActorUnavailable
        | GroupRefusal::InviteSelf
        | GroupRefusal::NoSuchPlayer
        | GroupRefusal::TargetOffline
        | GroupRefusal::NoPendingInvite
        | GroupRefusal::InviterUnavailable
        | GroupRefusal::KickSelf
        | GroupRefusal::InvalidLootRules
        | GroupRefusal::IntentAlreadyClaimed
        | GroupRefusal::ActionSuppressed
        | GroupRefusal::NotRaid
        | GroupRefusal::TargetIsSelf
        | GroupRefusal::InvalidSubgroup
        | GroupRefusal::SubgroupFull
        | GroupRefusal::InvalidTargetIcon => PartyResult::BadPlayerName,
    }
}

fn friend_result_for(refusal: ContactRefusal, is_ignore: bool) -> FriendResult {
    match (refusal, is_ignore) {
        (ContactRefusal::AddSelf, false) => FriendResult::SelfX,
        (ContactRefusal::AddSelf, true) => FriendResult::IgnoreSelf,
        (ContactRefusal::AlreadyOnList, false) => FriendResult::Already,
        (ContactRefusal::AlreadyOnList, true) => FriendResult::IgnoreAlready,
        (ContactRefusal::ListFull, false) => FriendResult::ListFull,
        (ContactRefusal::ListFull, true) => FriendResult::IgnoreFull,
        (ContactRefusal::NotOnList, true) => FriendResult::IgnoreNotFound,
        (ContactRefusal::NotOnList, false) | (ContactRefusal::ActorUnavailable, _) => {
            FriendResult::NotFound
        }
        (ContactRefusal::Enemy, _) => FriendResult::Enemy,
    }
}

pub(crate) fn friend_online_fields(presence: &presence::RealmPresence) -> codec::FriendOnline {
    let away = match &presence.whereabouts {
        presence::Whereabouts::InWorld { away, .. } => *away,
        presence::Whereabouts::InTransit | presence::Whereabouts::Offline => {
            presence::AwayStatus::None
        }
    };
    codec::FriendOnline {
        away,
        zone_id: presence.zone_id,
        level: presence.level,
        class: presence.class,
    }
}

pub(crate) fn friend_views<St: ShardRoutingStore + SocialStore + ?Sized>(
    store: &St,
    own_team: u32,
    friend_guids: &[u64],
) -> Vec<codec::FriendView> {
    friend_guids
        .iter()
        .map(|&guid| {
            let presence = presence::of(store, guid).unwrap_or(None);
            let online = presence.as_ref().is_some_and(|p| {
                p.session_online && lyracore_shared::faction::team_for_race(p.race) == own_team
            });
            let (away, level, class, zone_id) = match &presence {
                Some(p) if online => {
                    let away = match &p.whereabouts {
                        presence::Whereabouts::InWorld { away, .. } => *away,
                        presence::Whereabouts::InTransit | presence::Whereabouts::Offline => {
                            presence::AwayStatus::None
                        }
                    };
                    (away, p.level, p.class, p.zone_id)
                }
                _ => (presence::AwayStatus::None, 0, 0, 0),
            };
            codec::FriendView {
                guid,
                online,
                away,
                level,
                class,
                zone_id,
            }
        })
        .collect()
}

fn resolve_add_contact<St: CharacterStore + ShardRoutingStore + SocialStore + ?Sized>(
    store: &St,
    actor: Actor,
    name: &str,
    is_ignore: bool,
) -> Result<(FriendResult, u64, Option<codec::FriendOnline>)> {
    let Some(target_guid) = presence::resolve_by_name(store, name)? else {
        let not_found = if is_ignore {
            FriendResult::IgnoreNotFound
        } else {
            FriendResult::NotFound
        };
        return Ok((not_found, 0, None));
    };
    if is_ignore {
        let outcome = store.add_ignore(actor, target_guid)?;
        let result = match outcome {
            ContactOutcome::Done => FriendResult::IgnoreAdded,
            ContactOutcome::Refused(refusal) => friend_result_for(refusal, true),
        };
        return Ok((result, target_guid, None));
    }

    let target = presence::of(store, target_guid).unwrap_or_else(|error| {
        log::debug!("resolve_add_contact: Realm Presence read failed for {target_guid}: {error:#}");
        None
    });
    let target_race = match &target {
        Some(presence) => Some(presence.race),
        None => presence::character_identity_anywhere(store, target_guid)
            .unwrap_or(None)
            .map(|identity| identity.race),
    };
    let Some(target_race) = target_race else {
        return Ok((FriendResult::NotFound, 0, None));
    };
    let outcome = store.add_friend(actor, target_guid, target_race)?;
    let (result, online) = match outcome {
        ContactOutcome::Done => {
            let actor_race = store
                .character_by_guid(actor.guid())?
                .map_or(0, |character| character.race);
            match target.as_ref().filter(|presence| {
                presence.session_online
                    && lyracore_shared::faction::same_team(actor_race, presence.race)
            }) {
                Some(presence) => (
                    FriendResult::AddedOnline,
                    Some(friend_online_fields(presence)),
                ),
                None => (FriendResult::AddedOffline, None),
            }
        }
        ContactOutcome::Refused(refusal) => (friend_result_for(refusal, false), None),
    };
    Ok((result, target_guid, online))
}

fn resolve_del_contact<St: SocialStore + ?Sized>(
    store: &St,
    actor: Actor,
    target_guid: u64,
    is_ignore: bool,
) -> Result<(FriendResult, u64)> {
    let outcome = if is_ignore {
        store.del_ignore(actor, target_guid)?
    } else {
        store.del_friend(actor, target_guid)?
    };
    let result = match outcome {
        ContactOutcome::Done if is_ignore => FriendResult::IgnoreRemoved,
        ContactOutcome::Done => FriendResult::Removed,
        ContactOutcome::Refused(refusal) => friend_result_for(refusal, is_ignore),
    };
    Ok((result, target_guid))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_group_refusal_renders_its_own_party_result() {
        for refusal in GroupRefusal::ALL {
            let expected = match refusal {
                GroupRefusal::AlreadyInGroup => PartyResult::AlreadyInGroup,
                GroupRefusal::GroupFull => PartyResult::GroupFull,
                GroupRefusal::NotLeader => PartyResult::NotLeader,
                GroupRefusal::NotInGroup => PartyResult::NotInGroup,
                GroupRefusal::TargetNotInGroup => PartyResult::TargetNotInGroup,
                GroupRefusal::WrongFaction => PartyResult::PlayerWrongFaction,
                _ => PartyResult::BadPlayerName,
            };
            assert_eq!(party_result_for(refusal), expected, "{refusal:?}");
        }
        assert_eq!(party_result(PartyOutcome::Ran), PartyResult::Success);
    }

    #[test]
    fn each_contact_refusal_renders_the_code_of_the_list_it_names() {
        for refusal in ContactRefusal::ALL {
            let (friend, ignore) = match refusal {
                ContactRefusal::AddSelf => (FriendResult::SelfX, FriendResult::IgnoreSelf),
                ContactRefusal::AlreadyOnList => {
                    (FriendResult::Already, FriendResult::IgnoreAlready)
                }
                ContactRefusal::ListFull => (FriendResult::ListFull, FriendResult::IgnoreFull),
                ContactRefusal::NotOnList => (FriendResult::NotFound, FriendResult::IgnoreNotFound),
                ContactRefusal::ActorUnavailable => {
                    (FriendResult::NotFound, FriendResult::NotFound)
                }
                ContactRefusal::Enemy => (FriendResult::Enemy, FriendResult::Enemy),
            };
            assert_eq!(friend_result_for(refusal, false), friend, "{refusal:?}");
            assert_eq!(friend_result_for(refusal, true), ignore, "{refusal:?}");
        }
    }
}
