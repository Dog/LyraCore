use anyhow::Result;

use crate::codec::ZoneWeatherView;
use crate::stdb::bindings::*;
use crate::stdb::Coordinator;
use crate::world::WeatherStore;

impl WeatherStore for Coordinator {
    fn zone_weather(&self, zone_id: u32) -> Result<Option<ZoneWeatherView>> {
        if zone_id == 0 {
            return Ok(None);
        }
        Ok(self
            .0
            .coord()
            .conn
            .db
            .game_zone_weather()
            .zone_id()
            .find(&zone_id)
            .map(|row| ZoneWeatherView {
                weather_type: row.weather_type,
                intensity: row.intensity,
            }))
    }
}
