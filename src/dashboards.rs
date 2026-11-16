//! Miqat is its own library (a git subtree of sharno/miqat), not a forked layout.
use chrono::{DateTime, Utc};
pub fn render(
    renderer: &miqat::render::Renderer,
    config: &miqat::config::Config,
    now: DateTime<Utc>,
    sources: &miqat::sources::Sources,
) -> anyhow::Result<miqat::image::Gray> {
    let date = miqat::prayer::local_date(&config.location, now);
    let prayers = miqat::prayer::DayTimes::compute(&config.location, &config.prayer, date)?;
    renderer.gray(&miqat::layout::Frame {
        now,
        config,
        prayers: &prayers,
        weather: sources.weather(),
        radar: sources.radar(),
    })
}
/// The next prayer (or sunrise) after `now`, as the dashboard counts down to it.
pub fn next_prayer(config: &miqat::config::Config, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
    let date = miqat::prayer::local_date(&config.location, now);
    miqat::prayer::DayTimes::compute(&config.location, &config.prayer, date)
        .ok()
        .map(|prayers| prayers.upcoming(now).at)
}

/// Deterministic layout fixture without fetching weather, radar or map tiles.
pub fn preview(
    config: &miqat::config::Config,
    now: DateTime<Utc>,
) -> anyhow::Result<miqat::image::Gray> {
    let date = miqat::prayer::local_date(&config.location, now);
    let prayers = miqat::prayer::DayTimes::compute(&config.location, &config.prayer, date)?;
    let weather = miqat::weather::decode(
        include_str!("../crates/miqat/tests/fixtures/open-meteo.json"),
        config.location.timezone,
    )?;
    let snapshot = miqat::sources::Snapshot {
        data: weather,
        fetched_at: now,
    };
    miqat::render::Renderer::new(config.display.contrast).gray(&miqat::layout::Frame {
        now,
        config,
        prayers: &prayers,
        weather: Some(&snapshot),
        radar: None,
    })
}
