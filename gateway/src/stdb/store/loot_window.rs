use anyhow::Result;
use lyracore_shared::group::GroupKind;
use lyracore_shared::loot::{LootBoundaryFailure, LootRefusal};
use spacetimedb_sdk::Table;

use crate::codec;
use crate::stdb::bindings::*;
use crate::stdb::connection::{call_reducer, LiveConn};
use crate::stdb::reads::player_item_count;
use crate::stdb::{classify, Coordinator, DurableFailure};
use crate::world::{Actor, LootWindowRefusal, LootWindowRequestStatus, LootWindowStore};

impl LootWindowStore for Coordinator {
    fn loot_target_money(&self, target_guid: u64) -> Result<u32> {
        Ok(self
            .0
            .coord()
            .conn
            .db
            .game_world_entity()
            .guid()
            .find(&target_guid)
            .filter(|e| e.dead)
            .map(|e| e.money)
            .unwrap_or(0))
    }

    fn loot_target_items(
        &self,
        target_guid: u64,
        viewer_guid: u64,
    ) -> Result<Vec<codec::LootItemView>> {
        let guard = self.0.coord();
        let db = &guard.conn.db;
        let mut items: Vec<codec::LootItemView> = db
            .game_corpse_loot()
            .iter()
            .filter(|l| l.corpse_guid == target_guid)
            .filter(|l| {
                if l.quest_only {
                    let needs = viewer_needs_quest_item(&guard, viewer_guid, l.item_entry);
                    quest_row_visible_to_viewer(l.quest_only, l.reserved_for, viewer_guid, needs)
                } else {
                    group_loot_row_visible_to_viewer(
                        l.reserved_for,
                        l.withheld,
                        l.master_only,
                        l.designated_looter_guid,
                        viewer_guid,
                    )
                }
            })
            .map(|l| {
                let display_id = db
                    .game_item_template()
                    .entry()
                    .find(&l.item_entry)
                    .map(|t| t.display_id)
                    .unwrap_or(0);
                (
                    l.slot,
                    l.item_entry,
                    l.count,
                    display_id,
                    l.random_property_id,
                )
            })
            .collect();
        items.sort_by_key(|(slot, ..)| *slot);
        Ok(items)
    }

    fn use_gameobject(&self, actor: Actor, go_guid: u64) -> Result<LootWindowRequestStatus> {
        let coord = self.0.call_pipe();
        legacy_loot_request_status(call_reducer!(
            coord.conn.reducers,
            "gw_use_gameobject",
            gw_use_gameobject_then(self.session_actor(actor), go_guid)
        ))
    }

    fn open_creature_loot(
        &self,
        actor: Actor,
        corpse_guid: u64,
    ) -> Result<LootWindowRequestStatus> {
        let coord = self.0.call_pipe();
        strict_loot_request_status(call_reducer!(
            coord.conn.reducers,
            "gw_open_creature_loot",
            gw_open_creature_loot_then(self.session_actor(actor), corpse_guid)
        ))
    }

    fn skin_corpse(&self, actor: Actor, corpse_guid: u64) -> Result<LootWindowRequestStatus> {
        let coord = self.0.call_pipe();
        legacy_loot_request_status(call_reducer!(
            coord.conn.reducers,
            "gw_skin",
            gw_skin_then(self.session_actor(actor), corpse_guid)
        ))
    }

    fn loot_money(&self, actor: Actor, target_guid: u64) -> Result<LootWindowRequestStatus> {
        let coord = self.0.call_pipe();
        strict_loot_request_status(call_reducer!(
            coord.conn.reducers,
            "gw_loot_money",
            gw_loot_money_then(self.session_actor(actor), target_guid)
        ))
    }

    fn take_loot(
        &self,
        actor: Actor,
        corpse_guid: u64,
        loot_slot: u8,
    ) -> Result<LootWindowRequestStatus> {
        let coord = self.0.call_pipe();
        strict_loot_request_status(call_reducer!(
            coord.conn.reducers,
            "gw_take_loot",
            gw_take_loot_then(self.session_actor(actor), corpse_guid, loot_slot)
        ))
    }
}

fn viewer_in_raid(guard: &LiveConn, viewer_guid: u64) -> bool {
    let Some(group_id) = guard
        .party_memberships
        .read()
        .unwrap()
        .group_of(viewer_guid)
    else {
        return false;
    };
    guard
        .conn
        .db
        .game_group()
        .group_id()
        .find(&group_id)
        .is_some_and(|group| {
            GroupKind::from_wire(group.group_type).unwrap_or_default() == GroupKind::Raid
        })
}

fn viewer_needs_quest_item(guard: &LiveConn, viewer_guid: u64, item_entry: u32) -> bool {
    let db = &guard.conn.db;
    let active: Vec<u32> = db
        .game_character_quest()
        .iter()
        .filter(|quest| quest.character_guid == viewer_guid && !quest.rewarded)
        .map(|quest| quest.quest_entry)
        .collect();
    let objective_table = db.game_quest_objective();
    let objectives = objective_table
        .iter()
        .filter(|objective| active.contains(&objective.quest_entry))
        .map(|objective| lyracore_shared::loot::QuestItemObjective {
            quest_entry: objective.quest_entry,
            quest_type: db
                .game_quest_template()
                .entry()
                .find(&objective.quest_entry)
                .map_or(0, |template| template.quest_type),
            kind: objective.kind,
            target_entry: objective.target_entry,
            required_count: objective.required_count,
        });
    lyracore_shared::loot::needs_quest_item(
        viewer_in_raid(guard, viewer_guid),
        &active,
        objectives,
        item_entry,
        player_item_count(db, viewer_guid, item_entry),
    )
}

pub(crate) fn quest_row_visible_to_viewer(
    quest_only: bool,
    reserved_for: u64,
    viewer_guid: u64,
    viewer_needs_item: bool,
) -> bool {
    !quest_only
        || lyracore_shared::loot::quest_take_allowed(reserved_for, viewer_guid, viewer_needs_item)
}

pub(crate) fn group_loot_row_visible_to_viewer(
    reserved_for: u64,
    withheld: bool,
    master_only: bool,
    designated_looter_guid: u64,
    viewer_guid: u64,
) -> bool {
    lyracore_shared::loot::group_loot_take_allowed(
        withheld,
        reserved_for,
        master_only,
        designated_looter_guid,
        viewer_guid,
    )
}

#[derive(Clone, Copy)]
enum UntaggedLootRejection {
    Fatal,
    LegacyUnanswered,
}

fn strict_loot_request_status(result: Result<()>) -> Result<LootWindowRequestStatus> {
    loot_request_status(result, UntaggedLootRejection::Fatal)
}

fn legacy_loot_request_status(result: Result<()>) -> Result<LootWindowRequestStatus> {
    loot_request_status(result, UntaggedLootRejection::LegacyUnanswered)
}

fn loot_request_status(
    result: Result<()>,
    untagged: UntaggedLootRejection,
) -> Result<LootWindowRequestStatus> {
    match result {
        Ok(()) => Ok(LootWindowRequestStatus::Applied),
        Err(error) => match classify(&error) {
            DurableFailure::Refusal { reason } => {
                if LootBoundaryFailure::parse_tag(reason).is_some() {
                    return Err(error);
                }
                if let Some(refusal) = LootRefusal::parse_tag(reason) {
                    log::debug!("stdb: loot Durable Request refused: {error:#}");
                    return Ok(LootWindowRequestStatus::Refused(refusal.into()));
                }
                if reason.starts_with("loot:") {
                    return Err(error);
                }
                match untagged {
                    UntaggedLootRejection::LegacyUnanswered => {
                        log::debug!("stdb: legacy loot Durable Request refused: {error:#}");
                        Ok(LootWindowRequestStatus::Refused(
                            LootWindowRefusal::Unanswered,
                        ))
                    }
                    UntaggedLootRejection::Fatal => Err(error),
                }
            }
            DurableFailure::TransportLoss => Err(error),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::{group_loot_row_visible_to_viewer, quest_row_visible_to_viewer};

    #[test]
    fn non_quest_rows_are_always_visible_regardless_of_viewer_state() {
        assert!(quest_row_visible_to_viewer(false, 0, 7, false));
        assert!(quest_row_visible_to_viewer(false, 99, 7, false));
        assert!(quest_row_visible_to_viewer(false, 7, 7, false));
    }

    #[test]
    fn an_unreserved_quest_row_is_visible_only_to_a_needing_viewer() {
        assert!(
            quest_row_visible_to_viewer(true, 0, 7, true),
            "needs it -> sees the shared row"
        );
        assert!(
            !quest_row_visible_to_viewer(true, 0, 7, false),
            "doesn't need it -> invisible"
        );
    }

    #[test]
    fn a_reserved_clone_is_visible_only_to_its_owner() {
        assert!(
            quest_row_visible_to_viewer(true, 7, 7, false),
            "the viewer's own reserved clone"
        );
        assert!(quest_row_visible_to_viewer(true, 7, 7, true));
        assert!(
            !quest_row_visible_to_viewer(true, 7, 8, true),
            "reserved for someone else"
        );
        assert!(!quest_row_visible_to_viewer(true, 7, 8, false));
    }

    #[test]
    fn group_loot_baseline_ffa_is_visible_to_everyone() {
        assert!(group_loot_row_visible_to_viewer(0, false, false, 0, 7));
        assert!(group_loot_row_visible_to_viewer(0, false, false, 0, 8));
    }

    #[test]
    fn group_loot_withheld_row_is_invisible_to_everyone_including_the_eventual_winner() {
        assert!(!group_loot_row_visible_to_viewer(0, true, false, 0, 7));
        assert!(!group_loot_row_visible_to_viewer(0, true, false, 7, 7));
    }

    #[test]
    fn group_loot_master_only_row_never_shows_in_the_plain_window() {
        assert!(!group_loot_row_visible_to_viewer(0, false, true, 42, 42));
        assert!(!group_loot_row_visible_to_viewer(0, false, true, 42, 8));
    }

    #[test]
    fn group_loot_designated_row_is_visible_only_to_its_designee() {
        assert!(group_loot_row_visible_to_viewer(0, false, false, 42, 42));
        assert!(!group_loot_row_visible_to_viewer(0, false, false, 42, 8));
    }

    #[test]
    fn a_reserved_winner_waits_for_the_roll_to_release_its_row() {
        assert!(!group_loot_row_visible_to_viewer(7, true, true, 99, 7));
        assert!(group_loot_row_visible_to_viewer(7, false, true, 99, 7));
        assert!(!group_loot_row_visible_to_viewer(7, true, true, 99, 8));
    }

    #[test]
    fn a_grey_autoloots_while_a_green_on_the_same_corpse_is_withheld() {
        let grey = (0u64, false, false, 0u64);
        let green = (0u64, true, false, 0u64);
        for viewer in [7u64, 8u64] {
            assert!(
                group_loot_row_visible_to_viewer(grey.0, grey.1, grey.2, grey.3, viewer),
                "the grey row autoloots for any viewer"
            );
            assert!(
                !group_loot_row_visible_to_viewer(green.0, green.1, green.2, green.3, viewer),
                "the green row is withheld from every viewer while its roll is live"
            );
        }
    }
}

#[cfg(test)]
mod loot_reducer_tests {
    use super::*;
    use crate::stdb::connection::ReducerCallError;

    fn refusal_of(
        result: Result<()>,
        classify: fn(Result<()>) -> Result<LootWindowRequestStatus>,
    ) -> LootWindowRefusal {
        match classify(result) {
            Ok(LootWindowRequestStatus::Refused(refusal)) => refusal,
            Ok(LootWindowRequestStatus::Applied) => panic!("a Refusal was applied"),
            Err(error) => panic!("a Refusal ended the session: {error:#}"),
        }
    }

    fn rejected(reason: &str) -> Result<()> {
        Err(
            anyhow::Error::from(ReducerCallError::refused("gw_take_loot", reason))
                .context("loot window"),
        )
    }

    #[test]
    fn every_module_refusal_tag_becomes_one_client_answer() {
        for refusal in LootRefusal::ALL {
            for classify in [
                strict_loot_request_status as fn(Result<()>) -> Result<LootWindowRequestStatus>,
                legacy_loot_request_status,
            ] {
                assert_eq!(
                    refusal_of(rejected(refusal.as_tag()), classify),
                    LootWindowRefusal::from(refusal),
                    "{refusal:?}"
                );
            }
        }
    }

    #[test]
    fn only_legacy_cores_keep_untagged_gameplay_refusals_unanswered() {
        for reason in ["it is locked", "not a beast", "inventory is full"] {
            assert_eq!(
                refusal_of(rejected(reason), legacy_loot_request_status),
                LootWindowRefusal::Unanswered,
                "{reason}"
            );
            assert!(
                strict_loot_request_status(rejected(reason)).is_err(),
                "{reason}"
            );
        }
    }

    #[test]
    fn boundary_failures_and_unknown_loot_tags_are_fatal() {
        let reasons = LootBoundaryFailure::ALL
            .into_iter()
            .map(LootBoundaryFailure::as_tag)
            .chain(["loot:newer_module_refusal"]);

        for reason in reasons {
            assert!(
                strict_loot_request_status(rejected(reason)).is_err(),
                "{reason}"
            );
            assert!(
                legacy_loot_request_status(rejected(reason)).is_err(),
                "{reason}"
            );
        }
    }

    #[test]
    fn a_transport_loss_is_not_answered_as_a_refusal() {
        let not_refusals = [
            anyhow::Error::from(ReducerCallError::transport_lost("gw_loot_money")),
            anyhow::anyhow!(
                "wrapped text that mentions {}",
                LootRefusal::LootTagIneligible.as_tag()
            ),
        ];
        for error in not_refusals {
            let text = format!("{error:#}");
            assert!(strict_loot_request_status(Err(error)).is_err(), "{text}");
        }
    }
}
