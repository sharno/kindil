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

    /// The whole page, [`layout::WIDTH`] × the frame's page height.
    pub fn gray(&self, frame: &Frame) -> anyhow::Result<Gray> {
        self.rasterize(
            &layout::svg(frame),
            frame.page.height(),
            frame
                .radar
                .filter(|_| frame.rain_expected())
                .map(|radar| (&radar.data.map, layout::RADAR_LEFT, frame.page.radar_top())),
        )
    }

    /// Only the header: the top [`layout::Page::header_height`] rows of
    /// [`Self::gray`]'s page, pixel for pixel, at a fraction of the cost.
    /// The frame's weather and radar aren't used.
    pub fn header_gray(&self, frame: &Frame) -> anyhow::Result<Gray> {
        self.rasterize(&layout::header_svg(frame), frame.page.header_height(), None)
    }

    /// Rasterises a full-page dashboard SVG, blitting an optional radar raster first at (left, top).
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
    use crate::layout::Page;
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
        // A whole page, and one under kindil's status bar.
        for (page_size, header_height) in [(Page::FULL, 300), (Page::below(56), 282)] {
            let header_len = (layout::WIDTH * header_height) as usize;
            for clock in [ClockFormat::TwentyFourHour, ClockFormat::TwelveHour] {
                config.display.clock = clock;
                // Through a day, each prayer's countdown in turn, and past Isha.
                for hours in (1..24).step_by(2) {
                    let now: DateTime<Utc> = start + Duration::hours(hours) + Duration::minutes(7);
                    let date = local_date(&config.location, now);
                    let prayers =
                        DayTimes::compute(&config.location, &config.prayer, date).unwrap();
                    for weather in [None, Some(&dry), Some(&wet)] {
                        let frame = Frame {
                            now,
                            config: &config,
                            prayers: &prayers,
                            weather,
                            radar: None,
                            page: page_size,
                        };
                        let page = renderer.gray(&frame).unwrap();
                        assert_eq!(
                            (page.width, page.height),
                            (layout::WIDTH, page_size.height())
                        );
                        let header = renderer.header_gray(&frame).unwrap();
                        assert_eq!(
                            (header.width, header.height),
                            (layout::WIDTH, header_height)
                        );
                        assert!(
                            page.pixels[..header_len] == header.pixels[..],
                            "the header differs from the page's top at {now} ({clock:?}, \
                             {page_size:?})"
                        );
                        assert!(
                            header.pixels.iter().filter(|&&p| p == 0).count() > 10_000,
                            "the header draws the clock and countdown"
                        );
                    }
                }
            }
        }
    }

    /// Under a bar, the page is the full page's elements with the spacing
    /// tightened: nothing falls off the bottom, and the radar moves with its card.
    #[test]
    fn a_shorter_page_keeps_everything_on_it() {
        let config: Config = toml::from_str(include_str!("../config.example.toml")).unwrap();
        let mut rainy = crate::weather::decode(
            include_str!("../tests/fixtures/open-meteo.json"),
            config.location.timezone,
        )
        .unwrap();
        let start = rainy.hours[0].at;
        rainy.quarters = (0..400)
            .map(|i| Quarter {
                at: start + Duration::minutes(15 * i),
                precipitation_mm: WET_MM,
            })
            .collect();
        let weather = Snapshot {
            data: rainy,
            fetched_at: start,
        };
        let radar = Snapshot {
            data: crate::radar::RadarImage {
                observed_at: start,
                map: Gray {
                    width: layout::RADAR_SIZE.0,
                    height: layout::RADAR_SIZE.1,
                    pixels: vec![0x88; (layout::RADAR_SIZE.0 * layout::RADAR_SIZE.1) as usize],
                },
            },
            fetched_at: start,
        };
        let now = start + Duration::hours(1);
        let date = local_date(&config.location, now);
        let prayers = DayTimes::compute(&config.location, &config.prayer, date).unwrap();
        let renderer = Renderer::new(config.display.contrast);
        let frame = |page| Frame {
            now,
            config: &config,
            prayers: &prayers,
            weather: Some(&weather),
            radar: Some(&radar),
            page,
        };
        assert!(frame(Page::FULL).rain_expected());
        let short = Page::below(56);
        let full = renderer.gray(&frame(Page::FULL)).unwrap();
        let gray = renderer.gray(&frame(short)).unwrap();
        let row = |y: u32| &gray.pixels[(y * layout::WIDTH) as usize..][..layout::WIDTH as usize];
        // The radar card, raster and all, is the full page's, moved up.
        let (left, top) = (layout::RADAR_LEFT as u32, short.radar_top() as u32);
        let lift = (layout::RADAR_TOP - short.radar_top()) as u32;
        for y in top - 2..top + layout::RADAR_SIZE.1 + 2 {
            let span = |pixels: &[u8], y: u32| {
                pixels[(y * layout::WIDTH + left - 2) as usize..]
                    [..layout::RADAR_SIZE.0 as usize + 4]
                    .to_vec()
            };
            assert_eq!(
                span(&gray.pixels, y),
                span(&full.pixels, y + lift),
                "row {y}"
            );
        }
        let gray_level = row(top + 200)[left as usize + 40];
        assert!(gray_level != 0 && gray_level != 255, "the radar raster");
        // The rain line is drawn, with white under it to the page's end.
        let inked = |y: u32| row(y).iter().any(|&p| p < 0x80);
        let last = (0..short.height()).rev().find(|&y| inked(y)).unwrap();
        assert!(
            (short.height() - 40..short.height() - 20).contains(&last),
            "the rain line ends at {last}"
        );
    }

    #[test]
    fn quantize_hits_16_levels() {
        let levels: std::collections::BTreeSet<u8> = (0..=255).map(quantize).collect();
        assert_eq!(levels.len(), 16);
        assert_eq!((quantize(0), quantize(255), quantize(9)), (0, 255, 17));
    }
}
