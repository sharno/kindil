//! Miqat is a pinned git submodule/library, not a forked layout.
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
