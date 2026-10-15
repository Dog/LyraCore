use crate::codec::{self};
use crate::stdb::bindings::*;
use crate::stdb::views::hunter_pet_protocol_view;
use crate::stdb::world_index::{EntityLayer, ShardId};
use crate::stdb::world_view::*;
use crate::world::Outbound;
use lyracore_shared::spatial::BOX_HALF_SPAN;
use wow_world_messages::vanilla::opcodes::ServerOpcodeMessage;

/// Every viewer receives name-cache fields; only the owner receives care and progression fields.
pub(crate) fn hunter_pet_outbound(row: &HunterPetProtocol, viewer_guid: u64) -> Vec<Outbound> {
    let pet = hunter_pet_protocol_view(row.clone());
    let values = if row.owner_guid == viewer_guid {
        codec::build_hunter_pet_values(&pet)
    } else {
        codec::build_hunter_pet_identity_values(&pet)
    };
    vec![Outbound::One(ServerOpcodeMessage::SMSG_UPDATE_OBJECT(
        Box::new(values),
    ))]
}

/// Snapshot replay may resend these idempotent VALUES updates.
pub(crate) fn hunter_pet_appeared(
    view: &WorldView,
    shard: ShardId,
    previous: Option<&HunterPetProtocol>,
    row: &HunterPetProtocol,
) {
    let identity_changed = previous.is_none_or(|old| {
        old.pet_id != row.pet_id
            || old.live_pet_guid != row.live_pet_guid
            || old.name_timestamp != row.name_timestamp
    });
    let key = identity_changed
        .then(|| {
            view.spatial
                .entity_cell_on_shard(EntityLayer::WorldEntity, row.live_pet_guid, shard)
        })
        .flatten();
    for viewer in view.cell_audience(shard, key, BOX_HALF_SPAN, &[row.owner_guid]) {
        let row = row.clone();
        enqueue(viewer, move |viewer| {
            if !viewer.created.lock().unwrap().contains(&row.live_pet_guid) {
                return Vec::new();
            }
            hunter_pet_outbound(&row, viewer.self_guid)
        });
    }
}

crate::stdb::relays::declare_relays! {
    game_hunter_pet_protocol [false] {
        insert(Snapshot) => |context, _event, row| hunter_pet_appeared(&context.view, context.shard, None, row);
        update(Snapshot) => |context, _event, old, row| {
            if old != row {
                hunter_pet_appeared(&context.view, context.shard, Some(old), row)
            }
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::{Object, UpdateMask};
    use crate::stdb::world_index::CellKey;
    use crate::stdb::world_view::test_support::{queued_job, registered_viewer};

    fn pet() -> HunterPetProtocol {
        HunterPetProtocol {
            pet_id: 7,
            owner_guid: 7,
            live_pet_guid: 90,
            creature_entry: 3098,
            name: "Fang".into(),
            name_timestamp: 123,
            level: 8,
            pet_xp: 321,
            next_level_xp: 4_500,
            happiness: 166_500,
            loyalty_level: 2,
        }
    }

    #[test]
    fn renamed_pet_refreshes_known_observers_without_owner_only_fields() {
        let view = WorldView::new(true);
        let (owner, owner_rx) = registered_viewer(&view, 0, 7);
        let (observer, observer_rx) = registered_viewer(&view, 0, 8);
        let (_, unseen_rx) = registered_viewer(&view, 0, 9);
        let (foreign, foreign_rx) = registered_viewer(&view, 1, 10);
        for viewer in [&owner, &observer, &foreign] {
            viewer.created.lock().unwrap().insert(90);
        }
        view.spatial
            .upsert_entity(EntityLayer::WorldEntity, 90, CellKey::at(0, 0, 0, 0), 0, 7);
        hunter_pet_appeared(&view, 0, None, &pet());
        for (rx, owner_fields) in [(&owner_rx, true), (&observer_rx, false)] {
            let messages = queued_job(rx);
            let [Outbound::One(ServerOpcodeMessage::SMSG_UPDATE_OBJECT(update))] =
                messages.as_slice()
            else {
                panic!("expected the pet's descriptor update");
            };
            let Object::Values {
                mask1: UpdateMask::Unit(unit),
                ..
            } = &update.objects[0]
            else {
                panic!("expected sparse unit VALUES");
            };
            assert_eq!(unit.unit_petnumber(), Some(7));
            assert_eq!(unit.unit_pet_name_timestamp(), Some(123));
            assert_eq!(unit.unit_petexperience(), owner_fields.then_some(321));
            assert_eq!(unit.unit_power5(), owner_fields.then_some(166_500));
        }
        assert!(queued_job(&unseen_rx).is_empty());
        assert!(foreign_rx.try_recv().is_err());
    }

    #[test]
    fn pet_identity_update_is_dropped_after_the_viewer_forgets_the_pet() {
        let view = WorldView::new(true);
        let (observer, rx) = registered_viewer(&view, 0, 8);
        observer.created.lock().unwrap().insert(90);
        view.spatial
            .upsert_entity(EntityLayer::WorldEntity, 90, CellKey::at(0, 0, 0, 0), 0, 7);
        hunter_pet_appeared(&view, 0, None, &pet());
        observer.created.lock().unwrap().remove(&90);
        assert!(queued_job(&rx).is_empty());
    }
}
