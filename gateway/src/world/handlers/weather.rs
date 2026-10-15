use super::super::*;

pub(crate) trait WeatherStore: Send + Sync {
    fn zone_weather(&self, zone_id: u32) -> Result<Option<codec::ZoneWeatherView>>;
}

pub(crate) fn zone_weather_message<St: WeatherStore + ?Sized>(
    store: &St,
    zone_id: u32,
    change: WeatherChangeType,
) -> ServerOpcodeMessage {
    let view = match store.zone_weather(zone_id) {
        Ok(view) => view.unwrap_or_default(),
        Err(e) => {
            log::warn!(
                "world: could not read weather for zone {zone_id} ({e:#}) — sending fine weather"
            );
            codec::ZoneWeatherView::default()
        }
    };
    ServerOpcodeMessage::SMSG_WEATHER(Box::new(codec::build_weather(view, change)))
}
