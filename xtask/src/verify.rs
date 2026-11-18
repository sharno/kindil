//! `cargo xtask verify`: the formal models in verify/ (TLA+, checked with
//! everystate), and the traces kindil's tests record (src/trace.rs)
//! checked against them. See docs/DEVELOP.md, "Formal models".
use anyhow::{bail, Context};
use std::path::Path;
use std::process::Command;
use std::time::Instant;

/// What a configuration is expected to say.
#[derive(Clone, Copy)]
enum Expect {
    Holds,
    /// A finding on the current code: the model keeps the property, and
    /// the check fails until the code is fixed. Then it holds, and this
    /// becomes `Holds`.
    Fails(&'static str),
}

/// Every model configuration: the module, the .cfg, what it should say.
const CHECKS: &[(&str, &str, Expect)] = &[
    ("Sleep", "Sleep", Expect::Holds),
    ("Sleep", "SleepDoze", Expect::Holds),
    ("Sleep", "SleepUsb", Expect::Holds),
    ("Sleep", "SleepOpen", Expect::Holds),
    // Failed on 0.15.0; holds since lowbat and the charger check before powering off.
    ("Sleep", "SleepCharger", Expect::Holds),
    (
        "Sleep",
        "SleepRaces",
        Expect::Fails("a nap or sleep that the app left still suspends"),
    ),
    (
        "Sleep",
        "SleepNearAlarm",
        Expect::Fails("a press just before the check alarm is taken for it"),
    ),
    (
        "Sleep",
        "SleepAlarmFails",
        Expect::Fails("without its check alarm, a later press is taken for it"),
    ),
    // Holds since d674cfd: the battery's sleep powers off when it can't suspend.
    ("Sleep", "SleepRefused", Expect::Holds),
    // Failed on 0.15.0; holds since power::sleep looks for a charger before powering off.
    ("Sleep", "SleepPowerOff", Expect::Holds),
    (
        "Sleep",
        "SleepNapClock",
        Expect::Fails("a clock step back under a nap being armed"),
    ),
    (
        "Sleep",
        "SleepCableQuiet",
        Expect::Fails("unplugged asleep, if a cable doesn't wake the PW1"),
    ),
    ("Launcher", "Launcher", Expect::Holds),
    (
        "Launcher",
        "LauncherCommitCut",
        Expect::Fails("an exit between the commit's renames"),
    ),
    ("Boot", "Boot", Expect::Holds),
    (
        "Boot",
        "BootDark",
        Expect::Fails("a boot the overlays hang isn't counted by the guard"),
    ),
];

/// Where the tests record their traces.
const TRACES: &str = "target/verify/traces";
/// A UI test's trace: up to this many of the model's steps go unlogged
/// before each event (a host's nap, an alarm, a timer firing).
const UI_HIDDEN: &str = "3";

fn installed() -> bool {
    Command::new("everystate")
        .arg("--version")
        .output()
        .is_ok_and(|out| out.status.success())
}

/// A field of everystate's -json result, as written.
fn field<'a>(json: &'a str, key: &str) -> Option<&'a str> {
    let at = json.rfind(&format!("\"{key}\":"))? + key.len() + 3;
    let rest = &json[at..];
    let rest = rest.strip_prefix('"').unwrap_or(rest);
    let end = rest.find(['"', ',', '}']).unwrap_or(rest.len());
    Some(&rest[..end])
}

pub fn verify() -> anyhow::Result<()> {
    if !installed() {
        println!(
            "everystate isn't installed: skipping the formal models \
             (cargo install everystate, or npm i -g everystate; `nix develop` has it)"
        );
        return Ok(());
    }
    models()?;
    conformance()
}

fn models() -> anyhow::Result<()> {
    let mut wrong = Vec::new();
    for &(module, cfg, expect) in CHECKS {
        let text = std::fs::read_to_string(format!("verify/{cfg}.cfg"))?;
        let mut command = Command::new("everystate");
        command.current_dir("verify").arg("-json");
        // Liveness is checked on one thread; safety on every core.
        if !text.contains("PROPERTIES") {
            command.args(["-workers", "auto"]);
        }
        command.args(["-config", &format!("{cfg}.cfg"), &format!("{module}.tla")]);
        let since = Instant::now();
        let out = command.output().context("running everystate")?;
        let json = String::from_utf8_lossy(&out.stdout);
        let json = json.lines().last().unwrap_or_default();
        let status = field(json, "status").unwrap_or("?");
        let distinct = field(json, "distinct").unwrap_or("?");
        let name = field(json, "name").unwrap_or("a liveness property");
        let took = since.elapsed().as_secs_f64();
        let verdict = match (status, expect) {
            ("ok", Expect::Holds) => "holds".to_string(),
            ("ok", Expect::Fails(finding)) => {
                wrong.push(format!(
                    "{cfg}: holds now ({finding}): fixed? make it Expect::Holds"
                ));
                "HOLDS, expected to fail".to_string()
            }
            ("invariant" | "liveness" | "deadlock", Expect::Fails(finding)) => {
                format!("fails as expected: {name} ({finding})")
            }
            (_, Expect::Holds) => {
                wrong.push(format!("{cfg}: {status} {name}: {json}"));
                format!("FAILS: {status} {name}")
            }
            (_, Expect::Fails(_)) => {
                wrong.push(format!("{cfg}: {status}: {json}"));
                format!("ERROR: {status}")
            }
        };
        println!("{module:9} {cfg:18} {distinct:>9} states {took:6.1} s  {verdict}");
    }
    if !wrong.is_empty() {
        bail!("formal models:\n  {}", wrong.join("\n  "));
    }
    Ok(())
}

/// The traces the tests record, checked against verify/Sleep.tla: the
/// reducer's seeded random runs against SleepOpen (any event at any time),
/// the UI tests' runs against SleepTrace (the app's own timers, workers
/// and the host's naps and sleeps as modelled).
fn conformance() -> anyhow::Result<()> {
    let traces = Path::new(TRACES);
    let _ = std::fs::remove_dir_all(traces);
    let dir = std::fs::canonicalize(".")?.join(traces);
    for target in [["--lib", "trace::"], ["--test", "slint_ui"]] {
        crate::dev::run(
            crate::dev::cargo()
                .env("KINDIL_TRACE", &dir)
                .args(["test", "--locked", "-p", "kindil", "--quiet"])
                .args(target),
        )?;
    }
    let mut files: Vec<_> = std::fs::read_dir(traces)?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.extension().is_some_and(|ext| ext == "jsonl"))
        .collect();
    files.sort();
    ensure_some(&files)?;
    let mut wrong = Vec::new();
    for file in &files {
        let name = file.file_name().unwrap_or_default().to_string_lossy();
        let (cfg, hidden) = if name.starts_with("trace.random") {
            ("SleepOpen.cfg", "1")
        } else {
            ("SleepTrace.cfg", UI_HIDDEN)
        };
        let since = Instant::now();
        let out = Command::new("everystate")
            .current_dir("verify")
            .args(["conform", "-hidden", hidden, "-trace"])
            .arg(std::fs::canonicalize(file)?)
            .args(["-config", cfg, "Sleep.tla"])
            .output()
            .context("running everystate conform")?;
        let said = String::from_utf8_lossy(&out.stdout);
        let first = said.lines().next().unwrap_or_default();
        println!(
            "conform {cfg:15} {:6.1} s  {name}: {first}",
            since.elapsed().as_secs_f64()
        );
        if !out.status.success() {
            wrong.push(format!("{name}:\n{said}"));
        }
    }
    if !wrong.is_empty() {
        bail!(
            "traces that don't conform to verify/Sleep.tla:\n{}",
            wrong.join("\n")
        );
    }
    Ok(())
}

fn ensure_some(files: &[std::path::PathBuf]) -> anyhow::Result<()> {
    if files.len() < 2 {
        bail!("the tests recorded no traces in {TRACES}: is src/trace.rs called?");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_everystates_json() {
        let json = r#"{"status":"invariant","name":"TypeOK","generated":5,"distinct":4,"queue":0}"#;
        assert_eq!(field(json, "status"), Some("invariant"));
        assert_eq!(field(json, "name"), Some("TypeOK"));
        assert_eq!(field(json, "distinct"), Some("4"));
        assert_eq!(field(json, "missing"), None);
    }

    #[test]
    fn every_configuration_is_checked() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../verify");
        let mut cfgs: Vec<String> = std::fs::read_dir(root)
            .unwrap()
            .filter_map(|entry| {
                let name = entry.unwrap().file_name().to_string_lossy().into_owned();
                name.strip_suffix(".cfg").map(str::to_owned)
            })
            // Only for traces.
            .filter(|cfg| cfg != "SleepTrace")
            .collect();
        cfgs.sort();
        let mut checked: Vec<String> = CHECKS.iter().map(|(_, cfg, _)| cfg.to_string()).collect();
        checked.sort();
        assert_eq!(cfgs, checked);
    }
}
