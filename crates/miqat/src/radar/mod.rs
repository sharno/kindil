//! Rain radar from RainViewer over an OpenStreetMap basemap, as one grayscale image.
//!
//! Both layers share one Web Mercator pixel space: RainViewer 512 px tiles at zoom `z`
//! line up with OSM 256 px tiles at `z + 1`. RainViewer stops at zoom 7, so deeper maps
//! fetch its zoom-7 tiles for the same area and magnify them.

mod palette;

use anyhow::Context;
use chrono::{DateTime, Utc};
use serde::Deserialize;

use crate::config::{Location, Zoom};
use crate::image::{Gray, Rgba, luma_over_white, replace};

/// The static map for one viewport; radar can only be drawn over one of these.
#[derive(Debug, Clone, PartialEq)]
pub struct Basemap(Gray);

#[derive(Debug, Clone, PartialEq)]
pub struct RadarImage {
    pub observed_at: DateTime<Utc>,
    pub map: Gray,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Viewport {
    /// Top-left corner in world pixels.
    left: f64,
    top: f64,
    pub width: u32,
    pub height: u32,
    zoom: Zoom,
}

/// Radar tiles are 512 px at the configured zoom.
const RADAR_TILE: u32 = 512;
/// Basemap tiles are 256 px one zoom deeper: the same world size, sharper labels.
const BASEMAP_TILE: u32 = 256;

impl Viewport {
    pub fn centered(location: &Location, zoom: Zoom, width: u32, height: u32) -> Self {
        let world = world_size(zoom);
        let lat = location.latitude.degrees().to_radians();
        let x = (location.longitude.degrees() + 180.0) / 360.0 * world;
        let y = (1.0 - (lat.tan() + 1.0 / lat.cos()).ln() / std::f64::consts::PI) / 2.0 * world;
        Self {
            left: (x - f64::from(width) / 2.0).floor(),
            top: (y - f64::from(height) / 2.0).floor(),
            width,
            height,
            zoom,
        }
    }

    /// The same area at `zoom` (no deeper than this one), one pixel larger on each axis
    /// so magnifying it back covers every edge pixel.
    fn coarser(&self, zoom: Zoom) -> Coarse {
        let factor = 1u32 << (self.zoom.level() - zoom.level());
        let scale = f64::from(factor);
        let viewport = Viewport {
            left: (self.left / scale).floor(),
            top: (self.top / scale).floor(),
            width: self.width.div_ceil(factor) + 1,
            height: self.height.div_ceil(factor) + 1,
            zoom,
        };
        Coarse {
            factor,
            left: (self.left - viewport.left * scale) as i64,
            top: (self.top - viewport.top * scale) as i64,
            viewport,
        }
    }

    /// Tiles of `tile_px` that cover the viewport, and where each lands in it.
    fn placements(&self, tile_px: u32) -> Vec<Placement> {
        let size = f64::from(tile_px);
        let per_axis = (world_size(self.zoom) / size) as i64;
        let first = |start: f64| (start / size).floor() as i64;
        let last = |start: f64, len: u32| ((start + f64::from(len) - 1.0) / size).floor() as i64;
        let columns = first(self.left)..=last(self.left, self.width);
        let rows =
            (first(self.top)..=last(self.top, self.height)).filter(|y| (0..per_axis).contains(y));
        rows.flat_map(|y| {
            columns.clone().map(move |x| Placement {
                x: x.rem_euclid(per_axis) as u32,
                y: y as u32,
                left: x * i64::from(tile_px) - self.left as i64,
                top: y * i64::from(tile_px) - self.top as i64,
            })
        })
        .collect()
    }
}

fn world_size(zoom: Zoom) -> f64 {
    f64::from(RADAR_TILE) * f64::from(1u32 << zoom.level())
}

/// A lower-zoom viewport and where the original sits inside it once magnified.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Coarse {
    viewport: Viewport,
    factor: u32,
    left: i64,
    top: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Placement {
    x: u32,
    y: u32,
    left: i64,
    top: i64,
}

/// OSM's colorful style pushed toward white, so it reads as context under the rain.
fn faded(pixel: [u8; 4]) -> u8 {
    255 - ((255 - u32::from(luma_over_white(pixel))) * 11 / 20) as u8
}

/// Rain intensity (upper dBZ bound, gray): darker is heavier. Below 10 dBZ is mostly
/// virga and clutter that would wash out the map, so it stays transparent.
const INTENSITY_GRAYS: [(i8, u8); 6] = [
    (10, 255),
    (20, 195),
    (30, 150),
    (40, 105),
    (50, 55),
    (i8::MAX, 0),
];

fn rain_gray(pixel: [u8; 4]) -> u8 {
    let rgba = u32::from_be_bytes(pixel);
    palette::UNIVERSAL_BLUE
        .binary_search_by_key(&rgba, |&(color, _)| color)
        .ok()
        .and_then(|i| palette::UNIVERSAL_BLUE.get(i))
        .map(|&(_, dbz)| dbz)
        .and_then(|dbz| INTENSITY_GRAYS.iter().find(|(below, _)| dbz < *below))
        .map_or(255, |&(_, gray)| gray)
}

pub struct RadarClient {
    agent: ureq::Agent,
}

#[derive(Deserialize)]
struct WeatherMaps {
    host: String,
    radar: RadarFrames,
}

#[derive(Deserialize)]
struct RadarFrames {
    past: Vec<RadarFrame>,
}

#[derive(Deserialize)]
struct RadarFrame {
    time: i64,
    path: String,
}

impl RadarClient {
    pub fn new(agent: ureq::Agent) -> Self {
        Self { agent }
    }

    pub fn basemap(&self, viewport: &Viewport) -> anyhow::Result<Basemap> {
        let zoom = viewport.zoom.level() + 1;
        let blank = Gray::filled(viewport.width, viewport.height, 255);
        // Fetched once per process (a handful of tiles), within OSM's tile usage policy.
        self.composite(viewport, BASEMAP_TILE, blank, faded, replace, |p| {
            format!("https://tile.openstreetmap.org/{zoom}/{}/{}.png", p.x, p.y)
        })
        .map(Basemap)
    }

    /// The newest radar frame drawn over `basemap`, which must come from the same viewport.
    pub fn latest(&self, viewport: &Viewport, basemap: &Basemap) -> anyhow::Result<RadarImage> {
        let maps: WeatherMaps = self
            .agent
            .get("https://api.rainviewer.com/public/weather-maps.json")
            .call()?
            .body_mut()
            .read_json()
            .context("decoding RainViewer weather-maps.json")?;
        let frame = maps
            .radar
            .past
            .last()
            .context("RainViewer returned no radar frames")?;
        let observed_at =
            DateTime::from_timestamp(frame.time, 0).context("radar frame time out of range")?;
        let coarse = viewport.coarser(viewport.zoom.radar());
        let zoom = coarse.viewport.zoom.level();
        let blank = Gray::filled(coarse.viewport.width, coarse.viewport.height, 255);
        // Options `0_0`: unsmoothed, so pixels match the palette exactly; no separate snow colors.
        let rain = self
            .composite(
                &coarse.viewport,
                RADAR_TILE,
                blank,
                rain_gray,
                replace,
                |p| {
                    format!(
                        "{}{}/{RADAR_TILE}/{zoom}/{}/{}/2/0_0.png",
                        maps.host, frame.path, p.x, p.y
                    )
                },
            )?
            .magnified(
                coarse.factor,
                coarse.left,
                coarse.top,
                viewport.width,
                viewport.height,
            );
        // The darker value wins so map labels stay visible under rain.
        let mut map = basemap.0.clone();
        map.blit(&rain, 0, 0, u8::min);
        Ok(RadarImage { observed_at, map })
    }

    fn composite(
        &self,
        viewport: &Viewport,
        tile_px: u32,
        canvas: Gray,
        to_gray: impl Fn([u8; 4]) -> u8,
        combine: impl Fn(u8, u8) -> u8 + Copy,
        url: impl Fn(&Placement) -> String,
    ) -> anyhow::Result<Gray> {
        viewport
            .placements(tile_px)
            .iter()
            .try_fold(canvas, |mut canvas, placement| {
                let url = url(placement);
                let bytes = self
                    .agent
                    .get(&url)
                    .call()
                    .and_then(|mut r| r.body_mut().read_to_vec())
                    .with_context(|| format!("fetching {url}"))?;
                let tile = Rgba::decode_png(&bytes).with_context(|| format!("decoding {url}"))?;
                canvas.blit(
                    &tile.map_to_gray(&to_gray),
                    placement.left,
                    placement.top,
                    combine,
                );
                Ok(canvas)
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    fn location(lat: f64, lon: f64) -> Location {
        let text = format!(
            "[location]\nlatitude = {lat:?}\nlongitude = {lon:?}\ntimezone = \"UTC\"\n[prayer]\nmethod = \"egyptian\"\n"
        );
        toml::from_str::<Config>(&text).unwrap().location
    }

    #[test]
    fn viewport_centers_on_location() {
        let zoom = Zoom::try_from(7).unwrap();
        let v = Viewport::centered(&location(0.0, 0.0), zoom, 100, 50);
        let world = world_size(zoom);
        assert_eq!((v.left, v.top), (world / 2.0 - 50.0, world / 2.0 - 25.0));
    }

    #[test]
    fn placements_cover_viewport_and_wrap_longitude() {
        let zoom = Zoom::try_from(3).unwrap();
        let v = Viewport::centered(&location(0.0, 179.9), zoom, 758, 280);
        let tiles = v.placements(RADAR_TILE);
        let per_axis = (world_size(zoom) / f64::from(RADAR_TILE)) as u32;
        assert!(tiles.iter().all(|p| p.x < per_axis));
        assert!(
            tiles.iter().any(|p| p.x == 0),
            "wraps past the antimeridian: {tiles:?}"
        );
        // Every viewport pixel is covered by exactly one tile.
        let covered = |x: i64, y: i64| {
            tiles
                .iter()
                .filter(|p| {
                    (p.left..p.left + 512).contains(&x) && (p.top..p.top + 512).contains(&y)
                })
                .count()
        };
        assert!(
            [(0, 0), (757, 0), (0, 279), (757, 279), (379, 140)]
                .iter()
                .all(|&(x, y)| covered(x, y) == 1)
        );
    }

    #[test]
    fn coarser_viewport_covers_the_same_area() {
        let zoom = Zoom::try_from(9).unwrap();
        let v = Viewport::centered(&location(41.0, -71.0), zoom, 758, 276);
        let c = v.coarser(zoom.radar());
        assert_eq!(c.factor, 4);
        assert_eq!(c.viewport.zoom.level(), 7);
        let s = f64::from(c.factor);
        assert!((0..4).contains(&c.left) && (0..4).contains(&c.top));
        assert!(c.viewport.left * s <= v.left && c.viewport.top * s <= v.top);
        let right = (c.viewport.left + f64::from(c.viewport.width)) * s;
        let bottom = (c.viewport.top + f64::from(c.viewport.height)) * s;
        assert!(right >= v.left + f64::from(v.width) && bottom >= v.top + f64::from(v.height));
        // At the radar's own zoom nothing is magnified.
        let same = Viewport::centered(&location(41.0, -71.0), Zoom::try_from(7).unwrap(), 758, 276);
        let c = same.coarser(Zoom::try_from(7).unwrap());
        assert_eq!((c.factor, c.left, c.top), (1, 0, 0));
    }

    #[test]
    fn rain_intensity_maps_to_darker_grays() {
        let color = |dbz: i8| {
            let (rgba, _) = palette::UNIVERSAL_BLUE
                .iter()
                .find(|(_, d)| *d == dbz)
                .unwrap();
            rain_gray(rgba.to_be_bytes())
        };
        assert_eq!(color(5), 255);
        assert!(color(15) > color(35));
        assert!(color(35) > color(55));
        assert_eq!(rain_gray([0, 0, 0, 0]), 255);
    }
}
