use super::*;

struct WeatherFake {
    rows: Vec<(u32, codec::ZoneWeatherView)>,
    error: Option<&'static str>,
}

impl WeatherStore for WeatherFake {
    fn zone_weather(&self, zone_id: u32) -> Result<Option<codec::ZoneWeatherView>> {
        if let Some(error) = self.error {
            return Err(anyhow!("{error}"));
        }
        Ok(self
            .rows
            .iter()
            .find(|(zone, _)| *zone == zone_id)
            .map(|(_, view)| *view))
    }
}

fn run(store: &WeatherFake, zone_id: u32) -> SMSG_WEATHER {
    match zone_weather_message(store, zone_id, WeatherChangeType::Instant) {
        ServerOpcodeMessage::SMSG_WEATHER(weather) => *weather,
        other => panic!("expected SMSG_WEATHER, got {other}"),
    }
}

#[test]
fn a_zone_with_no_weather_row_is_fine_weather() {
    let weather = run(
        &WeatherFake {
            rows: Vec::new(),
            error: None,
        },
        12,
    );
    assert_eq!(weather.weather_type, WeatherType::Fine);
    assert_eq!(weather.grade, 0.0);
    assert_eq!(weather.sound_id, 0);
    assert_eq!(weather.change, WeatherChangeType::Instant);
}

#[test]
fn a_weather_read_failure_is_fine_weather() {
    let weather = run(
        &WeatherFake {
            rows: Vec::new(),
            error: Some("shard cache unavailable"),
        },
        12,
    );
    assert_eq!(weather.weather_type, WeatherType::Fine);
    assert_eq!(weather.grade, 0.0);
}

#[test]
fn a_zone_with_a_weather_row_gets_that_sky_and_no_other_zone_does() {
    let store = WeatherFake {
        rows: vec![(
            12,
            codec::ZoneWeatherView {
                weather_type: 1,
                intensity: 0.8,
            },
        )],
        error: None,
    };
    let weather = run(&store, 12);
    assert_eq!(weather.weather_type, WeatherType::Rain);
    assert_eq!(weather.grade, 0.8);
    assert_eq!(run(&store, 40).weather_type, WeatherType::Fine);
}

fn world_session_in_world(
    store: std::sync::Arc<WorldFake>,
    character_guid: u64,
) -> (UnixStream, DecrypterHalf, std::thread::JoinHandle<()>) {
    let (mut client, server_end) = world_session_socket_pair();
    let server = std::thread::spawn(move || {
        run_world_session(server_end, store.clone()).unwrap();
    });
    let (mut c_enc, mut c_dec) = client_handshake(&mut client, "TESTER", K);
    CMSG_PLAYER_LOGIN {
        guid: Guid::new(character_guid),
    }
    .write_encrypted_client(&mut client, &mut c_enc)
    .unwrap();
    drain_world_entry(&mut client, &mut c_dec);
    (client, c_dec, server)
}

fn zone_weather_row(
    zone_id: u32,
    weather_type: u8,
    intensity: f32,
) -> crate::stdb::bindings::ZoneWeather {
    crate::stdb::bindings::ZoneWeather {
        zone_id,
        weather_type,
        intensity,
        changed_at_micros: 0,
    }
}

#[test]
fn forced_elwynn_rain_reaches_only_the_client_standing_in_elwynn() {
    const ELWYNN: u32 = 12;
    const WESTFALL: u32 = 40;
    let view = std::sync::Arc::new(crate::stdb::world_view::WorldView::new(true));

    let mut westfall_entity = warrior_entity();
    westfall_entity.guid = 2;
    westfall_entity.zone_id = WESTFALL;
    let session_store = |entity: codec::EntityView| {
        std::sync::Arc::new({
            let base = tester_store(7);
            WorldFake {
                session: SessionState {
                    login_entity: Some(entity),
                    relay_view: Some(view.clone()),
                    ..base.session
                },
                ..base
            }
        })
    };

    let (mut elwynn_client, mut elwynn_dec, elwynn_server) =
        world_session_in_world(session_store(warrior_entity()), 1);
    let (mut westfall_client, mut westfall_dec, westfall_server) =
        world_session_in_world(session_store(westfall_entity), 2);

    crate::codec::weather::subscriptions::relay_zone_weather(
        &view,
        0,
        &zone_weather_row(ELWYNN, 1, 0.75),
    );
    crate::codec::weather::subscriptions::relay_zone_weather(
        &view,
        0,
        &zone_weather_row(WESTFALL, 2, 0.30),
    );

    match ServerOpcodeMessage::read_encrypted(&mut elwynn_client, &mut elwynn_dec).unwrap() {
        ServerOpcodeMessage::SMSG_WEATHER(m) => {
            assert_eq!(m.weather_type, WeatherType::Rain);
            assert_eq!(m.grade, 0.75);
            assert_eq!(m.sound_id, 8535, "0.75 is the heavy rain band");
            assert_eq!(m.change, WeatherChangeType::Smooth);
        }
        other => panic!("the Elwynn client must receive Elwynn's rain, got {other}"),
    }
    match ServerOpcodeMessage::read_encrypted(&mut westfall_client, &mut westfall_dec).unwrap() {
        ServerOpcodeMessage::SMSG_WEATHER(m) => {
            assert_eq!(
                m.weather_type,
                WeatherType::Snow,
                "the first weather the Westfall client sees must be Westfall's own — rain here \
                 means Elwynn's row leaked across the zone boundary"
            );
            assert_eq!(m.grade, 0.30);
            assert_eq!(m.sound_id, 8536, "0.30 is the light snow band");
        }
        other => panic!("the Westfall client must receive Westfall's snow, got {other}"),
    }

    let quiet = std::time::Duration::from_millis(250);
    for (name, client, dec) in [
        ("Elwynn", &mut elwynn_client, &mut elwynn_dec),
        ("Westfall", &mut westfall_client, &mut westfall_dec),
    ] {
        client.set_read_timeout(Some(quiet)).unwrap();
        assert!(
            ServerOpcodeMessage::read_encrypted(client, dec).is_err(),
            "the {name} client must receive exactly one weather packet, its own zone's"
        );
    }

    drop(elwynn_client);
    drop(westfall_client);
    elwynn_server.join().unwrap();
    westfall_server.join().unwrap();
}
