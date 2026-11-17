//! The window rendered offscreen, on a software framebuffer, for a Kindle
//! that is ready and for none: the checklist and buttons follow the state.
//! Writes the ready window to `installer.png` in Cargo's test folder, or to
//! `$KINDIL_INSTALLER_SCREENSHOT`.
use kindil_installer::detect;
use kindil_installer_gui::{render, scanned, InstallerWindow, State};
use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType};
use slint::platform::{Platform, WindowAdapter};
use slint::ComponentHandle;
use std::rc::Rc;

const WIDTH: u32 = 640;
const HEIGHT: u32 = 900;

struct Headless(Rc<MinimalSoftwareWindow>);

impl Platform for Headless {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, slint::PlatformError> {
        Ok(self.0.clone())
    }
}

fn pixels(window: &MinimalSoftwareWindow) -> Vec<slint::Rgb8Pixel> {
    let mut pixels = vec![slint::Rgb8Pixel::default(); (WIDTH * HEIGHT) as usize];
    window.window().request_redraw();
    slint::platform::update_timers_and_animations();
    window.draw_if_needed(|renderer| {
        renderer.render(&mut pixels, WIDTH as usize);
    });
    pixels
}

fn save(pixels: &[slint::Rgb8Pixel], path: &std::path::Path) {
    let file = std::fs::File::create(path).unwrap();
    let mut encoder = png::Encoder::new(std::io::BufWriter::new(file), WIDTH, HEIGHT);
    encoder.set_color(png::ColorType::Rgb);
    encoder.set_depth(png::BitDepth::Eight);
    let data: Vec<u8> = pixels.iter().flat_map(|p| [p.r, p.g, p.b]).collect();
    encoder
        .write_header()
        .unwrap()
        .write_image_data(&data)
        .unwrap();
}

/// A jailbroken PW1 with KUAL, kindil 0.13.2 and its settings on it.
fn kindle() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let put = |path: &str, body: &[u8]| {
        let path = dir.path().join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
    };
    put("system/version.txt", b"Kindle 5.6.1.1 (2689890035)\n");
    put("documents/KUAL.kual", b"");
    put("extensions/MRInstaller/config.xml", b"");
    put("extensions/kindil/menu.json", b"{}");
    put(
        "extensions/kindil/bin/kindil",
        b"ELF kindil-version=0.13.2\n",
    );
    put("kindil/miqat.toml", b"[location]\n");
    dir
}

#[test]
fn renders_the_steps() {
    let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
    slint::platform::set_platform(Box::new(Headless(window.clone()))).unwrap();
    window.set_size(slint::PhysicalSize::new(WIDTH, HEIGHT));
    let ui = InstallerWindow::new().unwrap();
    ui.show().unwrap();

    // Nothing plugged in: nothing to install.
    let mut state = State {
        latest: Some(Ok("0.13.3".into())),
        ..State::default()
    };
    render(&ui, &state);
    assert!(!ui.get_found());
    assert!(!ui.get_can_install());
    assert_eq!(ui.get_kindle_status(), "Looking for a Kindle…");
    let empty = pixels(&window);

    let dir = kindle();
    let mut found = detect::inspect(dir.path(), Some("B024123456789ABC"));
    // Where macOS mounts it, for the screenshot.
    found.volume = "/Volumes/Kindle".into();
    scanned(&mut state, vec![found]);
    render(&ui, &state);
    assert!(ui.get_found());
    assert!(ui.get_ready());
    assert!(ui.get_can_install());
    assert_eq!(ui.get_install_label(), "Upgrade kindil 0.13.2 → 0.13.3");
    let ready = pixels(&window);
    assert_ne!(ready, empty, "the found Kindle and its checklist show");
    let ink = ready.iter().filter(|p| p.r < 64).count();
    assert!(
        ink > 5_000,
        "text and the lantern are drawn ({ink} dark pixels)"
    );
    let path = std::env::var_os("KINDIL_INSTALLER_SCREENSHOT")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("installer.png"));
    save(&ready, &path);
    println!("screenshot: {}", path.display());

    // The model unknown: installing needs the owner's word.
    scanned(&mut state, vec![detect::inspect(dir.path(), None)]);
    render(&ui, &state);
    assert!(!ui.get_ready());
    assert!(ui.get_needs_vouch());
    assert!(!ui.get_can_install());
    ui.set_vouched(true);
    assert!(ui.get_can_install());

    // Unplugged after an install: the outcome stays until the next Kindle.
    state.done = true;
    scanned(&mut state, vec![]);
    render(&ui, &state);
    assert_eq!(ui.get_kindle_status(), "The Kindle is unplugged.");
    assert!(ui.get_done());
    scanned(&mut state, vec![detect::inspect(dir.path(), Some("B024"))]);
    assert!(!state.done, "a Kindle plugged in after starts over");
}
