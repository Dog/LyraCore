use crate::codec::{self};
use crate::stdb::bindings::*;
use crate::stdb::world_index::ShardId;
use crate::stdb::world_view::*;
use crate::world::Outbound;
use std::sync::atomic::Ordering;
use wow_world_messages::vanilla::opcodes::ServerOpcodeMessage;
use wow_world_messages::vanilla::WeatherChangeType;

pub(crate) fn weather_outbound(
    view: codec::ZoneWeatherView,
    change: WeatherChangeType,
) -> Vec<Outbound> {
    vec![Outbound::One(ServerOpcodeMessage::SMSG_WEATHER(Box::new(
        codec::build_weather(view, change),
    )))]
}

pub(crate) fn zone_weather_outbound<St: crate::world::WeatherStore + ?Sized>(
    store: &St,
    zone_id: u32,
    change: WeatherChangeType,
) -> Vec<Outbound> {
    vec![Outbound::One(crate::world::zone_weather_message(
        store, zone_id, change,
    ))]
}

pub(crate) fn weather_changed(view: &WorldView, shard: ShardId, row: &ZoneWeather) {
    let zone_id = row.zone_id;
    let view_of_row = crate::codec::ZoneWeatherView {
        weather_type: row.weather_type,
        intensity: row.intensity,
    };
    for viewer in viewers_on_shard(view, shard) {
        if viewer.zone_id.load(Ordering::Relaxed) != zone_id {
            continue;
        }
        enqueue(viewer.clone(), move |_| {
            crate::codec::weather::subscriptions::weather_outbound(
                view_of_row,
                WeatherChangeType::Smooth,
            )
        });
    }
}

#[cfg(test)]
pub(crate) fn relay_zone_weather(view: &WorldView, shard: ShardId, row: &ZoneWeather) {
    weather_changed(view, shard, row);
}

pub(crate) fn zone_crossed<St>(
    view: &WorldView,
    store: &St,
    shard: ShardId,
    guid: u64,
    map_id: u32,
    old_zone: u32,
    zone_id: u32,
) where
    St: crate::world::WeatherStore + Clone + 'static,
{
    if zone_id == old_zone {
        return;
    }
    let Some(viewer) = view.viewer_of_owner_on_shard(shard, OwnerGuid(guid)) else {
        return;
    };
    if !viewer.enter_zone(zone_id) {
        return;
    }
    let store = store.clone();
    enqueue(viewer.clone(), move |_| {
        let (opcode, body) = codec::build_init_world_states(map_id, zone_id);
        let mut out = vec![Outbound::Raw { opcode, body }];
        out.extend(zone_weather_outbound(
            &store,
            zone_id,
            WeatherChangeType::Instant,
        ));
        out
    });
}

crate::stdb::relays::declare_relays! {
    game_zone_weather [false] {
        insert(Snapshot) => |context, _event, row| weather_changed(&context.view, context.shard, row);
        update(Snapshot) => |context, _event, _old, row| weather_changed(&context.view, context.shard, row);
    }
}

#[cfg(test)]
mod audience_tests;
