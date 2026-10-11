//! Table Relays for this Protocol Family.

use crate::codec::{self};
use crate::stdb::bindings::*;
use crate::stdb::world_index::ShardId;
use crate::stdb::world_view::*;
use crate::world::Outbound;
use std::sync::atomic::Ordering;
use wow_world_messages::vanilla::opcodes::ServerOpcodeMessage;
use wow_world_messages::vanilla::WeatherChangeType;

/// Encode one zone's sky. The audience decision (the viewer's stored zone) already happened in
/// `world_view::weather_changed`, and the changed row carried the whole packet, so nothing is read
/// here.
pub(crate) fn weather_outbound(
    view: codec::ZoneWeatherView,
    change: WeatherChangeType,
) -> Vec<Outbound> {
    vec![Outbound::One(ServerOpcodeMessage::SMSG_WEATHER(Box::new(
        codec::build_weather(view, change),
    )))]
}

/// Read `zone_id`'s current sky off this shard's cache and encode it — the zone-entry leg, run on
/// the arriving viewer's own writer thread. Goes through the same `WeatherStore` seam world entry
/// uses, so a zone with no row means fine weather here too.
pub(crate) fn zone_weather_outbound<St: crate::world::WeatherStore + ?Sized>(
    store: &St,
    zone_id: u32,
    change: WeatherChangeType,
) -> Vec<Outbound> {
    vec![Outbound::One(crate::world::zone_weather_message(
        store, zone_id, change,
    ))]
}

/// A zone's sky changed → `SMSG_WEATHER` to every viewer standing in that zone on this shard, and
/// to nobody else. The zone comparison IS the audience: weather is per zone, so an Elwynn row can
/// never reach a Westfall viewer however close the two stand.
///
/// Smooth, because this is a scheduled drift the client should fade in. The synchronizing sends
/// (world entry, zone entry) are Instant and come from [`zone_crossed`] and the login path.
///
/// The row carries the whole packet, so no cache read happens in the job — which also means a
/// later row cannot overtake an earlier one's encoding and show a client the wrong sky.
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

/// The `game_zone_weather` relay leg, reachable from the world-session tests: the encrypted-wire
/// test drives the SAME body [`arm_shard`] wires to the table callback, so a live weather row and a
/// test row take one path. Test-only because a shard's own callback is the sole production caller.
#[cfg(test)]
pub(crate) fn relay_zone_weather(view: &WorldView, shard: ShardId, row: &ZoneWeather) {
    weather_changed(view, shard, row);
}

/// The self entity's own row says it is standing in a different zone than the viewer's routing
/// state remembers → move the routing state and send the destination zone's current weather ONCE,
/// Instant, so the stale source-zone sky is replaced immediately (story 7).
///
/// This covers walking, a same-map teleport and a taxi landing — every path that changes the zone
/// without rebuilding the session. A cross-map path tears the viewer down and re-runs world entry,
/// which sends its own weather.
///
/// `store` is this shard's coordinator, taken through the [`crate::world::WeatherStore`] seam
/// because the destination's sky is READ, on the arriving viewer's own writer thread, rather than
/// carried by the row that triggered this.
///
/// The row-delta comparison comes FIRST because this sits on the pump, under the highest-rate
/// table in the module: every creature heartbeat and every player step that stays inside its zone
/// leaves here without touching the viewer registry. Creatures never carry a zone at all, so their
/// updates are all `0 -> 0`.
pub(crate) fn zone_crossed<St>(
    view: &WorldView,
    store: &St,
    shard: ShardId,
    guid: u64,
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
        crate::codec::weather::subscriptions::zone_weather_outbound(
            &store,
            zone_id,
            WeatherChangeType::Instant,
        )
    });
}

crate::stdb::relays::declare_relays! {
    game_zone_weather [false] {
        insert(Snapshot) => |context, _event, row| weather_changed(&context.view, context.shard, row);
        update(Snapshot) => |context, _event, _old, row| weather_changed(&context.view, context.shard, row);
    }
}
