//! kindil: a single-process shell for the first-generation Kindle Paperwhite.
//!
//! The program owns the e-ink panel (via the EPDC framebuffer driver) and
//! the touch controller, and renders a small clock/menu shell. It is meant to
//! run either:
//!
//! * as a KUAL extension alongside Amazon's own UI, or
//! * as PID 1 in a custom initramfs, replacing the Kindle OS entirely.
//!
//! # Usage
//!
//! ```text
//! kindil [--fb /dev/fb0] [--touch /dev/input/event0]
//! ```

use kindil::epdc::Epdc;
use kindil::fb::Framebuffer;
use kindil::input::Touch;
use kindil::shell::Shell;

/// How long to wait for touch events before redrawing the clock.
const TICK_MS: i32 = 1_000;

/// The default touch device on the Paperwhite 1.
const DEFAULT_TOUCH: &str = "/dev/input/event0";

/// The default framebuffer device.
const DEFAULT_FB: &str = "/dev/fb0";

fn main() {
    let mut fb_path = DEFAULT_FB.to_string();
    let mut touch_path = DEFAULT_TOUCH.to_string();

    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--fb" => match args.next() {
                Some(v) => fb_path = v,
                None => fail("--fb requires a path"),
            },
            "--touch" => match args.next() {
                Some(v) => touch_path = v,
                None => fail("--touch requires a path"),
            },
            "--help" | "-h" => {
                println!(
                    "kindil - a shell for the first-generation Kindle Paperwhite\n\n\
                     Usage: kindil [--fb /dev/fb0] [--touch /dev/input/event0]"
                );
                return;
            }
            other => fail(&format!("unknown argument: {other}")),
        }
    }

    let epdc = match Epdc::open(&fb_path) {
        Ok(e) => e,
        Err(e) => fail(&format!("{e}")),
    };
    let framebuffer = match Framebuffer::map(&epdc) {
        Ok(f) => f,
        Err(e) => fail(&format!("{e}")),
    };

    let mut shell = Shell::new(epdc, framebuffer);
    shell.draw();

    // The touch device is optional: without it we still run, and just tick
    // the clock. That makes the shell usable over a serial console or as a
    // "signage" display.
    let mut touch = match Touch::open(&touch_path) {
        Ok(t) => Some(t),
        Err(e) => {
            eprintln!("kindil: touch unavailable ({e}); running clock-only");
            None
        }
    };

    loop {
        if let Some(t) = touch.as_mut() {
            match t.read_event(TICK_MS) {
                Ok(Some(event)) => {
                    // A touch may have changed the screen; redraw now rather
                    // than waiting for the next tick so the response feels
                    // immediate.
                    if shell.handle_touch(event) {
                        shell.draw();
                        continue;
                    }
                }
                Ok(None) => {}
                Err(e) => {
                    eprintln!("kindil: touch error: {e}");
                    touch = None;
                }
            }
        } else {
            std::thread::sleep(std::time::Duration::from_millis(TICK_MS as u64));
        }

        // Tick: the clock has moved on, so redraw.
        shell.draw();
    }
}

/// Print an error and exit.
fn fail(msg: &str) -> ! {
    eprintln!("kindil: {msg}");
    std::process::exit(1)
}
