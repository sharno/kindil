//! kindil: a single-process shell for the first-generation Kindle Paperwhite.
//!
//! The program owns the e-ink panel (via the EPDC framebuffer driver) and the
//! touch controller, and renders a clock and a small menu. There is no init, no
//! window manager and no compositor.
//!
//! It runs either:
//!
//! * as a KUAL extension, alongside Amazon's UI (see `scripts/make-kual-extension.sh`),
//! * as PID 1 in a custom initramfs, with `--init`
//!   (see `scripts/make-initramfs.sh`), or
//! * on a workstation with `--sim`, which renders the screens to PGM files.

use kindil::boot;
use kindil::civil::DateTime;
use kindil::frontlight::Frontlight;
use kindil::input::Touch;
use kindil::panel::{DevicePanel, Panel};
use kindil::shell::Shell;
use kindil::sim;
use kindil::time::Clock;
use std::path::PathBuf;
use std::time::Duration;

/// The default framebuffer device.
const DEFAULT_FB: &str = "/dev/fb0";

fn main() {
    let mut fb_path = DEFAULT_FB.to_string();
    let mut touch_path: Option<String> = None;
    let mut sim_dir: Option<PathBuf> = None;
    let mut init = false;

    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--fb" => fb_path = value(&mut args, "--fb"),
            "--touch" => touch_path = Some(value(&mut args, "--touch")),
            "--sim" => sim_dir = Some(PathBuf::from(value(&mut args, "--sim"))),
            "--init" => init = true,
            "--help" | "-h" => {
                print_help();
                return;
            }
            "--version" => {
                println!("kindil {}", env!("CARGO_PKG_VERSION"));
                return;
            }
            other => fail(&format!(
                "unknown argument: {other}\n\nTry `kindil --help`."
            )),
        }
    }

    if let Some(dir) = sim_dir {
        if let Err(e) = sim::run(&dir) {
            fail(&format!("simulator: {e}"));
        }
        return;
    }

    let is_pid1 = unsafe { kindil::sys::getpid() } == 1;
    if init || is_pid1 {
        // PID 1 has no udev behind it, so /dev may not have the nodes we need.
        boot::prepare();
    }
    run_device(&fb_path, touch_path.as_deref());
}

fn value(args: &mut impl Iterator<Item = String>, flag: &str) -> String {
    match args.next() {
        Some(v) => v,
        None => fail(&format!("{flag} requires a path")),
    }
}

fn print_help() {
    println!(
        "kindil {} - a shell for the first-generation Kindle Paperwhite\n\
         \n\
         Usage:\n  \
           kindil [--fb /dev/fb0] [--touch /dev/input/event0]\n  \
           kindil --init [--fb /dev/fb0]\n  \
           kindil --sim OUTDIR\n\
         \n\
         Options:\n  \
           --fb PATH      framebuffer device (default /dev/fb0)\n  \
           --touch PATH   touch device (default: scan /dev/input/event*)\n  \
           --init         mount devtmpfs and create device nodes first (PID 1)\n  \
           --sim DIR      render a scripted tour to DIR as PGM files\n  \
           --help         show this text\n  \
           --version      show the version",
        env!("CARGO_PKG_VERSION")
    );
}

/// The running shell plus its panel.
struct App<P: Panel> {
    shell: Shell,
    panel: P,
    clock: Clock,
    last: DateTime,
}

impl<P: Panel> App<P> {
    fn new(panel: P) -> Self {
        let shell = Shell::new(panel.width(), panel.height());
        App {
            shell,
            panel,
            clock: Clock::system(),
            last: DateTime::default(),
        }
    }

    /// Redraw and present the current screen.
    fn redraw(&mut self) {
        let now = self.clock.now();
        self.shell.draw(&now);
        if let Err(e) = self.shell.present(&mut self.panel) {
            fail(&format!("present: {e}"));
        }
        self.last = now;
    }

    /// Redraw if the displayed minute changed.
    fn tick(&mut self) {
        let now = self.clock.now();
        if (now.hour, now.minute, now.day) != (self.last.hour, self.last.minute, self.last.day) {
            self.redraw();
        }
    }
}

fn run_device(fb_path: &str, touch_path: Option<&str>) -> ! {
    let panel = match DevicePanel::open(fb_path) {
        Ok(p) => p,
        Err(e) => fail(&format!("{e}")),
    };
    let (width, height) = (panel.width(), panel.height());
    eprintln!(
        "kindil {}: panel {width}x{height}",
        env!("CARGO_PKG_VERSION")
    );

    let mut app = App::new(panel);

    // The frontlight state is only used by the Settings screen; failing to read
    // it must not stop the shell.
    if let Ok(frontlight) = Frontlight::open() {
        if let (Ok(current), Ok(max)) = (frontlight.intensity(), frontlight.max_intensity()) {
            app.shell.set_frontlight(Some((current, max)));
        }
    }

    let mut touch = match touch_path {
        Some(path) => match Touch::open(path) {
            Ok(t) => Some(t),
            Err(e) => {
                eprintln!("kindil: touch unavailable ({e}); running clock-only");
                None
            }
        },
        None => match Touch::open_default() {
            Ok(t) => {
                eprintln!(
                    "kindil: touch {} ({})",
                    t.name().unwrap_or_else(|| "unknown".into()),
                    t.fd()
                );
                Some(t)
            }
            Err(e) => {
                eprintln!("kindil: touch unavailable ({e}); running clock-only");
                None
            }
        },
    };

    app.redraw();

    loop {
        app.tick();

        // Wake up when the minute rolls over, so the clock does not refresh the
        // panel sixty times a minute.
        let now = app.clock.now();
        let wait_ms = ((60 - now.second as i32) * 1000).clamp(50, 60_000);

        match touch.as_mut() {
            Some(device) => match device.read_event(wait_ms) {
                Ok(Some(event)) => {
                    if app.shell.handle_touch(event) {
                        app.redraw();
                    }
                }
                Ok(None) => {}
                Err(e) => {
                    eprintln!("kindil: touch error: {e}");
                    touch = None;
                }
            },
            None => std::thread::sleep(Duration::from_millis(wait_ms as u64)),
        }
    }
}

/// Print an error and exit.
fn fail(msg: &str) -> ! {
    eprintln!("kindil: {msg}");
    std::process::exit(1)
}
