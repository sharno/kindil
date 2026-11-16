//! Minimal 8-bit image buffers and PNG codecs.

use anyhow::{Context, bail};

/// 8-bit grayscale, row-major; 0 is black.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Gray {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}

impl Gray {
    pub fn filled(width: u32, height: u32, value: u8) -> Self {
        Self {
            width,
            height,
            pixels: vec![value; (width * height) as usize],
        }
    }

    /// Merges `tile` into the canvas with its top-left at (left, top), clipped to the
    /// canvas; `combine(canvas, tile)` picks each resulting pixel.
    pub fn blit(&mut self, tile: &Gray, left: i64, top: i64, combine: impl Fn(u8, u8) -> u8) {
        let (w, h) = (i64::from(self.width), i64::from(self.height));
        let x0 = left.max(0);
        let x1 = (left + i64::from(tile.width)).min(w);
        if x0 >= x1 {
            return;
        }
        (top.max(0)..(top + i64::from(tile.height)).min(h)).for_each(|y| {
            let src_row = ((y - top) * i64::from(tile.width)) as usize;
            let dst_row = (y * w) as usize;
            let src = &tile.pixels[src_row + (x0 - left) as usize..src_row + (x1 - left) as usize];
            self.pixels[dst_row + x0 as usize..dst_row + x1 as usize]
                .iter_mut()
                .zip(src)
                .for_each(|(dst, &new)| *dst = combine(*dst, new));
        });
    }

    /// A `width`×`height` window of this image magnified `factor` times, starting
    /// `(left, top)` magnified pixels in, with bilinear smoothing. Edges are clamped.
    pub fn magnified(&self, factor: u32, left: i64, top: i64, width: u32, height: u32) -> Gray {
        let scale = f64::from(factor);
        // Source coordinate of an output pixel's center.
        let source = |out: u32, offset: i64, len: u32| {
            let u = ((f64::from(out) + offset as f64 + 0.5) / scale - 0.5)
                .clamp(0.0, f64::from(len - 1));
            let i = u.floor() as u32;
            (i, (i + 1).min(len - 1), u - f64::from(i))
        };
        let at = |x: u32, y: u32| f64::from(self.pixels[(y * self.width + x) as usize]);
        let pixels = (0..height)
            .flat_map(|y| (0..width).map(move |x| (x, y)))
            .map(|(x, y)| {
                let (x0, x1, fx) = source(x, left, self.width);
                let (y0, y1, fy) = source(y, top, self.height);
                let top_row = at(x0, y0) * (1.0 - fx) + at(x1, y0) * fx;
                let bottom_row = at(x0, y1) * (1.0 - fx) + at(x1, y1) * fx;
                (top_row * (1.0 - fy) + bottom_row * fy).round() as u8
            })
            .collect();
        Gray {
            width,
            height,
            pixels,
        }
    }

    pub fn encode_png(&self) -> anyhow::Result<Vec<u8>> {
        let mut out = Vec::new();
        let mut encoder = png::Encoder::new(&mut out, self.width, self.height);
        encoder.set_color(png::ColorType::Grayscale);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header()?;
        writer.write_image_data(&self.pixels)?;
        writer.finish()?;
        Ok(out)
    }
}

/// Straight (not premultiplied) RGBA, so palette lookups match exact source colors.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rgba {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<[u8; 4]>,
}

impl Rgba {
    pub fn decode_png(bytes: &[u8]) -> anyhow::Result<Self> {
        let mut decoder = png::Decoder::new(std::io::Cursor::new(bytes));
        decoder.set_transformations(png::Transformations::normalize_to_color8());
        let mut reader = decoder.read_info().context("reading PNG header")?;
        let mut buf = vec![0; reader.output_buffer_size().context("PNG too large")?];
        let info = reader.next_frame(&mut buf).context("decoding PNG")?;
        let data = &buf[..info.buffer_size()];
        let pixels = match info.color_type {
            png::ColorType::Rgba => data.as_chunks::<4>().0.to_vec(),
            png::ColorType::Rgb => data
                .as_chunks::<3>()
                .0
                .iter()
                .map(|&[r, g, b]| [r, g, b, 255])
                .collect(),
            png::ColorType::GrayscaleAlpha => data
                .as_chunks::<2>()
                .0
                .iter()
                .map(|&[g, a]| [g, g, g, a])
                .collect(),
            png::ColorType::Grayscale => data.iter().map(|&g| [g, g, g, 255]).collect(),
            png::ColorType::Indexed => bail!("palette was not expanded"),
        };
        Ok(Self {
            width: info.width,
            height: info.height,
            pixels,
        })
    }

    pub fn map_to_gray(&self, f: impl Fn([u8; 4]) -> u8) -> Gray {
        Gray {
            width: self.width,
            height: self.height,
            pixels: self.pixels.iter().map(|&p| f(p)).collect(),
        }
    }
}

/// `blit` combiner that overwrites the canvas.
pub fn replace(_canvas: u8, tile: u8) -> u8 {
    tile
}

/// Rec. 601 luma, composited over white.
pub fn luma_over_white([r, g, b, a]: [u8; 4]) -> u8 {
    let luma = (299 * u32::from(r) + 587 * u32::from(g) + 114 * u32::from(b)) / 1000;
    let alpha = u32::from(a);
    ((luma * alpha + 255 * (255 - alpha)) / 255) as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blit_clips_at_edges() {
        let mut canvas = Gray::filled(4, 3, 255);
        let tile = Gray::filled(3, 3, 0);
        canvas.blit(&tile, -1, 1, replace);
        assert_eq!(
            canvas.pixels,
            vec![255, 255, 255, 255, 0, 0, 255, 255, 0, 0, 255, 255]
        );
        canvas.blit(&tile, 10, 0, replace);
        canvas.blit(&tile, -10, 0, replace);
    }

    #[test]
    fn magnify_by_one_is_a_crop() {
        let image = Gray {
            width: 3,
            height: 2,
            pixels: vec![0, 10, 20, 30, 40, 50],
        };
        assert_eq!(image.magnified(1, 1, 0, 2, 2).pixels, vec![10, 20, 40, 50]);
    }

    #[test]
    fn magnify_interpolates_between_pixels() {
        let image = Gray {
            width: 2,
            height: 1,
            pixels: vec![0, 200],
        };
        let wide = image.magnified(2, 0, 0, 4, 1);
        assert_eq!(wide.pixels, vec![0, 50, 150, 200]);
    }

    #[test]
    fn png_round_trip() {
        let image = Gray {
            width: 2,
            height: 1,
            pixels: vec![0, 200],
        };
        let decoded = Rgba::decode_png(&image.encode_png().unwrap()).unwrap();
        assert_eq!(decoded.pixels, vec![[0, 0, 0, 255], [200, 200, 200, 255]]);
    }

    #[test]
    fn transparent_is_white() {
        assert_eq!(luma_over_white([0, 0, 0, 0]), 255);
        assert_eq!(luma_over_white([0, 0, 0, 255]), 0);
    }
}
