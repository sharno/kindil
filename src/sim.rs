//! The host simulator: a panel that lives in memory and can be written out.
//!
//! This is what makes the shell testable and viewable without a Kindle. It runs
//! the real [`Shell`] with the real renderer, and dumps each frame as a PGM
//! (`P5`) that any image viewer or `ffmpeg` can open.

use crate::canvas::{Canvas, Rect};
use crate::civil::DateTime;
use crate::input::TouchEvent;
use crate::panel::{Panel, PresentError};
use crate::shell::Shell;
use std::io::Write;
use std::path::Path;

/// A panel that keeps the last frame in memory.
pub struct SimPanel {
    width: u32,
    height: u32,
    pixels: Vec<u8>,
}

impl SimPanel {
    /// A white panel of the given size.
    pub fn new(width: u32, height: u32) -> Self {
        SimPanel {
            width,
            height,
            pixels: vec![crate::render::WHITE; (width as usize) * (height as usize)],
        }
    }

    /// The current image.
    pub fn pixels(&self) -> &[u8] {
        &self.pixels
    }

    /// Write the image as a binary PGM (`P5`).
    pub fn write_pgm(&self, path: &Path) -> std::io::Result<()> {
        let mut file = std::fs::File::create(path)?;
        write!(file, "P5\n{} {}\n255\n", self.width, self.height)?;
        file.write_all(&self.pixels)
    }
}

impl Panel for SimPanel {
    fn width(&self) -> u32 {
        self.width
    }

    fn height(&self) -> u32 {
        self.height
    }

    fn present(&mut self, canvas: &Canvas, rect: Rect, _full: bool) -> Result<(), PresentError> {
        let rect = rect.clip(self.width, self.height);
        for y in rect.y..rect.bottom() {
            let start = (y as usize) * (self.width as usize) + rect.x as usize;
            let end = start + rect.w as usize;
            self.pixels[start..end]
                .copy_from_slice(&canvas.row(y)[rect.x as usize..rect.right() as usize]);
        }
        Ok(())
    }
}

/// Render a scripted tour of the shell into `dir` as PGM files.
pub fn run(dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;

    let mut shell = Shell::new(758, 1024);
    let mut panel = SimPanel::new(758, 1024);
    let now = DateTime {
        year: 2026,
        month: 11,
        day: 11,
        hour: 22,
        minute: 30,
        second: 0,
        weekday: 3,
    };

    let mut frame = 0;
    let mut shot = |shell: &mut Shell, panel: &mut SimPanel, name: &str| -> std::io::Result<()> {
        frame += 1;
        shell.draw(&now);
        shell
            .present(panel)
            .map_err(|e| std::io::Error::other(e.to_string()))?;
        let path = dir.join(std::format!("{frame}-{name}.pgm"));
        panel.write_pgm(&path)?;
        println!("{}", path.display());
        Ok(())
    };

    shot(&mut shell, &mut panel, "home")?;

    shell.handle_touch(TouchEvent::Release { x: 300, y: 500 });
    shot(&mut shell, &mut panel, "menu")?;

    let layout = shell.layout();
    shell.handle_touch(TouchEvent::Release {
        x: 300,
        y: (layout.list_top + layout.row_height * 2 + 5) as i32,
    });
    shot(&mut shell, &mut panel, "settings")?;

    shell.handle_touch(TouchEvent::Release { x: 300, y: 100 });
    let layout = shell.layout();
    shell.handle_touch(TouchEvent::Release {
        x: 300,
        y: (layout.list_top + layout.row_height * 3 + 5) as i32,
    });
    shot(&mut shell, &mut panel, "about")?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn present_copies_only_the_rectangle() {
        let mut panel = SimPanel::new(8, 4);
        let mut canvas = Canvas::new(8, 4);
        canvas.fill_rect(2, 1, 2, 2, crate::render::BLACK);
        panel
            .present(&canvas, Rect::new(2, 1, 2, 2), false)
            .unwrap();
        // Pixel (2,1) is black, (1,1) is still white.
        assert_eq!(panel.pixels()[8 + 2], crate::render::BLACK);
        assert_eq!(panel.pixels()[8 + 1], crate::render::WHITE);
    }
}
