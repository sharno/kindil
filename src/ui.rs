//! One Slint component for desktop, deterministic software previews and Kindle.
use crate::{
    civil::civil_from_unix,
    config::Config,
    reader::{self, Reader},
};
use slint::{ComponentHandle, ModelRc, VecModel};
use std::{cell::RefCell, path::Path, rc::Rc};
slint::include_modules!();

pub struct App {
    pub window: AppWindow,
    _clock: slint::Timer,
    brightness_timer: Rc<slint::Timer>,
}

fn strings(values: Vec<String>) -> ModelRc<slint::SharedString> {
    Rc::new(VecModel::from(
        values.into_iter().map(Into::into).collect::<Vec<_>>(),
    ))
    .into()
}

pub fn refresh_at(ui: &AppWindow, config: &Config, unix: i64) {
    let tz = config.tz();
    let offset = tz.offset_at(unix);
    let local = civil_from_unix(unix + i64::from(offset));
    ui.set_clock(format!("{:02}:{:02}", local.hour, local.minute).into());
    ui.set_date(format!("{:04}-{:02}-{:02}", local.year, local.month, local.day).into());
}

pub fn create(config: Config, book_dir: &Path) -> Result<App, slint::PlatformError> {
    let window = AppWindow::new()?;
    // Reuse Slint's MIT virtual-keyboard example and route its keys through the
    // normal input pipeline. Slint owns selection, UTF-8, password masking and
    // cursor editing; no second text editor or input-method implementation.
    let weak = window.as_weak();
    window
        .global::<VirtualKeyboardHandler>()
        .on_key_pressed(move |key| {
            if let Some(ui) = weak.upgrade() {
                if key == slint::SharedString::from(slint::platform::Key::Escape) {
                    ui.invoke_dismiss_keyboard();
                    return;
                }
                ui.window()
                    .dispatch_event(slint::platform::WindowEvent::KeyPressed { text: key.clone() });
                ui.window()
                    .dispatch_event(slint::platform::WindowEvent::KeyReleased { text: key });
            }
        });
    refresh_at(&window, &config, crate::time::unix_seconds());
    window.set_status("Preview / no hardware".into());
    window.set_dashboard_mode(if config.dashboard == crate::config::Dashboard::Miqat {
        0
    } else {
        1
    });
    window.set_settings(format!("kindil {}\nSlint software renderer\n\nMiqat renderer: same source, fonts and contrast as the existing project.\n\nNo flashing or bootloader writes.\nStandalone boot is experimental.\nSSH: keys only, standalone mode only.\nDeep suspend is not enabled.",env!("CARGO_PKG_VERSION")).into());
    let books = Rc::new(RefCell::new(reader::library(book_dir)));
    window.set_books(strings(
        books
            .borrow()
            .iter()
            .map(|p| {
                p.file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect(),
    ));
    let reader = Rc::new(RefCell::new(None::<Reader>));
    install_file_actions(&window, book_dir.to_owned(), books.clone());
    let font_reader = reader.clone();
    let weak = window.as_weak();
    window.on_font_size_changed(move |font| {
        if let (Some(ui), Some(reader)) = (weak.upgrade(), font_reader.borrow_mut().as_mut()) {
            reader.resize_font(font.round().clamp(16.0, 40.0) as u32);
            show_page(&ui, reader);
        }
    });
    let weak = window.as_weak();
    window.on_brightness_changed(move |percent| {
        if let Some(ui) = weak.upgrade() {
            ui.set_brightness_status(
                format!("Preview brightness: {percent:.0}% (no device writes)").into(),
            );
        }
    });
    let weak = window.as_weak();
    let book_state = reader.clone();
    window.on_open_book(move |index| {
        let Some(ui) = weak.upgrade() else {
            return;
        };
        let Some(path) = usize::try_from(index)
            .ok()
            .and_then(|i| books.borrow().get(i).cloned())
        else {
            return;
        };
        match Reader::open(&path, 48, 23) {
            Ok(mut book) => {
                book.resize_font(ui.get_reading_font_size().round().clamp(16.0, 40.0) as u32);
                show_page(&ui, &book);
                *book_state.borrow_mut() = Some(book);
                ui.set_page(2);
            }
            Err(error) => {
                ui.set_book_title("Cannot open book".into());
                ui.set_book_text(error.to_string().into());
                ui.set_page(2);
            }
        }
    });
    let weak = window.as_weak();
    window.on_turn_page(move |forward| {
        if let (Some(ui), Some(reader)) = (weak.upgrade(), reader.borrow_mut().as_mut()) {
            reader.turn(forward);
            show_page(&ui, reader);
        }
    });
    // Network operations run off the render/event thread. A timeout must never
    // freeze an e-ink tap or prevent navigation back to the dashboard.
    let interface = config.interface.clone();
    let weak = window.as_weak();
    window.on_scan_wifi(move || {
        let Some(ui) = weak.upgrade() else {
            return;
        };
        if ui.get_network_busy() {
            return;
        }
        ui.set_network_busy(true);
        ui.set_network_status("Searching…".into());
        let weak = weak.clone();
        let interface = interface.clone();
        std::thread::spawn(move || {
            let result = crate::wifi::open(&interface).and_then(|wpa| {
                wpa.command("SCAN")?;
                std::thread::sleep(std::time::Duration::from_secs(2));
                wpa.scan_results()
            });
            let _ = weak.upgrade_in_event_loop(move |ui| {
                ui.set_network_busy(false);
                match result {
                    Ok(networks) => {
                        ui.set_network_status(format!("{} networks found", networks.len()).into());
                        ui.set_networks(strings(networks.into_iter().map(|n| n.ssid).collect()));
                    }
                    Err(error) => ui.set_network_status(error.to_string().into()),
                }
            });
        });
    });
    let weak = window.as_weak();
    let interface = config.interface.clone();
    window.on_connect_wifi(move |ssid, passphrase| {
        let Some(ui) = weak.upgrade() else {
            return;
        };
        if ui.get_network_busy() {
            return;
        }
        ui.set_network_busy(true);
        ui.set_network_status("Connecting…".into());
        let weak = weak.clone();
        let interface = interface.clone();
        std::thread::spawn(move || {
            let result = crate::wifi::open(&interface)
                .and_then(|wpa| wpa.connect_network(&ssid, &passphrase));
            let _ = weak.upgrade_in_event_loop(move |ui| {
                ui.set_network_busy(false);
                ui.set_network_status(match result {
                    Ok(()) => "Connection requested; DHCP runs in standalone mode".into(),
                    Err(error) => error.to_string().into(),
                })
            });
        });
    });
    let weak = window.as_weak();
    let clock = slint::Timer::default();
    clock.start(
        slint::TimerMode::Repeated,
        std::time::Duration::from_secs(60),
        move || {
            if let Some(ui) = weak.upgrade() {
                refresh_at(&ui, &config, crate::time::unix_seconds());
            }
        },
    );
    Ok(App {
        window,
        _clock: clock,
        brightness_timer: Rc::new(slint::Timer::default()),
    })
}

impl App {
    pub fn monitor_wifi(&self, interface: String) {
        let weak = self.window.as_weak();
        std::thread::spawn(move || loop {
            let status = crate::wifi::open(&interface)
                .and_then(|wpa| wpa.status())
                .map(|status| status.label())
                .unwrap_or_else(|_| "Wi-Fi unavailable".into());
            if weak
                .upgrade_in_event_loop(move |ui| ui.set_wifi_state(status.into()))
                .is_err()
            {
                break;
            }
            std::thread::sleep(std::time::Duration::from_secs(15));
        });
    }
    pub fn set_miqat(&self, gray: miqat::image::Gray) {
        set_miqat(&self.window, gray);
    }
    /// One worker owns cached weather/radar sources. Nothing blocks the UI.
    pub fn start_miqat(&self, path: std::path::PathBuf) {
        let weak = self.window.as_weak();
        std::thread::spawn(move || {
            let config = match miqat::config::Config::load(&path) {
                Ok(config) => config,
                Err(error) => {
                    let _ = weak.upgrade_in_event_loop(move |ui| {
                        ui.set_miqat_error(error.to_string().into())
                    });
                    return;
                }
            };
            let mut sources = miqat::sources::Sources::new(&config);
            let renderer = miqat::render::Renderer::new(config.display.contrast);
            // First paint is offline-capable; network timeouts must not keep
            // the prayer/date dashboard blank after startup.
            if let Ok(gray) =
                crate::dashboards::render(&renderer, &config, chrono::Utc::now(), &sources)
            {
                let _ = weak.upgrade_in_event_loop(move |ui| set_miqat(&ui, gray));
            }
            loop {
                let now = chrono::Utc::now();
                sources.refresh(&config, now);
                let frame = crate::dashboards::render(&renderer, &config, now, &sources)
                    .map_err(|e| e.to_string());
                if weak
                    .upgrade_in_event_loop(move |ui| match frame {
                        Ok(gray) => set_miqat(&ui, gray),
                        Err(error) => ui.set_miqat_error(error.into()),
                    })
                    .is_err()
                {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_secs(60));
            }
        });
    }
    /// Called only after selecting the physical Kindle backend, never in preview.
    pub fn enable_frontlight(&self) {
        match crate::frontlight::Frontlight::open()
            .and_then(|light| light.percent().map(|percent| (light, percent)))
        {
            Ok((light, percent)) => {
                self.window.set_brightness(percent);
                self.window.set_brightness_status("PW1 frontlight".into());
                let light = Rc::new(light);
                let timer = self.brightness_timer.clone();
                let weak = self.window.as_weak();
                self.window.on_brightness_changed(move |percent| {
                    let light = light.clone();
                    let weak = weak.clone();
                    timer.start(
                        slint::TimerMode::SingleShot,
                        std::time::Duration::from_millis(120),
                        move || {
                            if let Err(error) = light.set_percent(percent) {
                                if let Some(ui) = weak.upgrade() {
                                    ui.set_brightness_status(error.to_string().into());
                                    ui.set_brightness_enabled(false);
                                }
                            }
                        },
                    );
                });
            }
            Err(error) => {
                self.window.set_brightness_enabled(false);
                self.window
                    .set_brightness_status(format!("Frontlight unavailable: {error}").into());
            }
        }
    }
}

fn set_miqat(ui: &AppWindow, gray: miqat::image::Gray) {
    let mut buffer = slint::SharedPixelBuffer::<slint::Rgb8Pixel>::new(gray.width, gray.height);
    for (dst, gray) in buffer.make_mut_slice().iter_mut().zip(gray.pixels) {
        *dst = slint::Rgb8Pixel {
            r: gray,
            g: gray,
            b: gray,
        };
    }
    ui.set_miqat_frame(slint::Image::from_rgb8(buffer));
}
fn show_page(ui: &AppWindow, reader: &Reader) {
    ui.set_book_title(reader.title.clone().into());
    ui.set_book_text(reader.visible().join("\n").into());
    ui.set_book_page(format!("{} / {}", reader.page() + 1, reader.pages()).into());
}

fn install_file_actions(
    ui: &AppWindow,
    root: std::path::PathBuf,
    books: Rc<RefCell<Vec<std::path::PathBuf>>>,
) {
    fn filename(books: &RefCell<Vec<std::path::PathBuf>>, index: i32) -> std::io::Result<String> {
        usize::try_from(index)
            .ok()
            .and_then(|i| books.borrow().get(i).cloned())
            .and_then(|p| p.file_name().map(|s| s.to_string_lossy().into_owned()))
            .ok_or_else(|| std::io::Error::other("select a file"))
    }
    fn refresh(ui: &AppWindow, root: &Path, books: &RefCell<Vec<std::path::PathBuf>>) {
        let paths = reader::library(root);
        ui.set_books(strings(
            paths
                .iter()
                .map(|p| {
                    p.file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .into_owned()
                })
                .collect(),
        ));
        *books.borrow_mut() = paths;
        ui.set_selected_file(-1);
    }
    let directory = root.clone();
    let paths = books.clone();
    let weak = ui.as_weak();
    ui.on_rename_file(move |index, new| {
        if let Some(ui) = weak.upgrade() {
            let result = (|| {
                let old = filename(&paths, index)?;
                crate::files::Files::new(&directory)?.rename(&old, &new)
            })();
            ui.set_file_status(match result {
                Ok(()) => "Renamed".into(),
                Err(e) => e.to_string().into(),
            });
            refresh(&ui, &directory, &paths);
        }
    });
    let last = Rc::new(RefCell::new(None::<(std::path::PathBuf, String)>));
    let directory = root.clone();
    let paths = books.clone();
    let weak = ui.as_weak();
    let trash = last.clone();
    ui.on_trash_file(move |index| {
        if let Some(ui) = weak.upgrade() {
            let result = (|| {
                let name = filename(&paths, index)?;
                let path = crate::files::Files::new(&directory)?.trash(&name)?;
                Ok::<_, std::io::Error>((path, name))
            })();
            match result {
                Ok(value) => {
                    *trash.borrow_mut() = Some(value);
                    ui.set_can_undo_trash(true);
                    ui.set_file_status("Moved to .kindil-trash; bytes retained".into());
                }
                Err(e) => ui.set_file_status(e.to_string().into()),
            }
            refresh(&ui, &directory, &paths);
        }
    });
    let weak = ui.as_weak();
    ui.on_undo_trash(move || {
        if let Some(ui) = weak.upgrade() {
            if let Some((path, name)) = last.borrow().as_ref() {
                match crate::files::Files::new(&root).and_then(|files| files.restore(path, name)) {
                    Ok(()) => {
                        ui.set_can_undo_trash(false);
                        ui.set_file_status("Restored".into());
                    }
                    Err(e) => ui.set_file_status(e.to_string().into()),
                }
            }
            refresh(&ui, &root, &books);
        }
    });
}

pub mod preview {
    use super::*;
    use slint::platform::{
        software_renderer::{MinimalSoftwareWindow, RepaintBufferType},
        Platform, WindowAdapter,
    };
    pub struct Headless {
        pub window: Rc<MinimalSoftwareWindow>,
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
        // Each snapshot allocates a fresh pixel vector. ReusedBuffer would
        // leave unchanged regions black/stale after the first screenshot.
        let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
        slint::platform::set_platform(Box::new(Headless {
            window: window.clone(),
            start: std::time::Instant::now(),
        }))
        .map_err(|e| slint::PlatformError::Other(e.to_string()))?;
        window.set_size(slint::PhysicalSize::new(758, 1024));
        Ok(window)
    }
    pub fn snapshot(window: &MinimalSoftwareWindow, path: &Path) -> std::io::Result<()> {
        use std::io::Write;
        let pixels = pixels(window);
        let mut file = std::io::BufWriter::new(std::fs::File::create(path)?);
        writeln!(file, "P6\n758 1024\n255")?;
        for pixel in pixels {
            file.write_all(&[pixel.r, pixel.g, pixel.b])?;
        }
        file.flush()
    }
    pub fn pixels(window: &MinimalSoftwareWindow) -> Vec<slint::Rgb8Pixel> {
        let mut pixels = vec![slint::Rgb8Pixel::default(); 758 * 1024];
        window.window().request_redraw();
        slint::platform::update_timers_and_animations();
        window.draw_if_needed(|renderer| {
            renderer.render(&mut pixels, 758);
        });
        pixels
    }
    /// Exercise the same pointer path the Kindle backend uses, without devices.
    pub fn tap(window: &MinimalSoftwareWindow, x: f32, y: f32) {
        use slint::platform::{PointerEventButton, WindowEvent};
        let position = slint::LogicalPosition::new(x, y);
        window.window().dispatch_event(WindowEvent::PointerPressed {
            position,
            button: PointerEventButton::Left,
        });
        window
            .window()
            .dispatch_event(WindowEvent::PointerReleased {
                position,
                button: PointerEventButton::Left,
            });
        slint::platform::update_timers_and_animations();
    }
}
