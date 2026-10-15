use super::super::*;

#[derive(Default)]
pub(crate) struct MemberStatsState {
    pub(crate) member_presence_reads: std::sync::atomic::AtomicUsize,
}

impl MemberStatsStore for WorldFake {
    fn group_mates(&self, self_guid: u64) -> Result<Vec<u64>> {
        let roster = match &self.topology.realm {
            Some(realm) => {
                let party = realm.party.party.lock().unwrap();
                party
                    .group_of(self_guid)
                    .and_then(|group| party.roster(group))
            }
            None => self
                .party
                .mirror
                .lock()
                .unwrap()
                .iter()
                .find(|roster| roster.has_member(self_guid))
                .cloned(),
        };
        Ok(roster
            .map(|roster| roster.member_guids())
            .unwrap_or_default()
            .into_iter()
            .filter(|member| *member != self_guid)
            .collect())
    }

    fn member_presence(&self, guid: u64) -> Result<MemberPresence> {
        self.member_stats
            .member_presence_reads
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(
            match presence::of(self, guid)?.map(|presence| presence.whereabouts) {
                Some(presence::Whereabouts::InWorld { entity, .. }) => {
                    MemberPresence::Live(Box::new(codec::MemberStats::from_entity(&entity)))
                }
                Some(presence::Whereabouts::InTransit) => MemberPresence::InTransit,
                Some(presence::Whereabouts::Offline) | None => MemberPresence::Offline,
            },
        )
    }
}
