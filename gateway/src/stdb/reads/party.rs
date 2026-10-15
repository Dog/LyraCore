use super::super::bindings::*;
use super::super::connection::Coordinator;
use lyracore_shared::group::{GroupKind, RaidSlot};

impl Coordinator {
    pub fn group_roster(&self, character_guid: u64) -> Option<crate::world::party::GroupRoster> {
        let group_id = self
            .0
            .coord()
            .party_memberships
            .read()
            .unwrap()
            .group_of(character_guid)?;
        self.group_roster_by_id(group_id)
    }

    pub fn group_roster_by_id(&self, group_id: u64) -> Option<crate::world::party::GroupRoster> {
        let guard = self.0.coord();
        let db = &guard.conn.db;
        let group = db.game_group().group_id().find(&group_id)?;
        let row_ids = guard
            .party_memberships
            .read()
            .unwrap()
            .member_row_ids(group_id);
        let mut rows: Vec<(u64, u64, u8)> = row_ids
            .into_iter()
            .filter_map(|row_id| db.game_group_member().id().find(&row_id))
            .map(|m| {
                let membership_revision = db
                    .game_group_member_partition()
                    .character_guid()
                    .find(&m.character_guid)
                    .filter(|partition| partition.group_id == group_id && partition.member_active)
                    .map_or(m.id, |partition| partition.membership_revision);
                (membership_revision, m.character_guid, m.raid_slot)
            })
            .collect();
        rows.sort_unstable();
        let partitions = rows
            .iter()
            .map(|(membership_revision, character_guid, _)| {
                crate::world::party::GroupMemberPartition {
                    character_guid: *character_guid,
                    group_id,
                    membership_revision: *membership_revision,
                    member_active: true,
                    map_id: 0,
                    instance_id: 0,
                    locator_revision: 0,
                    state: crate::world::party::PartyPartitionState::Unknown,
                }
            })
            .collect();
        Some(crate::world::party::GroupRoster {
            group_id,
            roster_revision: db
                .game_group_roster_revision()
                .group_id()
                .find(&group_id)
                .map_or(1, |row| row.revision),
            leader_guid: group.leader_guid,
            loot_method: group.loot_method,
            loot_threshold: group.loot_threshold,
            master_looter_guid: group.master_looter_guid,
            kind: GroupKind::from_wire_or_default(group.group_type),
            members: rows
                .into_iter()
                .map(|(_, guid, slot)| crate::world::party::GroupRosterMember {
                    guid,
                    slot: RaidSlot::from_wire_or_default(slot),
                })
                .collect(),
            partitions,
        })
    }
}
