//! Headless rendering: the production window on a software framebuffer with
//! no devices, for previews and tests.
use crate::ui::{App, DashboardKind, Kindil, Page};
use slint::platform::{
    software_renderer::{MinimalSoftwareWindow, RepaintBufferType},
    Platform, WindowAdapter,
};
use slint::ComponentHandle;
use std::path::Path;
use std::rc::Rc;

pub const WIDTH: u32 = 758;
pub const HEIGHT: u32 = 1024;

struct Headless {
    window: Rc<MinimalSoftwareWindow>,
    start: std::time::Instant,
}

impl Platform for Headless {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, slint::PlatformError> {
        Ok(self.window.clone())
    }
    fn duration_since_start(&self) -> std::time::Duration {
        self.start.elapsed()
    }
}

pub fn install() -> Result<Rc<MinimalSoftwareWindow>, slint::PlatformError> {
    // A fresh buffer per frame: ReusedBuffer would leave unchanged regions
    // black in the first snapshot.
    let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
    slint::platform::set_platform(Box::new(Headless {
        window: window.clone(),
        start: std::time::Instant::now(),
    }))
    .map_err(|e| slint::PlatformError::Other(e.to_string()))?;
    window.set_size(slint::PhysicalSize::new(WIDTH, HEIGHT));
    Ok(window)
}

pub fn pixels(window: &MinimalSoftwareWindow) -> Vec<slint::Rgb8Pixel> {
    let mut pixels = vec![slint::Rgb8Pixel::default(); (WIDTH * HEIGHT) as usize];
    window.window().request_redraw();
    slint::platform::update_timers_and_animations();
    window.draw_if_needed(|renderer| {
        renderer.render(&mut pixels, WIDTH as usize);
    });
    pixels
}

/// Binary PPM: no image encoder in the binary just for previews.
pub fn snapshot(window: &MinimalSoftwareWindow, path: &Path) -> std::io::Result<()> {
    let header = format!("P6\n{WIDTH} {HEIGHT}\n255\n");
    let body = pixels(window).into_iter().flat_map(|p| [p.r, p.g, p.b]);
    std::fs::write(path, header.bytes().chain(body).collect::<Vec<u8>>())
}

fn pointer(window: &MinimalSoftwareWindow, event: slint::platform::WindowEvent) {
    window.window().dispatch_event(event);
    slint::platform::update_timers_and_animations();
}

/// The same pointer path the Kindle backend uses.
pub fn tap(window: &MinimalSoftwareWindow, x: f32, y: f32) {
    use slint::platform::{PointerEventButton, WindowEvent};
    let position = slint::LogicalPosition::new(x, y);
    pointer(
        window,
        WindowEvent::PointerPressed {
            position,
            button: PointerEventButton::Left,
        },
    );
    pointer(
        window,
        WindowEvent::PointerReleased {
            position,
            button: PointerEventButton::Left,
        },
    );
}

/// A vertical finger drag, for scrolling lists.
pub fn drag(window: &MinimalSoftwareWindow, x: f32, from: f32, to: f32) {
    use slint::platform::{PointerEventButton, WindowEvent};
    let at = |y| slint::LogicalPosition::new(x, y);
    pointer(
        window,
        WindowEvent::PointerPressed {
            position: at(from),
            button: PointerEventButton::Left,
        },
    );
    (1..=8).for_each(|step| {
        let y = from + (to - from) * step as f32 / 8.0;
        pointer(window, WindowEvent::PointerMoved { position: at(y) });
    });
    pointer(
        window,
        WindowEvent::PointerReleased {
            position: at(to),
            button: PointerEventButton::Left,
        },
    );
}

/// Every screen, in a fixed state, as `<dir>/<name>.ppm`.
pub fn screens(app: &App, window: &MinimalSoftwareWindow, dir: &Path) -> anyhow::Result<()> {
    std::fs::create_dir_all(dir)?;
    let k = app.window.global::<Kindil>();
    let shot = |name: &str| snapshot(window, &dir.join(format!("{name}.ppm")));
    k.set_clock("09:41".into());
    k.set_date("Thursday 12 November".into());

    k.set_dashboard(DashboardKind::Miqat);
    k.invoke_go(Page::Dashboard);
    shot("dashboard-miqat")?;
    k.set_chrome_visible(true);
    shot("dashboard-miqat-tabs")?;
    k.set_dashboard(DashboardKind::Clock);
    k.invoke_go(Page::Dashboard);
    shot("dashboard-clock")?;

    k.invoke_go(Page::Library);
    shot("library")?;
    if slint::Model::row_count(&k.get_books()) > 0 {
        k.set_selected_book(0);
        shot("library-actions")?;
        k.set_selected_book(-1);
        k.invoke_open_book(0);
        k.invoke_next_page();
        shot("reader")?;
        k.set_reader_options(true);
        shot("reader-options")?;
        k.set_reader_options(false);
    }

    k.invoke_go(Page::Settings);
    shot("settings")?;
    drag(window, 380.0, 900.0, 200.0);
    drag(window, 380.0, 900.0, 200.0);
    shot("settings-device")?;

    k.invoke_go(Page::Networks);
    k.invoke_scan_networks();
    shot("networks")?;
    k.set_join_ssid("Home".into());
    k.set_join_open(true);
    shot("networks-join")?;
    k.set_join_open(false);
    k.invoke_go(Page::Dashboard);
    Ok(())
}
