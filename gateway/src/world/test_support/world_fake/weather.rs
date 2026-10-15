use super::super::*;

#[derive(Default)]
pub(crate) struct WeatherState {
    pub(crate) zone_weather: Vec<(u32, codec::ZoneWeatherView)>,
}

impl WeatherStore for WorldFake {
    fn zone_weather(&self, zone_id: u32) -> Result<Option<codec::ZoneWeatherView>> {
        Ok(self
            .weather
            .zone_weather
            .iter()
            .find(|(zone, _)| *zone == zone_id)
            .map(|(_, view)| *view))
    }
}
