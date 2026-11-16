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

    /// Rasterises a dashboard SVG, blitting an optional radar raster first at (left, top).
    pub fn gray_svg(&self, svg: &str, radar: Option<(&Gray, i64, i64)>) -> anyhow::Result<Gray> {
        let tree = usvg::Tree::from_str(svg, &self.options).context("parsing SVG")?;
        let mut pixmap =
            tiny_skia::Pixmap::new(layout::WIDTH, layout::HEIGHT).context("allocating canvas")?;
        pixmap.fill(tiny_skia::Color::WHITE);
        if let Some((map, left, top)) = radar {
            draw_gray(&mut pixmap, map, left, top);
        }
        resvg::render(&tree, tiny_skia::Transform::default(), &mut pixmap.as_mut());
        Ok(Gray {
            width: layout::WIDTH,
            height: layout::HEIGHT,
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

    #[test]
    fn quantize_hits_16_levels() {
        let levels: std::collections::BTreeSet<u8> = (0..=255).map(quantize).collect();
        assert_eq!(levels.len(), 16);
        assert_eq!((quantize(0), quantize(255), quantize(9)), (0, 255, 17));
    }
}
