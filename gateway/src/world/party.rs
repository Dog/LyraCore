use crate::world::{CharacterStore, SessionStore, ShardRoutingStore, SocialStore};
use std::time::{Duration, Instant};

use anyhow::Result;

use super::{presence, send, Actor, Outbound, SessionTx, WorldStore};
use crate::codec;
use lyracore_shared::group::{
    bot_op, realm_op, GroupKind, GroupRefusal, RaidSlot, RosterMember, RosterPayload,
    COMMAND_RESULT_WINDOW_MICROS,
};

pub(crate) trait PartyStore: Send + Sync {
    fn claim_bot_invite_intent(&self, intent_id: u64) -> Result<PartyOutcome>;

    fn claim_party_command_intent(&self, intent_id: u64, claim_token: u64) -> Result<()>;

    fn admit_party_command_authority(
        &self,
        group_id: u64,
        leader_guid: u64,
        bot_guid: u64,
        authority_member_guid: u64,
        expected_members: Vec<u64>,
    ) -> Result<CompanionCommandOutcome>;

    fn apply_admitted_party_command(
        &self,
        command: &AdmittedCompanionCommand,
    ) -> Result<CompanionCommandOutcome>;

    fn finish_party_command_intent(
        &self,
        intent_id: u64,
        claim_token: u64,
        outcome: CompanionCommandOutcome,
    ) -> Result<()>;

    fn confirm_party_command_receipt(
        &self,
        source_identity: spacetimedb_sdk::Identity,
        intent_id: u64,
    ) -> Result<Option<CompanionCommandOutcome>>;

    fn confirm_party_command_holder(&self, bot_guid: u64) -> Result<PartyCommandHolder>;

    fn entity_partition(&self, guid: u64) -> Option<(u32, u64)>;

    fn admit_sessionless_group_action(&self, character_guid: u64) -> Result<PartyOutcome>;

    fn realm_group_op(
        &self,
        op: u8,
        actor: Actor,
        target_guid: u64,
        arg_a: u8,
        arg_b: u8,
        arg_c: u64,
    ) -> Result<PartyOutcome>;

    /// Wait off-pump for the committed roster; this call depends on the Coordinator pump.
    fn realm_group_op_visible(
        &self,
        op: u8,
        actor: Actor,
        target_guid: u64,
        arg_a: u8,
        arg_b: u8,
        arg_c: u64,
    ) -> Result<PartyOutcome>;

    fn deleted_character_party_leave(&self, character: Actor) -> Result<PartyOutcome>;

    fn group_roster(&self, character_guid: u64) -> Result<Option<GroupRoster>>;

    fn party_command_group_roster(&self, character_guid: u64) -> Result<Option<GroupRoster>>;

    fn group_roster_by_id(&self, group_id: u64) -> Result<Option<GroupRoster>>;

    fn group_roster_revision(&self, group_id: u64) -> Result<u64>;

    fn held_roster_revision(&self, group_id: u64) -> Result<Option<u64>>;

    fn realm_character_partition(
        &self,
        character_guid: u64,
    ) -> Result<Option<RealmCharacterPartition>>;

    fn party_holder_observation(
        &self,
        character_guid: u64,
        map_id: u32,
        instance_id: u64,
    ) -> Result<PartyHolderObservation>;

    fn party_cleanup_group_roster_by_id(&self, group_id: u64) -> Result<Option<GroupRoster>>;

    fn party_member_guids(&self) -> Result<Vec<u64>>;

    fn party_group_ids(&self) -> Result<Vec<u64>>;

    fn sync_group_mirror(&self, roster: &GroupRoster) -> Result<()>;

    fn group_invite(&self, actor: Actor, target_guid: u64) -> Result<PartyOutcome>;

    fn group_accept(&self, actor: Actor) -> Result<PartyOutcome>;

    fn group_decline(&self, actor: Actor) -> Result<PartyOutcome>;

    fn group_leave(&self, actor: Actor) -> Result<PartyOutcome>;

    fn group_uninvite(&self, actor: Actor, target_guid: u64) -> Result<PartyOutcome>;

    fn group_loot_method(
        &self,
        actor: Actor,
        loot_setting: u8,
        master_guid: u64,
        loot_threshold: u8,
    ) -> Result<PartyOutcome>;
}

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct GroupRoster {
    pub group_id: u64,
    pub roster_revision: u64,
    pub leader_guid: u64,
    pub loot_method: u8,
    pub loot_threshold: u8,
    pub master_looter_guid: u64,
    pub kind: GroupKind,
    pub members: Vec<GroupRosterMember>,
    pub partitions: Vec<GroupMemberPartition>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GroupRosterMember {
    pub guid: u64,
    pub slot: RaidSlot,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GroupMemberPartition {
    pub character_guid: u64,
    pub group_id: u64,
    pub membership_revision: u64,
    pub member_active: bool,
    pub map_id: u32,
    pub instance_id: u64,
    pub locator_revision: u64,
    pub state: PartyPartitionState,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PartyPartitionState {
    Unknown,
    Known,
    PendingTransfer,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RealmCharacterPartition {
    pub map_id: u32,
    pub instance_id: u64,
    pub revision: u64,
    pub transfer_pending: bool,
    pub pending_destination_map: u32,
    pub pending_destination_instance: u64,
    pub bot_source_identity: spacetimedb_sdk::Identity,
    pub bot_transfer_intent_id: u64,
    pub bot_controller_generation: u64,
}

const PARTY_LOCATION_SHARD_LIMIT: usize = 16;
pub(crate) const PARTY_COMMAND_ABORT_STEP: &str = "apply_party_command";

pub(crate) fn party_command_abort_configuration(
    configured: Option<String>,
) -> Result<Option<String>> {
    match configured {
        None => Ok(None),
        Some(step) if step == PARTY_COMMAND_ABORT_STEP => Ok(Some(step)),
        Some(step) => anyhow::bail!(
            "LYRACORE_PARTY_COMMAND_ABORT_AFTER={step} names no party command step; valid step: \
             {PARTY_COMMAND_ABORT_STEP}"
        ),
    }
}

#[cfg(not(test))]
fn die_by_party_command_injection() -> ! {
    log::logger().flush();
    std::process::abort()
}

#[cfg(test)]
fn die_by_party_command_injection() -> ! {
    panic!("LYRACORE_PARTY_COMMAND_ABORT_AFTER: injected abort");
}

#[inline]
fn party_command_abort_point(
    abort_after: Option<&str>,
    source_identity: spacetimedb_sdk::Identity,
    intent_id: u64,
) {
    if abort_after != Some(PARTY_COMMAND_ABORT_STEP) {
        return;
    }
    log::error!(
        "party command {source_identity}/{intent_id}: \
         LYRACORE_PARTY_COMMAND_ABORT_AFTER={PARTY_COMMAND_ABORT_STEP}; target apply committed, \
         aborting before source finalization for the Gateway recovery test"
    );
    die_by_party_command_injection()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct PartyHolderObservation {
    pub(crate) serves_locator: bool,
    pub(crate) has_escrow: bool,
    pub(crate) character_partition: Option<(u32, u64)>,
}

fn classify_party_partition(
    locator: RealmCharacterPartition,
    observations: &[PartyHolderObservation],
) -> Result<PartyPartitionState> {
    let mut state = None;
    for observation in observations.iter().filter(|row| row.serves_locator) {
        let observed = if observation.has_escrow {
            Some(PartyPartitionState::PendingTransfer)
        } else if observation.character_partition == Some((locator.map_id, locator.instance_id)) {
            Some(PartyPartitionState::Known)
        } else {
            None
        };
        if let Some(observed) = observed {
            if state.replace(observed).is_some() {
                anyhow::bail!(
                    "more than one World Shard holds the Character at Realm locator revision {}",
                    locator.revision
                );
            }
        }
    }
    Ok(state.unwrap_or(PartyPartitionState::Unknown))
}

fn certify_roster_partitions<St: ShardRoutingStore + ?Sized>(
    store: &St,
    realm: &dyn WorldStore,
    mut roster: GroupRoster,
) -> Result<GroupRoster> {
    let shards = store.world_stores();
    if shards.len() > PARTY_LOCATION_SHARD_LIMIT {
        anyhow::bail!(
            "party partition certification exceeds the {PARTY_LOCATION_SHARD_LIMIT}-Shard limit"
        );
    }
    for projection in &mut roster.partitions {
        let Some(locator) = realm.realm_character_partition(projection.character_guid)? else {
            projection.map_id = 0;
            projection.instance_id = 0;
            projection.locator_revision = 0;
            projection.state = PartyPartitionState::Unknown;
            continue;
        };
        if locator.transfer_pending {
            projection.map_id = locator.map_id;
            projection.instance_id = locator.instance_id;
            projection.locator_revision = locator.revision;
            projection.state = PartyPartitionState::PendingTransfer;
            continue;
        }
        let observations: Vec<_> = shards
            .iter()
            .map(|shard| {
                shard.party_holder_observation(
                    projection.character_guid,
                    locator.map_id,
                    locator.instance_id,
                )
            })
            .collect::<Result<_>>()?;
        if realm.realm_character_partition(projection.character_guid)? != Some(locator) {
            anyhow::bail!(
                "party member {} changed Realm locator during certification",
                projection.character_guid
            );
        }
        projection.map_id = locator.map_id;
        projection.instance_id = locator.instance_id;
        projection.locator_revision = locator.revision;
        projection.state = classify_party_partition(locator, &observations)?;
    }
    Ok(roster)
}

fn append_departed_partitions(roster: &mut GroupRoster, previous: Option<&GroupRoster>) {
    let Some(previous) = previous.filter(|row| row.group_id == roster.group_id) else {
        return;
    };
    let departed: Vec<_> = previous
        .partitions
        .iter()
        .copied()
        .filter(|row| !roster.has_member(row.character_guid))
        .collect();
    for mut departed in departed {
        departed.member_active = false;
        departed.state = PartyPartitionState::Unknown;
        roster.partitions.push(departed);
    }
}

impl GroupRoster {
    pub fn disbanded(group_id: u64, roster_revision: u64) -> Self {
        Self {
            group_id,
            roster_revision,
            ..Default::default()
        }
    }

    pub fn member_guids(&self) -> Vec<u64> {
        self.members.iter().map(|member| member.guid).collect()
    }

    pub fn has_member(&self, guid: u64) -> bool {
        self.members.iter().any(|member| member.guid == guid)
    }

    pub fn list_payload(&self) -> RosterPayload {
        RosterPayload {
            leader: self.leader_guid,
            loot_method: self.loot_method,
            loot_threshold: self.loot_threshold,
            master_looter_guid: self.master_looter_guid,
            kind: self.kind,
            members: self
                .members
                .iter()
                .map(|member| RosterMember {
                    guid: member.guid,
                    name: String::new(),
                    slot: member.slot,
                })
                .collect(),
            target_icons: Vec::new(),
        }
    }
}

fn roster_or_disbanded(realm: &dyn WorldStore, group_id: u64) -> Result<GroupRoster> {
    Ok(match realm.group_roster_by_id(group_id)? {
        Some(roster) => roster,
        None => GroupRoster::disbanded(group_id, realm.group_roster_revision(group_id)?),
    })
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Op {
    Invite(u64),
    Accept,
    Decline,
    Leave,
    Uninvite(u64),
    LootMethod {
        setting: u8,
        master: u64,
        threshold: u8,
    },
    RaidConvert,
    SetLeader(u64),
    SetAssistant {
        target: u64,
        promote: bool,
    },
    ChangeSubgroup {
        target: u64,
        subgroup: u8,
    },
    SwapSubgroup {
        first: u64,
        second: u64,
    },
    ReadyCheckStart,
    ReadyCheckAnswer(u8),
    TargetIcon {
        icon: u8,
        target: u64,
    },
    MinimapPing {
        x: f32,
        y: f32,
    },
    RandomRoll {
        min: u32,
        max: u32,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ThrottledBroadcast {
    ReadyCheck,
    MinimapPing,
    RandomRoll,
}

impl ThrottledBroadcast {
    const COUNT: usize = 3;

    fn index(self) -> usize {
        match self {
            Self::ReadyCheck => 0,
            Self::MinimapPing => 1,
            Self::RandomRoll => 2,
        }
    }
}

pub(crate) const GROUP_BROADCAST_THROTTLE: Duration = Duration::from_secs(1);

#[derive(Debug, Default)]
pub(crate) struct GroupBroadcastCooldowns([Option<Instant>; ThrottledBroadcast::COUNT]);

impl GroupBroadcastCooldowns {
    pub(crate) fn admit_at(&mut self, op: Op, now: Instant) -> bool {
        let Some(kind) = op.broadcast_throttle() else {
            return true;
        };
        let until = &mut self.0[kind.index()];
        if until.is_some_and(|until| now < until) {
            return false;
        }
        *until = Some(now + GROUP_BROADCAST_THROTTLE);
        true
    }
}

type RealmOpArgs = (u8, u64, u8, u8, u64);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct AcceptorFacts {
    class: u8,
}

impl AcceptorFacts {
    const NONE: Self = Self { class: 0 };

    fn of<St: CharacterStore + ShardRoutingStore + ?Sized>(store: &St, guid: u64) -> Self {
        match presence::character_anywhere(store, guid) {
            Ok(Some(character)) => Self {
                class: character.class,
            },
            Ok(None) => Self::NONE,
            Err(error) => {
                log::warn!("party: class of acceptor {guid} unread: {error:#}");
                Self::NONE
            }
        }
    }

    fn for_op<St: CharacterStore + ShardRoutingStore + ?Sized>(
        store: &St,
        guid: u64,
        op: Op,
    ) -> Self {
        match op {
            Op::Accept => Self::of(store, guid),
            _ => Self::NONE,
        }
    }
}

impl Op {
    fn realm_args(self, acceptor: AcceptorFacts) -> RealmOpArgs {
        match self {
            Op::Invite(target) => (realm_op::INVITE, target, 0, 0, 0),
            Op::Accept => (realm_op::ACCEPT, 0, acceptor.class, 0, 0),
            Op::Decline => (realm_op::DECLINE, 0, 0, 0, 0),
            Op::Leave => (realm_op::LEAVE, 0, 0, 0, 0),
            Op::Uninvite(target) => (realm_op::UNINVITE, target, 0, 0, 0),
            Op::LootMethod {
                setting,
                master,
                threshold,
            } => (realm_op::LOOT_METHOD, master, setting, threshold, 0),
            Op::RaidConvert => (realm_op::RAID_CONVERT, 0, 0, 0, 0),
            Op::SetLeader(target) => (realm_op::SET_LEADER, target, 0, 0, 0),
            Op::SetAssistant { target, promote } => {
                (realm_op::SET_ASSISTANT, target, u8::from(promote), 0, 0)
            }
            Op::ChangeSubgroup { target, subgroup } => {
                (realm_op::CHANGE_SUBGROUP, target, subgroup, 0, 0)
            }
            Op::SwapSubgroup { first, second } => (realm_op::SWAP_SUBGROUP, first, 0, 0, second),
            Op::ReadyCheckStart => (realm_op::READY_CHECK_START, 0, 0, 0, 0),
            Op::ReadyCheckAnswer(state) => (realm_op::READY_CHECK_ANSWER, 0, state, 0, 0),
            Op::TargetIcon { icon, target } => (realm_op::TARGET_ICON, target, icon, 0, 0),
            Op::MinimapPing { x, y } => (
                realm_op::MINIMAP_PING,
                u64::from(x.to_bits()),
                0,
                0,
                u64::from(y.to_bits()),
            ),
            Op::RandomRoll { min, max } => {
                (realm_op::RANDOM_ROLL, u64::from(min), 0, 0, u64::from(max))
            }
        }
    }

    pub(crate) fn broadcast_throttle(self) -> Option<ThrottledBroadcast> {
        match self {
            Op::ReadyCheckStart => Some(ThrottledBroadcast::ReadyCheck),
            Op::MinimapPing { .. } => Some(ThrottledBroadcast::MinimapPing),
            Op::RandomRoll { .. } => Some(ThrottledBroadcast::RandomRoll),
            _ => None,
        }
    }

    fn is_group_broadcast(self) -> bool {
        matches!(
            self,
            Op::ReadyCheckStart
                | Op::ReadyCheckAnswer(_)
                | Op::TargetIcon { .. }
                | Op::MinimapPing { .. }
                | Op::RandomRoll { .. }
        )
    }
}

fn run_on_authority<A: PartyStore + ?Sized>(
    authority: &A,
    actor: Actor,
    op: Op,
) -> Result<PartyOutcome> {
    let (code, target, arg_a, arg_b, arg_c) = op.realm_args(AcceptorFacts::NONE);
    authority.realm_group_op(code, actor, target, arg_a, arg_b, arg_c)
}

fn run_on_authority_visible<A: PartyStore + ?Sized>(
    authority: &A,
    actor: Actor,
    op: Op,
    acceptor: AcceptorFacts,
) -> Result<PartyOutcome> {
    let (code, target, arg_a, arg_b, arg_c) = op.realm_args(acceptor);
    authority.realm_group_op_visible(code, actor, target, arg_a, arg_b, arg_c)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PartyOutcome {
    Ran,
    Refused(GroupRefusal),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompanionCommandOutcome {
    Applied,
    Unchanged,
    Malformed,
    NotLeader,
    NotMember,
    StalePartyMirror,
    WrongAccount,
    MissingBot,
    WrongPartition,
    Suppressed,
    TargetDead,
    TargetUnavailable,
    TargetControlled,
    Expired,
    WaitingForCapacity,
    OutcomeUnknown,
    Superseded,
}

#[derive(Clone, Debug)]
pub struct PartyCommandIntent {
    pub id: u64,
    pub source_identity: spacetimedb_sdk::Identity,
    pub issuer_guid: u64,
    pub issuer_sequence: u64,
    pub kind: u8,
    pub bot_guid: u64,
    pub authority_member_guid: u64,
    pub exact_target_guid: u64,
    pub expires_micros: i64,
}

#[derive(Clone, Debug)]
pub struct AdmittedCompanionCommand {
    pub source_identity: spacetimedb_sdk::Identity,
    pub intent_id: u64,
    pub issuer_guid: u64,
    pub issuer_sequence: u64,
    pub group_id: u64,
    pub leader_guid: u64,
    pub members: Vec<u64>,
    pub kind: u8,
    pub bot_guid: u64,
    pub authority_member_guid: u64,
    pub exact_target_guid: u64,
    pub expires_micros: i64,
    pub receipt_retain_until_micros: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PartyCommandHolder {
    Present,
    Missing,
    InTransit,
}

fn receipt_anywhere<St: PartyStore + ShardRoutingStore + ?Sized>(
    source: &St,
    source_identity: spacetimedb_sdk::Identity,
    intent_id: u64,
) -> Result<Option<CompanionCommandOutcome>> {
    let mut found = source.confirm_party_command_receipt(source_identity, intent_id)?;
    for shard in source.party_command_worlds()? {
        if let Some(outcome) = shard.confirm_party_command_receipt(source_identity, intent_id)? {
            if found.is_some_and(|previous| previous != outcome) {
                anyhow::bail!(
                    "party command receipt {source_identity}/{intent_id} has conflicting outcomes"
                );
            }
            found = Some(outcome);
        }
    }
    Ok(found)
}

pub(crate) fn finish_expired_party_command_intent<St: PartyStore + ShardRoutingStore + ?Sized>(
    source: &St,
    intent: &PartyCommandIntent,
    claim_token: u64,
) -> Result<CompanionCommandOutcome> {
    let outcome =
        receipt_anywhere(source, intent.source_identity, intent.id)?.unwrap_or_else(|| {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_micros() as i64;
            if now
                >= intent
                    .expires_micros
                    .saturating_add(COMMAND_RESULT_WINDOW_MICROS)
            {
                CompanionCommandOutcome::OutcomeUnknown
            } else {
                CompanionCommandOutcome::Expired
            }
        });
    source.finish_party_command_intent(intent.id, claim_token, outcome)?;
    Ok(outcome)
}

fn same_authority(left: &GroupRoster, right: &GroupRoster) -> bool {
    let mut left_members = left.member_guids();
    let mut right_members = right.member_guids();
    left_members.sort_unstable();
    right_members.sort_unstable();
    left.group_id == right.group_id
        && left.leader_guid == right.leader_guid
        && left_members == right_members
}

pub(crate) fn run_party_command_intent<St: WorldStore>(
    source: &St,
    intent: &PartyCommandIntent,
    claim_token: u64,
) -> Result<CompanionCommandOutcome> {
    let abort_after = party_command_abort_configuration(
        std::env::var("LYRACORE_PARTY_COMMAND_ABORT_AFTER").ok(),
    )?;
    source.claim_party_command_intent(intent.id, claim_token)?;

    if let Some(outcome) = receipt_anywhere(source, intent.source_identity, intent.id)? {
        source.finish_party_command_intent(intent.id, claim_token, outcome)?;
        return Ok(outcome);
    }

    let world_shards = source.party_command_worlds()?;
    let target: &dyn WorldStore;
    let owned_target;
    if world_shards.is_empty() {
        match source.confirm_party_command_holder(intent.bot_guid)? {
            PartyCommandHolder::Present => {}
            PartyCommandHolder::InTransit => anyhow::bail!("party command bot is in Transfer"),
            PartyCommandHolder::Missing => {
                let outcome = CompanionCommandOutcome::MissingBot;
                source.finish_party_command_intent(intent.id, claim_token, outcome)?;
                return Ok(outcome);
            }
        }
        target = source;
    } else {
        let mut holders = Vec::new();
        let mut in_transit = false;
        for shard in &world_shards {
            match shard.confirm_party_command_holder(intent.bot_guid)? {
                PartyCommandHolder::Present => holders.push(shard.clone()),
                PartyCommandHolder::InTransit => in_transit = true,
                PartyCommandHolder::Missing => {}
            }
        }
        if in_transit {
            anyhow::bail!("party command bot is in Transfer");
        }
        let Some(holder) = holders.pop() else {
            let outcome = CompanionCommandOutcome::MissingBot;
            source.finish_party_command_intent(intent.id, claim_token, outcome)?;
            return Ok(outcome);
        };
        if !holders.is_empty() {
            anyhow::bail!(
                "bot {} has more than one live World Shard holder",
                intent.bot_guid
            );
        }
        owned_target = holder;
        target = owned_target.as_ref();
    }

    let owned_realm;
    let realm: &dyn WorldStore = match source.realm_store()? {
        Some(handle) => {
            owned_realm = handle;
            owned_realm.as_ref()
        }
        None => source,
    };
    let Some(authority) = realm.party_command_group_roster(intent.issuer_guid)? else {
        let outcome = CompanionCommandOutcome::NotMember;
        source.finish_party_command_intent(intent.id, claim_token, outcome)?;
        return Ok(outcome);
    };
    let outcome = if authority.leader_guid != intent.issuer_guid {
        Some(CompanionCommandOutcome::NotLeader)
    } else if !authority.has_member(intent.bot_guid)
        || (intent.authority_member_guid != 0
            && !authority.has_member(intent.authority_member_guid))
    {
        Some(CompanionCommandOutcome::NotMember)
    } else {
        None
    };
    if let Some(outcome) = outcome {
        source.finish_party_command_intent(intent.id, claim_token, outcome)?;
        return Ok(outcome);
    }
    let authority_outcome = realm.admit_party_command_authority(
        authority.group_id,
        intent.issuer_guid,
        intent.bot_guid,
        intent.authority_member_guid,
        authority.member_guids(),
    )?;
    if authority_outcome != CompanionCommandOutcome::Applied {
        let outcome = authority_outcome;
        source.finish_party_command_intent(intent.id, claim_token, outcome)?;
        return Ok(outcome);
    }
    let local = target.party_command_group_roster(intent.bot_guid)?;
    if local
        .as_ref()
        .is_none_or(|local| !same_authority(local, &authority))
    {
        let outcome = CompanionCommandOutcome::StalePartyMirror;
        source.finish_party_command_intent(intent.id, claim_token, outcome)?;
        return Ok(outcome);
    }
    let bot_partition = target.entity_partition(intent.bot_guid);
    let issuer_partition = source.entity_partition(intent.issuer_guid).or_else(|| {
        world_shards
            .iter()
            .find_map(|shard| shard.entity_partition(intent.issuer_guid))
    });
    let member_partition = (intent.authority_member_guid != 0)
        .then(|| {
            source
                .entity_partition(intent.authority_member_guid)
                .or_else(|| {
                    world_shards
                        .iter()
                        .find_map(|shard| shard.entity_partition(intent.authority_member_guid))
                })
        })
        .flatten();
    let member_partition_required = intent.authority_member_guid != 0;
    if bot_partition.is_none()
        || issuer_partition != bot_partition
        || (member_partition_required && member_partition != bot_partition)
    {
        let outcome = CompanionCommandOutcome::WrongPartition;
        source.finish_party_command_intent(intent.id, claim_token, outcome)?;
        return Ok(outcome);
    }
    let admitted = AdmittedCompanionCommand {
        source_identity: intent.source_identity,
        intent_id: intent.id,
        issuer_guid: intent.issuer_guid,
        issuer_sequence: intent.issuer_sequence,
        group_id: authority.group_id,
        leader_guid: authority.leader_guid,
        members: authority.member_guids(),
        kind: intent.kind,
        bot_guid: intent.bot_guid,
        authority_member_guid: intent.authority_member_guid,
        exact_target_guid: intent.exact_target_guid,
        expires_micros: intent.expires_micros,
        receipt_retain_until_micros: intent
            .expires_micros
            .saturating_add(COMMAND_RESULT_WINDOW_MICROS),
    };
    let outcome = target.apply_admitted_party_command(&admitted)?;
    if outcome != CompanionCommandOutcome::WaitingForCapacity {
        party_command_abort_point(abort_after.as_deref(), intent.source_identity, intent.id);
        source.finish_party_command_intent(intent.id, claim_token, outcome)?;
    }
    Ok(outcome)
}

impl From<GroupRefusal> for PartyOutcome {
    fn from(refusal: GroupRefusal) -> Self {
        Self::Refused(refusal)
    }
}

fn own_group_roster<St: PartyStore + ShardRoutingStore + ?Sized>(
    store: &St,
    self_guid: u64,
) -> Result<Option<GroupRoster>> {
    match store.realm_store()? {
        Some(realm) => realm.group_roster(self_guid),
        None => store.group_roster(self_guid),
    }
}

fn resolve_in_roster<St: CharacterStore + ShardRoutingStore + ?Sized>(
    store: &St,
    roster: &GroupRoster,
    name: &str,
) -> Result<Option<u64>> {
    for member in &roster.members {
        if presence::character_anywhere(store, member.guid)?
            .is_some_and(|character| character.name.eq_ignore_ascii_case(name))
        {
            return Ok(Some(member.guid));
        }
    }
    Ok(None)
}

pub(crate) fn resolve_roster_member_by_name<
    St: CharacterStore + PartyStore + ShardRoutingStore + ?Sized,
>(
    store: &St,
    self_guid: u64,
    name: &str,
) -> Result<Option<u64>> {
    let Some(roster) = own_group_roster(store, self_guid)? else {
        return Ok(None);
    };
    resolve_in_roster(store, &roster, name)
}

pub(crate) fn resolve_roster_members_by_name<
    St: CharacterStore + PartyStore + ShardRoutingStore + ?Sized,
>(
    store: &St,
    self_guid: u64,
    first_name: &str,
    second_name: &str,
) -> Result<(Option<u64>, Option<u64>)> {
    let Some(roster) = own_group_roster(store, self_guid)? else {
        return Ok((None, None));
    };
    Ok((
        resolve_in_roster(store, &roster, first_name)?,
        resolve_in_roster(store, &roster, second_name)?,
    ))
}

fn admit_sessionless_answer<St: PartyStore + SessionStore + ShardRoutingStore + ?Sized>(
    store: &St,
    guid: u64,
) -> Result<PartyOutcome> {
    if store.entity_in_world(guid) {
        return store.admit_sessionless_group_action(guid);
    }
    if let Some(shard) = store
        .world_stores()
        .into_iter()
        .find(|shard| shard.entity_in_world(guid))
    {
        return shard.admit_sessionless_group_action(guid);
    }
    Ok(PartyOutcome::Refused(GroupRefusal::ActorUnavailable))
}

fn answer_for_session_less<
    St: CharacterStore + PartyStore + SessionStore + ShardRoutingStore + ?Sized,
>(
    store: &St,
    realm: &dyn WorldStore,
    guid: u64,
) {
    let Some(actor) = Actor::new(guid) else {
        return;
    };
    let admission = admit_sessionless_answer(store, guid);
    match admission {
        Ok(PartyOutcome::Ran) => {}
        Ok(PartyOutcome::Refused(_)) => return,
        Err(error) => {
            log::warn!("party: session-less {guid} admission unavailable: {error:#}");
            return;
        }
    }
    let acceptor = AcceptorFacts::of(store, guid);
    let joined = match run_on_authority_visible(realm, actor, Op::Accept, acceptor) {
        Ok(PartyOutcome::Ran) => {
            log::info!("party: session-less {guid} accepted its group invite");
            return;
        }
        Ok(PartyOutcome::Refused(refusal)) => format!("{refusal:?}"),
        Err(e) => format!("{e:#}"),
    };
    log::info!("party: session-less {guid} cannot join ({joined}), declining explicitly");
    match run_on_authority(realm, actor, Op::Decline) {
        Ok(PartyOutcome::Ran) => {}
        outcome => log::warn!(
            "party: session-less {guid} could neither join nor decline ({outcome:?}). The \
             inviter's dialog stands until realm-core's invite GC reaps it"
        ),
    }
}

pub(crate) fn run<
    St: CharacterStore + PartyStore + SessionStore + ShardRoutingStore + SocialStore + ?Sized,
>(
    store: &St,
    actor: Actor,
    op: Op,
) -> Result<PartyOutcome> {
    let self_guid = actor.guid();
    if let Op::Invite(target) = op {
        if cross_faction_invite(store, self_guid, target)? {
            return Ok(GroupRefusal::WrongFaction.into());
        }
    }
    if let Op::SetLeader(target) = op {
        if !presence::live_anywhere(store, target) {
            return Ok(GroupRefusal::TargetOffline.into());
        }
    }
    let Some(realm) = store.realm_store()? else {
        return match op {
            Op::Invite(target) => store.group_invite(actor, target),
            Op::Accept => store.group_accept(actor),
            Op::Decline => store.group_decline(actor),
            Op::Leave => store.group_leave(actor),
            Op::Uninvite(target) => store.group_uninvite(actor, target),
            Op::LootMethod {
                setting,
                master,
                threshold,
            } => store.group_loot_method(actor, setting, master, threshold),
            Op::RaidConvert
            | Op::SetLeader(_)
            | Op::SetAssistant { .. }
            | Op::ChangeSubgroup { .. }
            | Op::SwapSubgroup { .. }
            | Op::ReadyCheckStart
            | Op::ReadyCheckAnswer(_)
            | Op::TargetIcon { .. }
            | Op::MinimapPing { .. }
            | Op::RandomRoll { .. } => run_on_authority(store, actor, op),
        };
    };
    if op.is_group_broadcast() {
        return run_on_authority(realm.as_ref(), actor, op);
    }
    if let Op::Invite(target) = op {
        if let Some(refusal) = invite_gate(store, target)? {
            return Ok(refusal.into());
        }
    }
    let before = realm.group_roster(self_guid)?;
    if matches!(op, Op::Leave | Op::Uninvite(_)) {
        crate::world::loot::flush_pending_promotions(store, realm.as_ref());
    }
    let acceptor = AcceptorFacts::for_op(store, self_guid, op);
    if let PartyOutcome::Refused(refusal) =
        run_on_authority_visible(realm.as_ref(), actor, op, acceptor)?
    {
        return Ok(PartyOutcome::Refused(refusal));
    }
    if let Op::Invite(target) = op {
        answer_for_session_less(store, realm.as_ref(), target);
    }
    if op_changed_nothing(op, before.as_ref()) {
        return Ok(PartyOutcome::Ran);
    }
    if matches!(op, Op::Accept | Op::Leave | Op::Uninvite(_)) {
        sync_membership_mirrors(store, realm.as_ref(), self_guid, before);
    } else {
        sync_mirrors(store, realm.as_ref(), self_guid, before);
    }
    Ok(PartyOutcome::Ran)
}

fn sync_membership_mirrors<St: ShardRoutingStore + ?Sized>(
    store: &St,
    realm: &dyn WorldStore,
    self_guid: u64,
    before: Option<GroupRoster>,
) {
    let mut touched: Vec<u64> = before.iter().map(|roster| roster.group_id).collect();
    match realm.group_roster(self_guid) {
        Ok(Some(now)) if !touched.contains(&now.group_id) => touched.push(now.group_id),
        Ok(_) => {}
        Err(error) => log::warn!(
            "party: could not read the realm-core roster for {self_guid} ({error:#}); only its \
             previous Group is mirrored"
        ),
    }
    for group_id in touched {
        if let Err(error) = sync_group_mirrors_required(
            store,
            realm,
            group_id,
            before.as_ref(),
            &store.world_stores(),
        ) {
            log::warn!(
                "party: group {group_id} mirrors stay stale until the next op or world entry \
                 ({error:#})"
            );
        }
    }
}

fn op_changed_nothing(op: Op, before: Option<&GroupRoster>) -> bool {
    let Some(before) = before else {
        return false;
    };
    let subgroup_of = |guid| {
        before
            .members
            .iter()
            .find(|member| member.guid == guid)
            .map(|member| member.slot.subgroup())
    };
    match op {
        Op::RaidConvert => before.kind == GroupKind::Raid,
        Op::SetAssistant { target, promote } => before
            .members
            .iter()
            .any(|member| member.guid == target && member.slot.is_assistant() == promote),
        Op::ChangeSubgroup { target, subgroup } => subgroup_of(target) == Some(subgroup),
        Op::SwapSubgroup { first, second } => {
            let (first, second) = (subgroup_of(first), subgroup_of(second));
            first.is_some() && first == second
        }
        _ => false,
    }
}

fn invite_gate<St: SessionStore + ShardRoutingStore + SocialStore + ?Sized>(
    store: &St,
    target: u64,
) -> Result<Option<GroupRefusal>> {
    if presence::of(store, target)?.is_none() {
        return Ok(Some(GroupRefusal::NoSuchPlayer));
    }
    if !presence::live_anywhere(store, target) {
        return Ok(Some(GroupRefusal::TargetOffline));
    }
    Ok(None)
}

fn cross_faction_invite<St: CharacterStore + SessionStore + ShardRoutingStore + ?Sized>(
    store: &St,
    inviter: u64,
    target: u64,
) -> Result<bool> {
    if !presence::live_anywhere(store, target) {
        return Ok(false);
    }
    let (Some(inviter), Some(target)) = (
        presence::character_anywhere(store, inviter)?,
        presence::character_anywhere(store, target)?,
    ) else {
        return Ok(false);
    };
    Ok(!lyracore_shared::faction::same_team(
        inviter.race,
        target.race,
    ))
}

pub(crate) fn run_bot_invite_intent<St: WorldStore>(
    store: &St,
    intent_id: u64,
    op: u8,
    inviter_guid: u64,
    target_guid: u64,
) -> Result<PartyOutcome> {
    match store.claim_bot_invite_intent(intent_id)? {
        PartyOutcome::Ran => {}
        PartyOutcome::Refused(GroupRefusal::IntentAlreadyClaimed) => return Ok(PartyOutcome::Ran),
        refusal @ PartyOutcome::Refused(_) => return Ok(refusal),
    }
    match op {
        bot_op::INVITE => run_bot_invite(store, inviter_guid, target_guid),
        bot_op::LEAVE => run_bot_leave(store, inviter_guid),
        unknown => anyhow::bail!("unknown bot group intent op {unknown}"),
    }
}

pub(crate) fn run_bot_invite<St: WorldStore>(
    store: &St,
    inviter_guid: u64,
    target_guid: u64,
) -> Result<PartyOutcome> {
    let Some(inviter) = Actor::new(inviter_guid) else {
        return Ok(GroupRefusal::ActorUnavailable.into());
    };
    let owned_realm;
    let realm: &dyn WorldStore = match store.realm_store()? {
        Some(r) => {
            owned_realm = r;
            owned_realm.as_ref()
        }
        None => store,
    };
    if let Some(refusal) = invite_gate(store, target_guid)? {
        return Ok(refusal.into());
    }
    let before = realm.group_roster(inviter_guid)?;
    if let PartyOutcome::Refused(refusal) =
        run_on_authority_visible(realm, inviter, Op::Invite(target_guid), AcceptorFacts::NONE)?
    {
        return Ok(PartyOutcome::Refused(refusal));
    }
    answer_for_session_less(store, realm, target_guid);
    sync_mirrors(store, realm, inviter_guid, before);
    Ok(PartyOutcome::Ran)
}

pub(crate) fn run_bot_leave<St: WorldStore>(store: &St, leaver_guid: u64) -> Result<PartyOutcome> {
    let Some(leaver) = Actor::new(leaver_guid) else {
        return Ok(GroupRefusal::ActorUnavailable.into());
    };
    let owned_realm;
    let realm: &dyn WorldStore = match store.realm_store()? {
        Some(r) => {
            owned_realm = r;
            owned_realm.as_ref()
        }
        None => store,
    };
    let leave = run_server_leave(store, realm, leaver, 1, |realm, leaver| {
        run_on_authority_visible(realm, leaver, Op::Leave, AcceptorFacts::NONE)
    })?;
    if leave.outcome == PartyOutcome::Ran {
        sync_membership_mirrors(store, realm, leaver_guid, leave.previous_roster);
    }
    Ok(leave.outcome)
}

struct ServerLeave {
    outcome: PartyOutcome,
    previous_roster: Option<GroupRoster>,
}

fn run_server_leave<St: ShardRoutingStore + ?Sized>(
    store: &St,
    realm: &dyn WorldStore,
    leaver: Actor,
    attempts: usize,
    leave_party: impl Fn(&dyn WorldStore, Actor) -> Result<PartyOutcome>,
) -> Result<ServerLeave> {
    let leaver_guid = leaver.guid();
    let before = realm.group_roster(leaver_guid)?;
    crate::world::loot::flush_pending_promotions(store, realm);
    let mut last_error = None;
    for _ in 0..attempts {
        match leave_party(realm, leaver) {
            Ok(PartyOutcome::Ran) => {
                return Ok(ServerLeave {
                    outcome: PartyOutcome::Ran,
                    previous_roster: before,
                });
            }
            Ok(PartyOutcome::Refused(GroupRefusal::NotInGroup))
                if before.is_some() && last_error.is_some() =>
            {
                return Ok(ServerLeave {
                    outcome: PartyOutcome::Ran,
                    previous_roster: before,
                });
            }
            Ok(PartyOutcome::Refused(refusal)) => {
                return Ok(ServerLeave {
                    outcome: PartyOutcome::Refused(refusal),
                    previous_roster: before,
                });
            }
            Err(error) => last_error = Some(error),
        }
    }

    let Some(error) = last_error else {
        return Ok(ServerLeave {
            outcome: PartyOutcome::Refused(GroupRefusal::NotInGroup),
            previous_roster: before,
        });
    };
    if before.is_some() {
        match realm.group_roster(leaver_guid) {
            Ok(None) => {
                return Ok(ServerLeave {
                    outcome: PartyOutcome::Ran,
                    previous_roster: before,
                });
            }
            Ok(Some(_)) => {}
            Err(confirmation_error) => {
                log::warn!(
                    "party: could not confirm Character {leaver_guid} membership after Realm-core \
                     LEAVE failed ({confirmation_error:#}); retrying"
                );
            }
        }
    }
    Err(error)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DeletedCharacterPartyCleanup {
    Preserved,
    Removed,
    AlreadyClean,
}

const DELETED_CHARACTER_LEAVE_ATTEMPTS: usize = 3;

pub(crate) fn cleanup_deleted_character<St: CharacterStore + ShardRoutingStore>(
    store: &St,
    character_guid: u64,
) -> Result<DeletedCharacterPartyCleanup> {
    let Some(realm) = store.realm_store()? else {
        return Ok(DeletedCharacterPartyCleanup::AlreadyClean);
    };
    let Some(character) = Actor::new(character_guid) else {
        return Ok(DeletedCharacterPartyCleanup::AlreadyClean);
    };
    if store.character_exists_on_any_world_shard(character_guid)? {
        return Ok(DeletedCharacterPartyCleanup::Preserved);
    }
    let leave = run_server_leave(
        store,
        realm.as_ref(),
        character,
        DELETED_CHARACTER_LEAVE_ATTEMPTS,
        |realm, character| realm.deleted_character_party_leave(character),
    )?;
    match leave.outcome {
        PartyOutcome::Ran => {
            if let Some(previous) = leave.previous_roster {
                sync_group_mirrors_required(
                    store,
                    realm.as_ref(),
                    previous.group_id,
                    Some(&previous),
                    &store.world_stores(),
                )?;
            }
            Ok(DeletedCharacterPartyCleanup::Removed)
        }
        PartyOutcome::Refused(GroupRefusal::NotInGroup) => {
            Ok(DeletedCharacterPartyCleanup::AlreadyClean)
        }
        PartyOutcome::Refused(refusal) => {
            anyhow::bail!("realm-core refused deleted Character cleanup: {refusal:?}")
        }
    }
}

pub(crate) fn reconcile_deleted_character_parties<St: CharacterStore + ShardRoutingStore>(
    store: &St,
) -> Result<()> {
    let Some(realm) = store.realm_store()? else {
        return Ok(());
    };
    let mut failures = 0usize;
    let mut last_error = None;
    for character_guid in realm.party_member_guids()? {
        if let Err(error) = cleanup_deleted_character(store, character_guid) {
            failures += 1;
            log::warn!(
                "party: could not finish reconciling Character {character_guid} ({error:#}); retrying"
            );
            last_error = Some(error);
        }
    }
    let mut group_ids: std::collections::HashSet<u64> =
        realm.party_group_ids()?.into_iter().collect();
    for shard in store.world_stores() {
        match shard.party_group_ids() {
            Ok(ids) => group_ids.extend(ids),
            Err(error) => {
                failures += 1;
                log::warn!(
                    "party: could not enumerate mirrored groups on {} ({error:#}); retrying",
                    shard.shard_name()
                );
                last_error = Some(error);
            }
        }
    }
    for group_id in group_ids {
        if let Err(error) = sync_group_mirrors_required(
            store,
            realm.as_ref(),
            group_id,
            None,
            &store.world_stores(),
        ) {
            failures += 1;
            log::warn!("party: could not reconcile group {group_id} mirrors ({error:#}); retrying");
            last_error = Some(error);
        }
    }
    match last_error {
        Some(error) => Err(error.context(format!(
            "{failures} deleted Character party reconciliation attempt(s) were deferred"
        ))),
        None => Ok(()),
    }
}

const MIRROR_PUSH_ATTEMPTS: usize = 3;

const MIRROR_RETRY_WINDOW: std::time::Duration = std::time::Duration::from_secs(5);

pub(crate) fn sync_group_mirrors_required<St: ShardRoutingStore + ?Sized>(
    store: &St,
    realm: &dyn WorldStore,
    group_id: u64,
    previous: Option<&GroupRoster>,
    shards: &[std::sync::Arc<dyn WorldStore>],
) -> Result<()> {
    let roster = match realm.party_cleanup_group_roster_by_id(group_id)? {
        Some(roster) => roster,
        None => GroupRoster::disbanded(group_id, realm.group_roster_revision(group_id)?),
    };
    let mut roster = certify_roster_partitions(store, realm, roster)?;
    append_departed_partitions(&mut roster, previous);
    let mut failures = 0usize;
    let mut last_error = None;
    let retry_until = std::time::Instant::now() + MIRROR_RETRY_WINDOW;
    for shard in shards {
        let mut synced = false;
        for attempt in 0..MIRROR_PUSH_ATTEMPTS {
            if attempt > 0 && std::time::Instant::now() >= retry_until {
                break;
            }
            match shard.sync_group_mirror(&roster) {
                Ok(()) => {
                    synced = true;
                    break;
                }
                Err(error) => last_error = Some(error),
            }
        }
        if !synced {
            failures += 1;
        }
    }
    match last_error {
        Some(error) if failures > 0 => Err(error.context(format!(
            "{failures} World Shard party mirror update(s) failed within \
             {MIRROR_PUSH_ATTEMPTS} attempts and the retry window"
        ))),
        _ => Ok(()),
    }
}

pub(crate) fn sync_mirrors<St: ShardRoutingStore + ?Sized>(
    store: &St,
    realm: &dyn WorldStore,
    self_guid: u64,
    before: Option<GroupRoster>,
) {
    let now = match realm.group_roster(self_guid) {
        Ok(r) => r,
        Err(e) => {
            log::warn!(
                "party: could not read the realm-core roster for {self_guid} ({e:#}) — the shard mirrors keep their previous roster until the next op or world entry"
            );
            return;
        }
    };
    let mut touched: Vec<u64> = Vec::new();
    if let Some(r) = &now {
        touched.push(r.group_id);
    }
    if let Some(before) = &before {
        if !touched.contains(&before.group_id) {
            touched.push(before.group_id);
        }
    }
    for group_id in touched {
        let roster = match roster_or_disbanded(realm, group_id) {
            Ok(r) => r,
            Err(e) => {
                log::warn!(
                    "party: could not read realm-core group {group_id} ({e:#}) — shard mirrors unchanged"
                );
                continue;
            }
        };
        let mut roster = match certify_roster_partitions(store, realm, roster) {
            Ok(roster) => roster,
            Err(e) => {
                log::warn!(
                    "party: could not certify group {group_id} member partitions ({e:#}); shard mirrors unchanged"
                );
                continue;
            }
        };
        append_departed_partitions(&mut roster, before.as_ref());
        for shard in store.world_stores() {
            if let Err(e) = shard.sync_group_mirror(&roster) {
                log::warn!(
                    "party: could not mirror group {group_id} onto shard {} ({e:#}) — that shard's \
                     in-world party reads stay stale until the next op or world entry",
                    shard.shard_name()
                );
            }
        }
    }
}

pub(crate) fn on_world_entry<
    St: CharacterStore + PartyStore + SessionStore + ShardRoutingStore + ?Sized,
>(
    tx: &SessionTx,
    store: &St,
    character: Actor,
) -> Result<()> {
    let self_guid = character.guid();
    let Some(roster) = sync_arrival_mirror(store, self_guid)? else {
        return Ok(());
    };
    send(tx, render_list(store, self_guid, &roster.list_payload()))?;
    if roster.kind == GroupKind::Party {
        request_target_icons(store, character);
    }
    Ok(())
}

// GROUP_LIST clears client raid marks; request the full icon list after sending it.
fn request_target_icons<St: ShardRoutingStore + ?Sized>(store: &St, character: Actor) {
    let realm = match store.realm_store() {
        Ok(Some(realm)) => realm,
        Ok(None) => return,
        Err(error) => {
            log::warn!(
                "party: cannot request Target Icons for Character {}: {error:#}",
                character.guid()
            );
            return;
        }
    };
    let op = Op::TargetIcon {
        icon: lyracore_shared::group::TARGET_ICON_LIST_REQUEST,
        target: 0,
    };
    match run_on_authority(realm.as_ref(), character, op) {
        Ok(PartyOutcome::Ran) => {}
        outcome => log::warn!(
            "party: Target Icon list for {} at world entry not requested: {outcome:?}",
            character.guid()
        ),
    }
}

pub(crate) fn sync_arrival_mirror<St: PartyStore + ShardRoutingStore + ?Sized>(
    store: &St,
    self_guid: u64,
) -> Result<Option<GroupRoster>> {
    let Some(realm) = store.realm_store()? else {
        return Ok(None);
    };
    let roster = realm
        .group_roster(self_guid)?
        .map(|roster| certify_roster_partitions(store, realm.as_ref(), roster))
        .transpose()?;
    if let Some(stale) = store.group_roster(self_guid)? {
        if roster.as_ref().is_none_or(|r| r.group_id != stale.group_id) {
            let repair = roster_or_disbanded(realm.as_ref(), stale.group_id)?;
            let repair = certify_roster_partitions(store, realm.as_ref(), repair)?;
            if let Err(e) = store.sync_group_mirror(&repair) {
                log::warn!(
                    "party: could not clear guid {self_guid} from shard {}'s stale mirror of group \
                     {} ({e:#}) — that shard's party reads for them stay wrong until they join one",
                    store.shard_name(),
                    stale.group_id
                );
            }
        }
    }
    let Some(roster) = roster else {
        return Ok(None);
    };
    store.sync_group_mirror(&roster)?;
    Ok(Some(roster))
}

/// Keep the arrival fence until the authoritative Group mirror reaches the destination.
pub(crate) fn sync_transfer_arrival_mirror<St: PartyStore + ShardRoutingStore + ?Sized>(
    store: &St,
    character_guid: u64,
) -> Result<()> {
    let Some(realm) = store.realm_store()? else {
        return Ok(());
    };
    let roster = realm
        .group_roster(character_guid)?
        .map(|roster| certify_roster_partitions(store, realm.as_ref(), roster))
        .transpose()?;
    if let Some(stale) = store.group_roster(character_guid)? {
        if roster
            .as_ref()
            .is_none_or(|row| row.group_id != stale.group_id)
        {
            let repair = roster_or_disbanded(realm.as_ref(), stale.group_id)?;
            let repair = certify_roster_partitions(store, realm.as_ref(), repair)?;
            store.sync_group_mirror(&repair)?;
        }
    }
    if let Some(roster) = roster {
        let shards = store.world_stores();
        if shards.is_empty() {
            store.sync_group_mirror(&roster)?;
        } else {
            for shard in shards {
                shard.sync_group_mirror(&roster)?;
            }
        }
    }
    Ok(())
}

pub(crate) fn render_list<St: CharacterStore + SessionStore + ShardRoutingStore + ?Sized>(
    store: &St,
    self_guid: u64,
    roster: &RosterPayload,
) -> Outbound {
    let mut roster = roster.clone();
    for member in &mut roster.members {
        if member.name.is_empty() {
            member.name = presence::character_anywhere(store, member.guid)
                .ok()
                .flatten()
                .map(|c| c.name)
                .unwrap_or_default();
        }
    }
    let list = codec::build_group_list(self_guid, &roster, |guid| {
        presence::live_anywhere(store, guid)
    });
    let (opcode, body) = codec::build_group_list_raw(&list);
    Outbound::Raw { opcode, body }
}

#[cfg(test)]
mod partition_tests {
    use super::*;

    const LOCATOR: RealmCharacterPartition = RealmCharacterPartition {
        map_id: 0,
        instance_id: 0,
        revision: 7,
        transfer_pending: false,
        pending_destination_map: 0,
        pending_destination_instance: 0,
        bot_source_identity: spacetimedb_sdk::Identity::ZERO,
        bot_transfer_intent_id: 0,
        bot_controller_generation: 0,
    };

    #[test]
    fn only_the_shard_serving_the_captured_locator_can_report_a_pending_transfer() {
        let state = classify_party_partition(
            LOCATOR,
            &[
                PartyHolderObservation {
                    serves_locator: false,
                    has_escrow: true,
                    character_partition: Some((36, 1)),
                },
                PartyHolderObservation {
                    serves_locator: true,
                    has_escrow: false,
                    character_partition: Some((0, 0)),
                },
            ],
        )
        .expect("one settled holder");

        assert_eq!(state, PartyPartitionState::Known);
    }

    #[test]
    fn the_serving_shards_escrow_suppresses_its_stale_character_location() {
        let state = classify_party_partition(
            LOCATOR,
            &[PartyHolderObservation {
                serves_locator: true,
                has_escrow: true,
                character_partition: Some((36, 1)),
            }],
        )
        .expect("one pending holder");

        assert_eq!(state, PartyPartitionState::PendingTransfer);
    }

    #[test]
    fn a_departed_member_keeps_the_realms_membership_revision_as_a_removal_fence() {
        let departed = GroupMemberPartition {
            character_guid: 100,
            group_id: 7,
            membership_revision: 42,
            member_active: true,
            map_id: 0,
            instance_id: 0,
            locator_revision: 3,
            state: PartyPartitionState::Known,
        };
        let previous = GroupRoster {
            group_id: 7,
            members: vec![GroupRosterMember {
                guid: 100,
                slot: RaidSlot::default(),
            }],
            partitions: vec![departed],
            ..Default::default()
        };
        let mut current = GroupRoster::disbanded(7, 2);

        append_departed_partitions(&mut current, Some(&previous));

        assert_eq!(
            current.partitions,
            vec![GroupMemberPartition {
                member_active: false,
                state: PartyPartitionState::Unknown,
                ..departed
            }]
        );
    }
}
