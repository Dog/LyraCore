use super::*;
use crate::stdb::bindings::ZoneWeather;
use crate::stdb::world_index::CellKey;
use crate::stdb::world_view::test_support::{identity, queued_job, viewer_in_zone};
use crate::stdb::world_view::WorldView;
use crate::world::{Outbound, SessionTx};
use wow_world_messages::vanilla::opcodes::ServerOpcodeMessage;
use wow_world_messages::vanilla::{WeatherChangeType, WeatherType};

const ELWYNN: u32 = 12;

const WESTFALL: u32 = 40;

fn zone_weather(zone_id: u32, weather_type: u8, intensity: f32) -> ZoneWeather {
    ZoneWeather {
        zone_id,
        weather_type,
        intensity,
        changed_at_micros: 0,
    }
}

#[derive(Clone)]
struct CannedWeather(Option<(u32, u8, f32)>);

impl crate::world::WeatherStore for CannedWeather {
    fn zone_weather(&self, zone_id: u32) -> anyhow::Result<Option<crate::codec::ZoneWeatherView>> {
        Ok(self.0.and_then(|(zone, weather_type, intensity)| {
            (zone == zone_id).then_some(crate::codec::ZoneWeatherView {
                weather_type,
                intensity,
            })
        }))
    }
}

fn only_weather(out: Vec<Outbound>) -> wow_world_messages::vanilla::SMSG_WEATHER {
    match out.as_slice() {
        [Outbound::One(ServerOpcodeMessage::SMSG_WEATHER(m))] => **m,
        other => panic!(
            "expected exactly one SMSG_WEATHER, got {} messages",
            other.len()
        ),
    }
}

fn crossing_weather(out: Vec<Outbound>, zone_id: u32) -> wow_world_messages::vanilla::SMSG_WEATHER {
    match out.as_slice() {
        [Outbound::Raw { opcode, body }, Outbound::One(ServerOpcodeMessage::SMSG_WEATHER(m))] => {
            assert_eq!(
                (*opcode, body.clone()),
                crate::codec::build_init_world_states(0, zone_id)
            );
            **m
        }
        other => panic!(
            "expected world states then weather, got {} messages",
            other.len()
        ),
    }
}

#[test]
fn a_weather_row_reaches_its_own_zone_and_no_other() {
    let view = WorldView::new(true);
    let (elwynn_tx, elwynn_rx) = SessionTx::with_depth(0);
    let (also_elwynn_tx, also_elwynn_rx) = SessionTx::with_depth(0);
    let (westfall_tx, westfall_rx) = SessionTx::with_depth(0);
    let (zoneless_tx, zoneless_rx) = SessionTx::with_depth(0);
    let anchor = CellKey::at(0, 0, 0, 0);
    for (session, guid, zone, tx) in [
        (1, 9001, ELWYNN, elwynn_tx),
        (2, 9002, ELWYNN, also_elwynn_tx),
        (3, 9003, WESTFALL, westfall_tx),
        (4, 9004, 0, zoneless_tx),
    ] {
        let v = viewer_in_zone(session, guid, identity(session as u8), zone, tx);
        view.add_viewer_on_shard(v, anchor, 0);
    }

    weather_changed(&view, 0, &zone_weather(ELWYNN, 1, 0.5));

    for rx in [&elwynn_rx, &also_elwynn_rx] {
        let m = only_weather(queued_job(rx));
        assert_eq!(m.weather_type, WeatherType::Rain);
        assert_eq!(m.grade, 0.5);
        assert_eq!(m.sound_id, 8534);
        assert_eq!(
            m.change,
            WeatherChangeType::Smooth,
            "a scheduled change fades in rather than snapping"
        );
    }
    assert!(
        westfall_rx.try_recv().is_err(),
        "Elwynn's weather must not reach a viewer standing in Westfall"
    );
    assert!(
        zoneless_rx.try_recv().is_err(),
        "the unresolved zone matches no weather row"
    );
}

#[test]
fn crossing_into_a_zone_sends_its_weather_once() {
    let view = WorldView::new(true);
    let (walker_tx, walker_rx) = SessionTx::with_depth(0);
    let (resident_tx, resident_rx) = SessionTx::with_depth(0);
    let anchor = CellKey::at(0, 0, 0, 0);
    let walker = viewer_in_zone(1, 9001, identity(1), ELWYNN, walker_tx);
    let resident = viewer_in_zone(2, 9002, identity(2), WESTFALL, resident_tx);
    view.add_viewer_on_shard(walker.clone(), anchor, 0);
    view.add_viewer_on_shard(resident.clone(), anchor, 0);
    let store = CannedWeather(Some((WESTFALL, 3, 0.9)));

    zone_crossed(&view, &store, 0, walker.self_guid, 0, ELWYNN, WESTFALL);
    let m = crossing_weather(queued_job(&walker_rx), WESTFALL);
    assert_eq!(m.weather_type, WeatherType::Storm);
    assert_eq!(m.grade, 0.9);
    assert_eq!(m.sound_id, 8558);
    assert_eq!(m.change, WeatherChangeType::Instant);

    zone_crossed(&view, &store, 0, walker.self_guid, 0, WESTFALL, WESTFALL);
    zone_crossed(&view, &store, 0, walker.self_guid, 0, WESTFALL, 0);
    zone_crossed(&view, &store, 0, walker.self_guid, 0, 0, WESTFALL);
    assert!(
        walker_rx.try_recv().is_err(),
        "staying inside a zone sends nothing, the unresolved zone is not a crossing, and a \
         zone the viewer is already in is not re-sent"
    );
    assert_eq!(
        walker.zone_id.load(std::sync::atomic::Ordering::Relaxed),
        WESTFALL,
        "an unresolved zone must not erase the zone the viewer is really in"
    );
    assert!(
        resident_rx.try_recv().is_err(),
        "another player's crossing is not this viewer's business"
    );

    weather_changed(&view, 0, &zone_weather(WESTFALL, 2, 0.3));
    assert_eq!(
        only_weather(queued_job(&walker_rx)).weather_type,
        WeatherType::Snow
    );
}

#[test]
fn crossing_into_a_zone_with_no_weather_row_sends_fine_weather() {
    let view = WorldView::new(true);
    let (tx, rx) = SessionTx::with_depth(0);
    let walker = viewer_in_zone(1, 9001, identity(1), ELWYNN, tx);
    view.add_viewer_on_shard(walker.clone(), CellKey::at(0, 0, 0, 0), 0);

    zone_crossed(
        &view,
        &CannedWeather(None),
        0,
        walker.self_guid,
        0,
        ELWYNN,
        WESTFALL,
    );

    let m = crossing_weather(queued_job(&rx), WESTFALL);
    assert_eq!(m.weather_type, WeatherType::Fine);
    assert_eq!(m.grade, 0.0);
    assert_eq!(m.sound_id, 0);
    assert_eq!(m.change, WeatherChangeType::Instant);
}
