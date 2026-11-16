//! SVG + radar raster → the 16-level grayscale PNG the Kindle displays.

use anyhow::Context;
use resvg::{tiny_skia, usvg};

use crate::config::Contrast;
use crate::image::{Gray, luma_over_white};
use crate::layout::{self, Frame};

const FONTS: [&[u8]; 3] = [
    include_bytes!("../assets/fonts/IBMPlexSansArabic-Regular.ttf"),
    include_bytes!("../assets/fonts/IBMPlexSansArabic-SemiBold.ttf"),
    include_bytes!("../assets/fonts/IBMPlexSansArabic-Bold.ttf"),
];

pub struct Renderer {
    options: usvg::Options<'static>,
    contrast: Contrast,
}

impl Renderer {
    pub fn new(contrast: Contrast) -> Self {
        let mut options = usvg::Options::default();
        FONTS
            .iter()
            .for_each(|font| options.fontdb_mut().load_font_data(font.to_vec()));
        Self { options, contrast }
    }

    pub fn png(&self, frame: &Frame) -> anyhow::Result<Vec<u8>> {
        self.gray(frame)?.encode_png()
    }

    pub fn gray(&self, frame: &Frame) -> anyhow::Result<Gray> {
        self.gray_svg(
            &layout::svg(frame),
            frame
                .radar
                .filter(|_| frame.rain_expected())
                .map(|radar| (&radar.data.map, layout::RADAR_LEFT, layout::RADAR_TOP)),
        )
    }

    /// Only the header: the top [`layout::HEADER_HEIGHT`] rows of [`Self::gray`]'s
    /// page, pixel for pixel, at a fraction of the cost. The frame's weather
    /// and radar aren't used.
    pub fn header_gray(&self, frame: &Frame) -> anyhow::Result<Gray> {
        self.rasterize(&layout::header_svg(frame), layout::HEADER_HEIGHT, None)
    }

    /// Rasterises a dashboard SVG, blitting an optional radar raster first at (left, top).
    pub fn gray_svg(&self, svg: &str, radar: Option<(&Gray, i64, i64)>) -> anyhow::Result<Gray> {
        self.rasterize(svg, layout::HEIGHT, radar)
    }

    /// The top `height` rows of the page `svg` draws.
    fn rasterize(
        &self,
        svg: &str,
        height: u32,
        radar: Option<(&Gray, i64, i64)>,
    ) -> anyhow::Result<Gray> {
        let tree = usvg::Tree::from_str(svg, &self.options).context("parsing SVG")?;
        let mut pixmap =
            tiny_skia::Pixmap::new(layout::WIDTH, height).context("allocating canvas")?;
        pixmap.fill(tiny_skia::Color::WHITE);
        if let Some((map, left, top)) = radar {
            draw_gray(&mut pixmap, map, left, top);
        }
        resvg::render(&tree, tiny_skia::Transform::default(), &mut pixmap.as_mut());
        Ok(Gray {
            width: layout::WIDTH,
            height,
            // The canvas is opaque, so premultiplied and straight RGBA agree.
            pixels: pixmap
                .pixels()
                .iter()
                .map(|p| {
                    let luma = luma_over_white([p.red(), p.green(), p.blue(), p.alpha()]);
                    quantize(self.contrast.darken(luma))
                })
                .collect(),
        })
    }
}

fn draw_gray(pixmap: &mut tiny_skia::Pixmap, image: &Gray, left: i64, top: i64) {
    let (w, h) = (i64::from(pixmap.width()), i64::from(pixmap.height()));
    let x0 = left.max(0);
    let x1 = (left + i64::from(image.width)).min(w);
    let y0 = top.max(0);
    let y1 = (top + i64::from(image.height)).min(h);
    if x0 >= x1 || y0 >= y1 {
        return;
    }
    (y0..y1).for_each(|y| {
        let src_row = ((y - top) * i64::from(image.width)) as usize;
        let dst_row = (y * w) as usize;
        let src = &image.pixels[src_row + (x0 - left) as usize..src_row + (x1 - left) as usize];
        pixmap.pixels_mut()[dst_row + x0 as usize..dst_row + x1 as usize]
            .iter_mut()
            .zip(src)
            .for_each(|(dst, &g)| {
                *dst = tiny_skia::PremultipliedColorU8::from_rgba(g, g, g, 255).unwrap_or(*dst)
            });
    });
}

/// The Paperwhite shows 16 gray levels; snapping here keeps the panel from dithering.
fn quantize(value: u8) -> u8 {
    ((u16::from(value) + 8) / 17 * 17) as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::config::{ClockFormat, Config};
    use crate::prayer::{DayTimes, local_date};
    use crate::sources::Snapshot;
    use crate::weather::{Quarter, WET_MM};
    use chrono::{DateTime, Duration, Utc};

    /// Every layout the header sits over: each prayer next, both clocks,
    /// without weather, with it, and with rain on the way (the radar card).
    #[test]
    fn the_header_alone_is_the_pages_top_pixel_for_pixel() {
        let mut config: Config = toml::from_str(include_str!("../config.example.toml")).unwrap();
        let mut weather = crate::weather::decode(
            include_str!("../tests/fixtures/open-meteo.json"),
            config.location.timezone,
        )
        .unwrap();
        let start = weather.hours[0].at;
        let mut rainy = weather.clone();
        rainy.quarters = (0..400)
            .map(|i| Quarter {
                at: start + Duration::minutes(15 * i),
                precipitation_mm: WET_MM,
            })
            .collect();
        weather
            .quarters
            .iter_mut()
            .for_each(|q| q.precipitation_mm = 0.0);
        let snapshot = |data| Snapshot {
            data,
            fetched_at: start,
        };
        let (dry, wet) = (snapshot(weather), snapshot(rainy));
        let renderer = Renderer::new(config.display.contrast);
        let header_len = (layout::WIDTH * layout::HEADER_HEIGHT) as usize;
        for clock in [ClockFormat::TwentyFourHour, ClockFormat::TwelveHour] {
            config.display.clock = clock;
            // Through a day, each prayer's countdown in turn, and past Isha.
            for hours in (1..24).step_by(2) {
                let now: DateTime<Utc> = start + Duration::hours(hours) + Duration::minutes(7);
                let date = local_date(&config.location, now);
                let prayers = DayTimes::compute(&config.location, &config.prayer, date).unwrap();
                for weather in [None, Some(&dry), Some(&wet)] {
                    let frame = Frame {
                        now,
                        config: &config,
                        prayers: &prayers,
                        weather,
                        radar: None,
                    };
                    let page = renderer.gray(&frame).unwrap();
                    let header = renderer.header_gray(&frame).unwrap();
                    assert_eq!((header.width, header.height), (layout::WIDTH, 300));
                    assert!(
                        page.pixels[..header_len] == header.pixels[..],
                        "the header differs from the page's top at {now} ({clock:?})"
                    );
                    assert!(
                        header.pixels.iter().filter(|&&p| p == 0).count() > 10_000,
                        "the header draws the clock and countdown"
                    );
                }
            }
        }
    }

    #[test]
    fn quantize_hits_16_levels() {
        let levels: std::collections::BTreeSet<u8> = (0..=255).map(quantize).collect();
        assert_eq!(levels.len(), 16);
        assert_eq!((quantize(0), quantize(255), quantize(9)), (0, 255, 17));
    }
}
