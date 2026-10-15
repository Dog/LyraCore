use super::super::*;
use crate::codec::{
    build_member_stats, build_member_status, full_update_mask, member_status, stats_delta,
    MemberStats as MemberStatsView, MemberStatsPacket,
};
use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard, PoisonError};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum MemberPresence {
    Live(Box<MemberStatsView>),
    InTransit,
    Offline,
}

pub(crate) trait MemberStatsStore: Send + Sync {
    fn group_mates(&self, self_guid: u64) -> Result<Vec<u64>>;

    fn member_presence(&self, guid: u64) -> Result<MemberPresence>;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum MemberSnapshot {
    Offline,
    Live(Box<MemberStatsView>),
}

/// Only writer jobs may change these snapshots, in the same order as client packets.
#[derive(Clone, Default)]
pub(crate) struct MemberStatsRecord(Arc<Mutex<HashMap<u64, MemberSnapshot>>>);

impl MemberStatsRecord {
    pub(crate) fn lock(&self) -> MutexGuard<'_, HashMap<u64, MemberSnapshot>> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub(crate) fn forget(&self, guid: u64) {
        self.lock().remove(&guid);
    }

    pub(crate) fn forget_all(&self) {
        self.lock().clear();
    }
}

pub(crate) fn member_stats_tick<St: MemberStatsStore + ?Sized>(
    store: &St,
    self_guid: u64,
    is_created: impl Fn(u64) -> bool,
    snapshots: &mut HashMap<u64, MemberSnapshot>,
) -> Result<Vec<Outbound>> {
    let mates = store.group_mates(self_guid)?;
    let mut presences = Vec::with_capacity(mates.len());
    for guid in mates {
        let presence = if is_created(guid) {
            None
        } else {
            Some(store.member_presence(guid)?)
        };
        presences.push((guid, presence));
    }

    snapshots.retain(|guid, _| presences.iter().any(|(mate, _)| mate == guid));
    let mut outbound = Vec::new();
    for (guid, presence) in presences {
        let (opcode, body) = match presence {
            None => {
                snapshots.remove(&guid);
                continue;
            }
            Some(MemberPresence::InTransit) => continue,
            Some(MemberPresence::Offline) => {
                if snapshots.insert(guid, MemberSnapshot::Offline) == Some(MemberSnapshot::Offline)
                {
                    continue;
                }
                build_member_status(MemberStatsPacket::Changed, guid, member_status::OFFLINE)
            }
            Some(MemberPresence::Live(stats)) => {
                let previous = match snapshots.insert(guid, MemberSnapshot::Live(stats.clone())) {
                    Some(MemberSnapshot::Live(previous)) => Some(previous),
                    Some(MemberSnapshot::Offline) | None => None,
                };
                let mask = stats_delta(previous.as_deref(), &stats);
                if mask.is_empty() {
                    continue;
                }
                build_member_stats(
                    MemberStatsPacket::Changed,
                    guid,
                    mask,
                    previous.as_deref(),
                    &stats,
                )
            }
        };
        outbound.push(Outbound::Raw { opcode, body });
    }
    Ok(outbound)
}

pub(crate) struct MemberStats;

impl<St: MemberStatsStore + ?Sized> ProtocolFamily<St> for MemberStats {
    fn handle(
        store: &St,
        session: &mut ProtocolSession,
        request: ProtocolRequest,
    ) -> Result<ProtocolReply> {
        let ClientOpcodeMessage::CMSG_REQUEST_PARTY_MEMBER_STATS(request) = request.message()?
        else {
            return Err(anyhow!("opcode routed to wrong Protocol Family"));
        };
        let Some(self_guid) = session.self_guid() else {
            return Ok(ProtocolReply::default());
        };
        let guid = request.guid.guid();
        let (opcode, body) = match full_answer(store, self_guid, guid) {
            Ok(answer) => answer,
            Err(error) => {
                log::warn!("member stats: no answer for {guid} to {self_guid}: {error:#}");
                return Ok(ProtocolReply::default());
            }
        };
        let record = match &session.state {
            WorldState::InWorld(world) => world.subs.member_stats_record().cloned(),
            WorldState::CharSelect => None,
        };
        let answer = move || {
            if let Some(record) = record {
                record.forget(guid);
            }
            vec![Outbound::Raw { opcode, body }]
        };
        Ok(vec![Outbound::Job(Box::new(answer))].into())
    }
}

fn full_answer<St: MemberStatsStore + ?Sized>(
    store: &St,
    self_guid: u64,
    guid: u64,
) -> Result<(u16, Vec<u8>)> {
    let packet = MemberStatsPacket::Full;
    if !store.group_mates(self_guid)?.contains(&guid) {
        return Ok(build_member_status(packet, guid, member_status::OFFLINE));
    }
    Ok(match store.member_presence(guid)? {
        MemberPresence::Live(stats) => {
            let mask = full_update_mask(&stats);
            build_member_stats(packet, guid, mask, None, &stats)
        }
        MemberPresence::InTransit => build_member_status(
            packet,
            guid,
            member_status::ONLINE | member_status::ZONE_OUT,
        ),
        MemberPresence::Offline => build_member_status(packet, guid, member_status::OFFLINE),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forgetting_one_member_keeps_the_others() {
        let record = MemberStatsRecord::default();
        record.lock().insert(1, MemberSnapshot::Offline);
        record.lock().insert(2, MemberSnapshot::Offline);
        record.forget(1);
        assert_eq!(record.lock().keys().copied().collect::<Vec<_>>(), [2]);
        record.forget_all();
        assert!(record.lock().is_empty());
    }
}
