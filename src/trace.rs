//! A record of the system state machine (src/system.rs) for the formal
//! model of it, verify/Sleep.tla: one JSON line per event the app handles
//! (`Ctx::device`), in the shape `everystate conform` reads. The action is
//! the event, named as the model's action; the state is the mode after the
//! step, whether it carries a light level, its effects, why it was ignored,
//! and the count of events so far, modulo 4 (the checker's unlogged steps
//! then can't be events). `cargo xtask verify` checks the traces the tests record against
//! the model, so the model can't drift from the code (docs/DEVELOP.md,
//! "Formal models").
//!
//! Off unless `/var/local/kindil/trace` exists when the first event comes
//! (then written to `/var/local/kindil/trace.jsonl`, afresh each start, up
//! to 1 MiB), or
//! `KINDIL_TRACE` names a directory: then each thread writes a file of its
//! own there, named after it (a test's thread is named after the test).
//! Off, it costs one load of a set-once flag per event.
// Variants are matched by name, `{ .. }` even where they carry nothing yet:
// a field added to a mode or an effect leaves this module as it is.
#![allow(clippy::unneeded_struct_pattern)]
use crate::system::{Doze, Drive, Effect, Event, Ignored, Mode, Sleep, Step};
use std::cell::{Cell, RefCell};
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

/// Present: the device records its trace.
pub const SWITCH: &str = "/var/local/kindil/trace";
/// Where the device records it.
pub const FILE: &str = "/var/local/kindil/trace.jsonl";
/// The device stops recording once the trace is this long.
const MAX_BYTES: u64 = 1 << 20;

enum Sink {
    Off,
    Device,
    Dir(PathBuf),
}

fn sink() -> &'static Sink {
    static SINK: OnceLock<Sink> = OnceLock::new();
    SINK.get_or_init(|| match std::env::var_os("KINDIL_TRACE") {
        Some(dir) => Sink::Dir(dir.into()),
        None if std::path::Path::new(SWITCH).exists() => Sink::Device,
        None => Sink::Off,
    })
}

/// Records `mode` handling `event` with this result, if recording, and
/// hands the result back: `match trace::step(mode, event, mode.step(..))`.
pub fn step(mode: Mode, event: Event, result: Result<Step, Ignored>) -> Result<Step, Ignored> {
    match sink() {
        Sink::Off => {}
        Sink::Device => device(mode, event, &result),
        Sink::Dir(dir) => per_thread(dir, mode, event, &result),
    }
    result
}

fn device(mode: Mode, event: Event, result: &Result<Step, Ignored>) {
    static FILE_HANDLED: OnceLock<Mutex<(Option<std::fs::File>, u64, u64)>> = OnceLock::new();
    let file = FILE_HANDLED.get_or_init(|| Mutex::new((std::fs::File::create(FILE).ok(), 0, 0)));
    let mut file = file.lock().unwrap_or_else(|e| e.into_inner());
    let (out, handled, written) = &mut *file;
    *handled += 1;
    let line = line(mode, event, result, *handled);
    if let Some(out) = out.as_mut().filter(|_| *written < MAX_BYTES) {
        if writeln!(out, "{line}").is_ok() {
            *written += line.len() as u64 + 1;
        }
    }
}

fn per_thread(dir: &std::path::Path, mode: Mode, event: Event, result: &Result<Step, Ignored>) {
    thread_local! {
        static FILE: RefCell<Option<std::fs::File>> = const { RefCell::new(None) };
        static HANDLED: Cell<u64> = const { Cell::new(0) };
    }
    let handled = HANDLED.with(|handled| {
        handled.set(handled.get() + 1);
        handled.get()
    });
    let line = line(mode, event, result, handled);
    FILE.with(|file| {
        let mut file = file.borrow_mut();
        if file.is_none() {
            let thread = std::thread::current();
            let name = thread
                .name()
                .map(|name| name.replace("::", "."))
                .unwrap_or_else(|| format!("{:?}", thread.id()));
            let _ = std::fs::create_dir_all(dir);
            *file = std::fs::File::create(dir.join(format!("{name}.jsonl"))).ok();
        }
        if let Some(file) = file.as_mut() {
            let _ = writeln!(file, "{line}");
        }
    });
}

/// The model's name for a mode.
pub fn mode_name(mode: Mode) -> &'static str {
    match mode {
        Mode::Awake { .. } => "Awake",
        Mode::Asleep(Sleep::DimmingLight { .. }) => "AsleepDimming",
        Mode::Asleep(Sleep::Settling { .. }) => "AsleepSettling",
        Mode::Asleep(Sleep::Suspended { .. }) => "AsleepSuspended",
        Mode::Dozing(Doze::DimmingLight { .. }) => "DozeDimming",
        Mode::Dozing(Doze::Settling { .. }) => "DozeSettling",
        Mode::Dozing(Doze::Napping { .. }) => "DozeNapping",
        Mode::Dozing(Doze::Drawing { .. }) => "DozeDrawing",
        Mode::UsbDrive { .. } => "UsbDrive",
        Mode::Returning { .. } => "Returning",
    }
}

/// Whether the mode carries a light level to put back.
fn light(mode: Mode) -> bool {
    match mode {
        Mode::Asleep(Sleep::Settling { light, .. } | Sleep::Suspended { light, .. })
        | Mode::Dozing(
            Doze::Settling { light, .. }
            | Doze::Napping { light, .. }
            | Doze::Drawing { light, .. },
        ) => light.is_some(),
        Mode::Awake { .. }
        | Mode::Asleep(Sleep::DimmingLight { .. })
        | Mode::Dozing(Doze::DimmingLight { .. })
        | Mode::UsbDrive { .. }
        | Mode::Returning { .. } => false,
    }
}

/// The model's action for an event, and its parameter if it has one.
fn action(mode: Mode, event: Event) -> (&'static str, Option<bool>) {
    match event {
        Event::PowerButton => ("PowerButton", None),
        Event::Idle => ("Idle", None),
        Event::Doze => ("Doze", None),
        Event::BatteryEmpty => ("BatteryEmpty", None),
        Event::LightOff(level) => ("LightOff", Some(level.is_some())),
        Event::PanelSettled => ("PanelSettled", None),
        Event::Resumed => ("Resumed", None),
        Event::SuspendFailed => ("SuspendFailed", None),
        Event::StorageLeaving => ("StorageLeaving", None),
        Event::Drive(Drive::Computer) => ("DriveComputer", None),
        Event::Drive(Drive::Kindle) => ("DriveKindle", None),
        Event::DriveNotice(notice) => ("DriveNotice", Some(notice.storage_back())),
        Event::DriveSettled(at) => (
            "DriveSettled",
            Some(matches!(mode, Mode::Returning { since } if since == at)),
        ),
    }
}

/// By name alone (`{ .. }`): what a variant carries can grow without this
/// changing.
fn effect_name(effect: &Effect) -> &'static str {
    match effect {
        Effect::ShowSleep { .. } => "ShowSleep",
        Effect::LightOff { .. } => "LightOff",
        Effect::AwaitPanel { .. } => "AwaitPanel",
        Effect::Suspend { .. } => "Suspend",
        Effect::Nap { .. } => "Nap",
        Effect::Redraw { .. } => "Redraw",
        Effect::Wake { .. } => "Wake",
        Effect::Rouse { .. } => "Rouse",
        Effect::Reconnect { .. } => "Reconnect",
        Effect::RestoreLight { .. } => "RestoreLight",
        Effect::ShowUsbDrive { .. } => "ShowUsbDrive",
        Effect::ReleaseStorage { .. } => "ReleaseStorage",
        Effect::AwaitDrive { .. } => "AwaitDrive",
        Effect::StorageBack { .. } => "StorageBack",
        Effect::DriveNotice { .. } => "DriveNotice",
    }
}

/// One line of the trace: the `handled`th event.
pub fn line(mode: Mode, event: Event, result: &Result<Step, Ignored>, handled: u64) -> String {
    let (name, param) = action(mode, event);
    let (after, effects, ignored) = match result {
        Ok(step) => (
            step.next,
            step.effects.iter().map(effect_name).collect(),
            "none",
        ),
        Err(why) => (
            mode,
            Vec::new(),
            match why {
                Ignored::WakingPress => "WakingPress",
                Ignored::InUsbDrive => "InUsbDrive",
                Ignored::Stale => "Stale",
            },
        ),
    };
    let mut line = serde_json::json!({
        "action": name,
        "state": {
            "mode": mode_name(after),
            "light": light(after),
            "effects": effects,
            "ignored": ignored,
            "handled": handled % 4,
        },
    });
    if let Some(param) = param {
        line["params"] = serde_json::json!([param]);
    }
    line.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::Brightness;
    use std::time::{Duration, Instant};

    #[test]
    fn a_line_names_the_models_action_and_state() {
        let mode = Mode::start(Instant::now());
        let event = Event::Doze;
        let result = mode.step(event, Instant::now());
        let json: serde_json::Value = serde_json::from_str(&line(mode, event, &result, 1)).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "action": "Doze",
                "state": {
                    "mode": "DozeDimming",
                    "light": false,
                    "effects": ["LightOff"],
                    "ignored": "none",
                    "handled": 1,
                },
            })
        );
        let mode = Mode::Dozing(Doze::DimmingLight);
        let event = Event::LightOff(Some(Brightness::new(40)));
        let result = mode.step(event, Instant::now());
        let json: serde_json::Value = serde_json::from_str(&line(mode, event, &result, 1)).unwrap();
        assert_eq!(json["params"], serde_json::json!([true]));
        assert_eq!(json["state"]["light"], serde_json::json!(true));
    }

    /// xorshift64*: the same sequence on every run and every machine.
    struct Seeded(u64);

    impl Seeded {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 >> 12;
            self.0 ^= self.0 << 25;
            self.0 ^= self.0 >> 27;
            self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
        }

        fn below(&mut self, n: u64) -> u64 {
            self.next() % n
        }
    }

    /// Any event, now and then one that the mode can't have asked for, as
    /// stale timers and workers deliver them.
    fn event(rng: &mut Seeded, mode: Mode, now: Instant) -> Event {
        use crate::drive::{Notice, Refusal};
        let level = || Some(Brightness::new(40));
        match rng.below(16) {
            0 | 1 => Event::PowerButton,
            2 => Event::Idle,
            3 => Event::Doze,
            4 => Event::BatteryEmpty,
            5 => Event::LightOff(if rng.below(3) == 0 { None } else { level() }),
            6 | 7 => Event::PanelSettled,
            8 | 9 => Event::Resumed,
            10 => Event::SuspendFailed,
            11 => Event::StorageLeaving,
            12 => Event::Drive(if rng.below(2) == 0 {
                Drive::Computer
            } else {
                Drive::Kindle
            }),
            13 => Event::DriveNotice(if rng.below(2) == 0 {
                Notice::Refused(Refusal::Unplugged)
            } else {
                Notice::Repaired
            }),
            _ => Event::DriveSettled(match (mode, rng.below(3)) {
                (Mode::Returning { since }, 0 | 1) => since,
                _ => now,
            }),
        }
    }

    /// Seeded runs of random events through `Mode::step`, each recorded
    /// as the app records its events, in a thread (and so a trace) of its
    /// own: `cargo xtask verify` checks they conform to verify/Sleep.tla
    /// (SleepOpen.cfg). Also: no event panics, and every mode is reached.
    #[test]
    fn random_events() {
        let runs: Vec<_> = [0x6b69_6e64_696c_u64, 0x51, 0x2026, 0xfeed, 0xd0e5]
            .into_iter()
            .map(|seed| {
                std::thread::Builder::new()
                    .name(format!("trace.random.{seed:x}"))
                    .spawn(move || random_run(seed, 800))
                    .unwrap()
            })
            .collect();
        let mut seen = std::collections::BTreeSet::new();
        for run in runs {
            seen.extend(run.join().unwrap());
        }
        assert_eq!(seen.len(), 10, "every mode reached: {seen:?}");
    }

    fn random_run(seed: u64, events: usize) -> std::collections::BTreeSet<&'static str> {
        let mut rng = Seeded(seed);
        let t0 = Instant::now();
        let mut now = t0;
        let mut mode = Mode::start(t0);
        let mut seen = std::collections::BTreeSet::new();
        for _ in 0..events {
            // Mostly within WAKING_PRESS of the last event, sometimes long after.
            now += Duration::from_millis(match rng.below(4) {
                0 => 5_000,
                _ => rng.below(1_500),
            });
            let event = event(&mut rng, mode, now);
            let result = step(mode, event, mode.step(event, now));
            seen.insert(mode_name(mode));
            if let Ok(Step { next, .. }) = result {
                mode = next;
            }
        }
        seen
    }
}
