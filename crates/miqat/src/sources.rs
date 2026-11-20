//! Remote data with its fetch time; refreshed lazily when a frame is requested.

use chrono::{DateTime, Duration, Utc};

use crate::config::Config;
use crate::layout;
use crate::radar::{Basemap, RadarClient, RadarImage, Viewport};
use crate::weather::{self, Weather};

#[derive(Debug, Clone)]
pub struct Snapshot<T> {
    pub data: T,
    pub fetched_at: DateTime<Utc>,
}

/// Open-Meteo updates current conditions every 15 minutes.
pub const WEATHER_EVERY: Duration = Duration::minutes(15);
/// RainViewer publishes a frame every 10 minutes.
pub const RADAR_EVERY: Duration = Duration::minutes(10);

/// Due when missing, old, or fetched "after" `now`: the clock went back.
fn is_due<T>(snapshot: &Option<Snapshot<T>>, now: DateTime<Utc>, every: Duration) -> bool {
    snapshot
        .as_ref()
        .is_none_or(|s| now - s.fetched_at >= every || now < s.fetched_at)
}

/// Cheap enough to clone (an agent, a basemap and a radar frame of
/// [`layout::RADAR_SIZE`]): a host can fetch on a copy while it draws with
/// the original.
#[derive(Clone)]
pub struct Sources {
    agent: ureq::Agent,
    radar: RadarClient,
    viewport: Viewport,
    weather: Option<Snapshot<Weather>>,
    radar_state: RadarState,
}

/// Radar needs the static basemap first; it is fetched once per process.
#[derive(Clone)]
enum RadarState {
    NoBasemap,
    Ready {
        basemap: Basemap,
        latest: Option<Snapshot<RadarImage>>,
    },
}

impl Sources {
    pub fn new(config: &Config) -> Self {
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_global(Some(std::time::Duration::from_secs(20)))
            .user_agent(concat!("miqat/", env!("CARGO_PKG_VERSION")))
            .build()
            .into();
        let (width, height) = layout::RADAR_SIZE;
        Self {
            radar: RadarClient::new(agent.clone()),
            agent,
            viewport: Viewport::centered(&config.location, config.radar.zoom, width, height),
            weather: None,
            radar_state: RadarState::NoBasemap,
        }
    }

    pub fn weather(&self) -> Option<&Snapshot<Weather>> {
        self.weather.as_ref()
    }

    /// Radar already drawn over the basemap.
    pub fn radar(&self) -> Option<&Snapshot<RadarImage>> {
        match &self.radar_state {
            RadarState::NoBasemap => None,
            RadarState::Ready { latest, .. } => latest.as_ref(),
        }
    }

    /// Whether [`Self::refresh`] at `now` would fetch anything.
    pub fn due(&self, now: DateTime<Utc>) -> bool {
        let radar = match &self.radar_state {
            RadarState::NoBasemap => true,
            RadarState::Ready { latest, .. } => is_due(latest, now, RADAR_EVERY),
        };
        is_due(&self.weather, now, WEATHER_EVERY) || (self.rain_expected(now) && radar)
    }

    fn rain_expected(&self, now: DateTime<Utc>) -> bool {
        self.weather
            .as_ref()
            .is_some_and(|snapshot| snapshot.data.rain_expected(now))
    }

    /// Refetches whatever is due. A failed fetch keeps the previous snapshot, so the
    /// dashboard shows aging data rather than nothing; it is retried on the next frame.
    pub fn refresh(&mut self, config: &Config, now: DateTime<Utc>) {
        if is_due(&self.weather, now, WEATHER_EVERY) {
            let url = weather::request_url(&config.location, &config.weather);
            let fetched = self
                .agent
                .get(&url)
                .call()
                .and_then(|mut r| r.body_mut().read_to_string())
                .map_err(anyhow::Error::from)
                .and_then(|body| weather::decode(&body, config.location.timezone));
            match fetched {
                Ok(data) => {
                    self.weather = Some(Snapshot {
                        data,
                        fetched_at: now,
                    })
                }
                Err(e) => eprintln!("weather: {e:#}"),
            }
        }
        let rain = self.rain_expected(now);
        let state = std::mem::replace(&mut self.radar_state, RadarState::NoBasemap);
        self.radar_state = if rain {
            self.refresh_radar(self.load_basemap(state), now)
        } else {
            state
        };
    }

    fn load_basemap(&self, state: RadarState) -> RadarState {
        match state {
            RadarState::NoBasemap => match self.radar.basemap(&self.viewport) {
                Ok(basemap) => RadarState::Ready {
                    basemap,
                    latest: None,
                },
                Err(e) => {
                    eprintln!("basemap: {e:#}");
                    RadarState::NoBasemap
                }
            },
            ready @ RadarState::Ready { .. } => ready,
        }
    }

    fn refresh_radar(&self, state: RadarState, now: DateTime<Utc>) -> RadarState {
        match state {
            RadarState::Ready { basemap, latest } if is_due(&latest, now, RADAR_EVERY) => {
                let latest = match self.radar.latest(&self.viewport, &basemap) {
                    Ok(data) => Some(Snapshot {
                        data,
                        fetched_at: now,
                    }),
                    Err(e) => {
                        eprintln!("radar: {e:#}");
                        latest
                    }
                };
                RadarState::Ready { basemap, latest }
            }
            other => other,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn due_when_missing_or_old() {
        let now = Utc::now();
        assert!(is_due::<()>(&None, now, WEATHER_EVERY));
        let fresh = Some(Snapshot {
            data: (),
            fetched_at: now - Duration::minutes(5),
        });
        assert!(!is_due(&fresh, now, WEATHER_EVERY));
        assert!(is_due(&fresh, now + Duration::minutes(10), WEATHER_EVERY));
        // The clock set back past the fetch: refetched, not kept for a day.
        assert!(is_due(&fresh, now - Duration::minutes(10), WEATHER_EVERY));
    }

    #[test]
    fn due_when_refresh_would_fetch() {
        let config: Config = toml::from_str(include_str!("../config.example.toml")).unwrap();
        let mut sources = Sources::new(&config);
        let mut weather = crate::weather::decode(
            include_str!("../tests/fixtures/open-meteo.json"),
            config.location.timezone,
        )
        .unwrap();
        let now = weather.hours[0].at;
        assert!(sources.due(now), "no weather yet");
        weather
            .quarters
            .iter_mut()
            .for_each(|q| q.precipitation_mm = 0.0);
        sources.weather = Some(Snapshot {
            data: weather.clone(),
            fetched_at: now,
        });
        assert!(!sources.due(now), "fresh, and no rain: no radar");
        assert!(sources.due(now + WEATHER_EVERY));
        // Rain on the way: the radar, and first its basemap.
        weather
            .quarters
            .iter_mut()
            .for_each(|q| q.precipitation_mm = crate::weather::WET_MM);
        sources.weather = Some(Snapshot {
            data: weather,
            fetched_at: now,
        });
        assert!(sources.due(now));
    }
}
