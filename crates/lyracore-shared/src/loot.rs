//! The loot tier contract shared by the Module and the Gateway.

/// Why the Module refused a loot Durable Request. The tag is the whole reducer error text, so
/// neither tier matches on human prose. The Module keeps the detail in its own log.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LootRefusal {
    /// The Actor is not in the Loot Source's Loot Tag eligibility set.
    LootTagIneligible,
    /// The Loot Source is on another map, in another instance, or beyond loot range.
    OutOfRange,
    /// The named Loot Source does not exist, or is not a lootable creature corpse.
    NoLootSource,
    /// The looter is not in the world, or is dead.
    LooterUnavailable,
    /// The Loot Source has nothing left in the requested slot or purse.
    NothingToLoot,
    /// No open Loot Roll answers this vote, or the voter already voted.
    RollUnavailable,
    /// The Actor does not hold the master-looter right on that row.
    NotMasterLooter,
    /// The master-loot recipient is no longer in the world.
    RecipientUnavailable,
    /// The master-loot recipient has no room for the item.
    RecipientInventoryFull,
}

impl LootRefusal {
    pub const ALL: [Self; 9] = [
        Self::LootTagIneligible,
        Self::OutOfRange,
        Self::NoLootSource,
        Self::LooterUnavailable,
        Self::NothingToLoot,
        Self::RollUnavailable,
        Self::NotMasterLooter,
        Self::RecipientUnavailable,
        Self::RecipientInventoryFull,
    ];

    pub fn as_tag(self) -> &'static str {
        match self {
            Self::LootTagIneligible => "loot:loot_tag_ineligible",
            Self::OutOfRange => "loot:out_of_range",
            Self::NoLootSource => "loot:no_loot_source",
            Self::LooterUnavailable => "loot:looter_unavailable",
            Self::NothingToLoot => "loot:nothing_to_loot",
            Self::RollUnavailable => "loot:roll_unavailable",
            Self::NotMasterLooter => "loot:not_master_looter",
            Self::RecipientUnavailable => "loot:recipient_unavailable",
            Self::RecipientInventoryFull => "loot:recipient_inventory_full",
        }
    }

    pub fn parse_tag(tag: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|refusal| refusal.as_tag() == tag)
    }
}

/// A failure in the trusted Gateway entry before a loot core can answer the request. These tags
/// cross the reducer boundary so legacy untagged gameplay compatibility cannot hide a broken
/// Operator or Actor invariant.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LootBoundaryFailure {
    OperatorRejected,
    MissingActor,
}

impl LootBoundaryFailure {
    pub const ALL: [Self; 2] = [Self::OperatorRejected, Self::MissingActor];

    pub fn as_tag(self) -> &'static str {
        match self {
            Self::OperatorRejected => "loot:boundary_operator_rejected",
            Self::MissingActor => "loot:boundary_missing_actor",
        }
    }

    pub fn parse_tag(tag: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|failure| failure.as_tag() == tag)
    }
}

#[cfg(test)]
mod tests {
    use super::{LootBoundaryFailure, LootRefusal};

    #[test]
    fn every_refusal_tag_round_trips() {
        for refusal in LootRefusal::ALL {
            assert_eq!(LootRefusal::parse_tag(refusal.as_tag()), Some(refusal));
        }
        assert_eq!(LootRefusal::parse_tag("loot:"), None);
        assert_eq!(
            LootRefusal::parse_tag("loot_tag_ineligible: actor_guid=7 corpse_guid=11"),
            None
        );
        assert_eq!(
            LootRefusal::parse_tag("gw_take_loot reducer timed out after 10s"),
            None
        );
    }

    #[test]
    fn every_boundary_failure_tag_round_trips() {
        for failure in LootBoundaryFailure::ALL {
            assert_eq!(
                LootBoundaryFailure::parse_tag(failure.as_tag()),
                Some(failure)
            );
            assert_eq!(LootRefusal::parse_tag(failure.as_tag()), None);
        }
    }
}

/// A reservation is the grant; only an unreserved quest row requires current need.
pub fn quest_take_allowed(reserved_for: u64, taker_guid: u64, taker_needs_item: bool) -> bool {
    reserved_for == taker_guid || (reserved_for == 0 && taker_needs_item)
}

pub fn group_loot_take_allowed(
    withheld: bool,
    reserved_for: u64,
    master_only: bool,
    designated_looter_guid: u64,
    taker_guid: u64,
) -> bool {
    if withheld {
        return false;
    }
    if reserved_for != 0 {
        return reserved_for == taker_guid;
    }
    !master_only && (designated_looter_guid == 0 || designated_looter_guid == taker_guid)
}

#[derive(Clone, Copy)]
pub struct QuestItemObjective {
    pub quest_entry: u32,
    pub quest_type: u32,
    pub kind: u8,
    pub target_entry: u32,
    pub required_count: u32,
}

/// Decide item need from active quests, objective kinds, quantities and Raid membership.
pub fn needs_quest_item(
    in_raid: bool,
    active_quests: &[u32],
    objectives: impl IntoIterator<Item = QuestItemObjective>,
    item: u32,
    held: u32,
) -> bool {
    objectives.into_iter().any(|objective| {
        objective.kind == crate::quest::objective_kind::COLLECT_ITEM
            && active_quests.contains(&objective.quest_entry)
            && objective.target_entry == item
            && held < objective.required_count.max(1)
            && crate::quest::quest_progresses_for_raid(in_raid, objective.quest_type)
    })
}

#[cfg(test)]
mod rule_tests {
    use super::*;

    #[test]
    fn withheld_loot_stays_owned_by_the_roll_even_with_a_reservation() {
        assert!(!group_loot_take_allowed(true, 7, false, 0, 7));
        assert!(group_loot_take_allowed(false, 7, true, 8, 7));
        assert!(!group_loot_take_allowed(false, 7, false, 0, 8));
    }

    #[test]
    fn a_quest_reservation_survives_the_characters_need() {
        assert!(quest_take_allowed(7, 7, false));
        assert!(!quest_take_allowed(7, 8, true));
        assert!(quest_take_allowed(0, 8, true));
        assert!(!quest_take_allowed(0, 8, false));
    }

    #[test]
    fn collect_need_respects_raid_type_quantity_and_the_minimum_requirement() {
        let collect = |quest_type, target_entry, required_count| QuestItemObjective {
            quest_entry: 100,
            quest_type,
            kind: 1,
            target_entry,
            required_count,
        };
        assert!(needs_quest_item(false, &[100], [collect(0, 55, 10)], 55, 9));
        assert!(!needs_quest_item(
            false,
            &[100],
            [collect(0, 55, 10)],
            55,
            10
        ));
        assert!(!needs_quest_item(true, &[100], [collect(0, 55, 10)], 55, 0));
        assert!(needs_quest_item(true, &[100], [collect(62, 55, 10)], 55, 0));
        assert!(needs_quest_item(false, &[100], [collect(0, 55, 0)], 55, 0));
        assert!(!needs_quest_item(false, &[100], [collect(0, 55, 0)], 55, 1));
        assert!(!needs_quest_item(
            false,
            &[100],
            [collect(0, 56, 10)],
            55,
            0
        ));
        assert!(!needs_quest_item(false, &[], [collect(0, 55, 10)], 55, 0));
        let kill = QuestItemObjective {
            kind: 0,
            ..collect(0, 55, 10)
        };
        assert!(!needs_quest_item(false, &[100], [kill], 55, 0));
    }
}
