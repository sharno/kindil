//! The build, test and device-script tasks, run from the repo root.
use crate::device::Device;
use anyhow::{ensure, Context};
use std::path::{Path, PathBuf};
use std::process::Command;

const TARGET: &str = "armv7-unknown-linux-musleabihf";

fn run(command: &mut Command) -> anyhow::Result<()> {
    let status = command
        .status()
        .with_context(|| format!("running {command:?}"))?;
    ensure!(status.success(), "{command:?} failed ({status})");
    Ok(())
}

fn output(command: &mut Command) -> anyhow::Result<String> {
    let out = command
        .output()
        .with_context(|| format!("running {command:?}"))?;
    ensure!(out.status.success(), "{command:?} failed ({})", out.status);
    Ok(String::from_utf8(out.stdout)?.trim().to_owned())
}

fn cargo() -> Command {
    Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".into()))
}

pub fn test() -> anyhow::Result<()> {
    run(cargo().args(["test", "--workspace", "--locked"]))?;
    // The shims that hand over to the launcher.
    run(Command::new("shellcheck").args([
        "kindle/extensions/kindil/bin/start.sh",
        "kindle/extensions/kindil/bin/boot.sh",
        "kindle/extensions/kindil/boot/install.sh",
    ]))
}

pub fn check() -> anyhow::Result<()> {
    run(cargo().args(["fmt", "--all", "--check"]))?;
    run(cargo().args([
        "clippy",
        "--locked",
        "--workspace",
        "--all-targets",
        "--features",
        "kindil/kindle",
        "--",
        "-D",
        "warnings",
    ]))?;
    test()
}

pub fn fmt() -> anyhow::Result<()> {
    run(cargo().args(["fmt", "--all"]))?;
    run(Command::new("nix").arg("fmt"))
}

/// The flake's musl-aware link driver (or ARM_CC); built if missing.
fn arm_linker() -> anyhow::Result<String> {
    if let Ok(cc) = std::env::var("ARM_CC") {
        return Ok(cc);
    }
    let out = output(Command::new("nix").args(["eval", "--raw", ".#armv7-linker.outPath"]))?;
    let cc = format!("{out}/bin/armv7l-unknown-linux-gnueabihf-gcc");
    if !Path::new(&cc).exists() {
        run(Command::new("nix").args(["build", "--no-link", ".#armv7-linker"]))?;
    }
    ensure!(
        Path::new(&cc).exists(),
        "no ARM link driver; run 'nix build .#armv7-linker' or set ARM_CC"
    );
    Ok(cc)
}

fn arm_cargo(cc: &str) -> Command {
    let mut command = cargo();
    command.env("CC_armv7_unknown_linux_musleabihf", cc).env(
        "RUSTFLAGS",
        format!("-C linker={cc} -C target-feature=+crt-static -C link-self-contained=no"),
    );
    command
}

/// dist/kindil, static ARMv7, with kindil's own wpa_supplicant, Dropbear
/// and its launcher inside.
pub fn device() -> anyhow::Result<PathBuf> {
    let cc = arm_linker()?;
    run(arm_cargo(&cc).args([
        "build",
        "--locked",
        "--release",
        "--package",
        "launcher",
        "--target",
        TARGET,
    ]))?;
    let launcher = std::fs::canonicalize(format!("target/{TARGET}/release/launcher"))?;
    let wpa = output(Command::new("nix").args([
        "build",
        "--no-link",
        "--print-out-paths",
        ".#wpa-supplicant-device",
    ]))?;
    let dropbear = output(Command::new("nix").args([
        "build",
        "--no-link",
        "--print-out-paths",
        ".#dropbear-device",
    ]))?;
    run(arm_cargo(&cc)
        .env("KINDIL_WPA_SUPPLICANT", format!("{wpa}/bin/wpa_supplicant"))
        .env("KINDIL_DROPBEAR", format!("{dropbear}/bin/dropbearmulti"))
        .env("KINDIL_LAUNCHER", &launcher)
        .args([
            "build",
            "--locked",
            "--release",
            "--features",
            "kindle",
            "--target",
            TARGET,
        ]))?;
    std::fs::create_dir_all("dist")?;
    let binary = PathBuf::from("dist/kindil");
    std::fs::copy(format!("target/{TARGET}/release/kindil"), &binary)?;
    let header = output(Command::new("readelf").arg("-h").arg(&binary))?;
    ensure!(
        header
            .lines()
            .any(|l| l.contains("Machine:") && l.contains("ARM")),
        "not an ARM binary"
    );
    let program = output(Command::new("readelf").arg("-l").arg(&binary))?;
    ensure!(
        !program.contains("INTERP"),
        "the binary is dynamically linked"
    );
    println!(
        "device binary: {} ({} bytes, static ARMv7)",
        binary.display(),
        std::fs::metadata(&binary)?.len()
    );
    Ok(binary)
}

pub fn test_arm() -> anyhow::Result<()> {
    let cc = arm_linker()?;
    run(arm_cargo(&cc)
        .env(
            "CARGO_TARGET_ARMV7_UNKNOWN_LINUX_MUSLEABIHF_RUNNER",
            "qemu-arm",
        )
        .args(["test", "--locked", "--workspace", "--target", TARGET]))
}

pub fn sim() -> anyhow::Result<()> {
    std::fs::create_dir_all("dist/sim")?;
    run(cargo().args([
        "run",
        "--quiet",
        "--",
        "preview",
        "dist/sim",
        "--config",
        "examples/kindil.toml",
        "--books",
        "examples/books",
    ]))?;
    let ffmpeg = Command::new("ffmpeg").arg("-version").output().is_ok();
    if !ffmpeg {
        println!("(install ffmpeg to convert the PPMs to PNG)");
        return Ok(());
    }
    for entry in std::fs::read_dir("dist/sim")? {
        let ppm = entry?.path();
        if ppm.extension().is_some_and(|e| e == "ppm") {
            run(Command::new("ffmpeg")
                .args(["-loglevel", "error", "-y", "-i"])
                .arg(&ppm)
                .arg(ppm.with_extension("png")))?;
        }
    }
    println!("PNGs in dist/sim");
    Ok(())
}

pub fn desktop() -> anyhow::Result<()> {
    run(cargo().args([
        "run",
        "--features",
        "desktop",
        "--",
        "desktop",
        "--config",
        "examples/kindil.toml",
        "--books",
        "examples/books",
    ]))
}

/// Builds, then copies the launcher files and the binary to the Kindle: the
/// launcher first, the binary last and moved into place, so a half-finished
/// copy never runs a stale binary.
pub fn deploy() -> anyhow::Result<()> {
    let binary = device()?;
    let device = Device::from_env()?;
    let launcher = Path::new("kindle/extensions/kindil");
    let files: Vec<PathBuf> = walk(launcher)?;
    device.run("mkdir -p /mnt/us/extensions/kindil /mnt/us/kindil")?;
    for file in &files {
        let inside = file.strip_prefix(launcher)?;
        let dir =
            Path::new("/mnt/us/extensions/kindil").join(inside.parent().unwrap_or(Path::new("")));
        device.run(&format!("mkdir -p '{}'", dir.display()))?;
        device.put(&[file], &dir.display().to_string())?;
    }
    // Staged aside, then moved over the old one in a single step.
    device.run("mkdir -p /mnt/us/extensions/kindil/bin.new")?;
    device.put(&[&binary], "/mnt/us/extensions/kindil/bin.new")?;
    print!(
        "{}",
        device.run(
            "chmod 755 /mnt/us/extensions/kindil/bin.new/kindil && \
             mv /mnt/us/extensions/kindil/bin.new/kindil /mnt/us/extensions/kindil/bin/kindil && \
             rmdir /mnt/us/extensions/kindil/bin.new && \
             /mnt/us/extensions/kindil/bin/kindil --version"
        )?
    );
    Ok(())
}

fn walk(dir: &Path) -> anyhow::Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        if path.is_dir() {
            files.extend(walk(&path)?);
        } else {
            files.push(path);
        }
    }
    Ok(files)
}

/// Starts `script` on the Kindle with `args`, and kindil's lifeline beside it
/// (src/lifeline.rs), which brings kindil and Wi-Fi back when it ends or at
/// `deadline` seconds. The lifeline runs as `kindil-lifeline`, a link to
/// kindil's binary, so neither it nor the experiment takes it for kindil.
pub fn run_on_device(script: &Path, deadline: u64, args: &[String]) -> anyhow::Result<()> {
    ensure!(script.is_file(), "no script {}", script.display());
    let device = Device::from_env()?;
    let name = script
        .file_name()
        .context("a script file")?
        .to_string_lossy()
        .into_owned();
    device.put(&[script], "/var/local/kindil")?;
    let quoted: Vec<String> = args.iter().map(|a| shell_quote(a)).collect();
    print!(
        "{}",
        device.run(&format!(
            "cd /var/local/kindil; sh /var/local/kindil/{name} {} </dev/null >/dev/null 2>&1 & pid=$!; \
             ln -sf kindil /var/local/kindil/kindil-lifeline; \
             setsid /var/local/kindil/kindil-lifeline {deadline} $pid </dev/null \
             >>/var/local/kindil/lifeline.log 2>&1 & \
             echo \"started {name} as $pid, lifeline at {deadline} s\"",
            quoted.join(" ")
        ))?
    );
    Ok(())
}

pub fn wait(hours: u64, command: &[String]) -> anyhow::Result<()> {
    let device = Device::from_env()?;
    device.wait(hours)?;
    println!("the Kindle answered");
    let command = if command.is_empty() {
        "/var/local/kindil/kindil --version; \
         grep -v 'weather:\\|radar:\\|start/running\\|stop/waiting' /var/local/kindil/kindil.log | tail -n 20; \
         wpa_cli -i wlan0 status 2>/dev/null | grep '^ssid\\|wpa_state\\|ip_address'"
            .to_owned()
    } else {
        command.join(" ")
    };
    print!("{}", device.run(&command)?);
    Ok(())
}

/// Single-quotes `arg` for the Kindle's shell.
pub fn shell_quote(arg: &str) -> String {
    if !arg.is_empty()
        && arg
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_./=:,+@%".contains(c))
    {
        return arg.to_owned();
    }
    format!("'{}'", arg.replace('\'', "'\\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quotes_only_what_needs_it() {
        assert_eq!(shell_quote("600"), "600");
        assert_eq!(shell_quote("/var/local/x.sh"), "/var/local/x.sh");
        assert_eq!(shell_quote("two words"), "'two words'");
        assert_eq!(shell_quote("it's"), "'it'\\''s'");
        assert_eq!(shell_quote(""), "''");
    }
}
