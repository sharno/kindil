//! The Kindle over SSH.
//!
//! `KINDIL_DEVICE` names its SSH target (e.g. `root@192.168.1.10`);
//! `KINDIL_SSH_KEY`, optionally, the only key to use. The PW1's dropbear
//! wants scp's old protocol (`-O`) and greets each session with a banner,
//! which [`Device::run`] drops. Commands run with the usual PATH: SSH
//! sessions lack sbin, where upstart's tools live.
use anyhow::{bail, Context};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const PATH: &str = "export PATH=/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin; ";

pub struct Device {
    target: String,
    key: Option<PathBuf>,
}

impl Device {
    pub fn from_env() -> anyhow::Result<Self> {
        let target = std::env::var("KINDIL_DEVICE")
            .context("set KINDIL_DEVICE to the Kindle's SSH target, e.g. root@192.168.1.10")?;
        let key = std::env::var_os("KINDIL_SSH_KEY").map(PathBuf::from);
        Ok(Self { target, key })
    }

    fn options(&self) -> Vec<String> {
        let mut options = vec![
            "-o".into(),
            "BatchMode=yes".into(),
            "-o".into(),
            "ConnectTimeout=6".into(),
        ];
        if let Some(key) = &self.key {
            options.extend([
                "-i".into(),
                key.display().to_string(),
                "-o".into(),
                "IdentitiesOnly=yes".into(),
                "-o".into(),
                "IdentityAgent=none".into(),
            ]);
        }
        options
    }

    /// Runs `command` in the Kindle's shell; its output without the banner.
    /// Fails if SSH can't reach it.
    pub fn run(&self, command: &str) -> anyhow::Result<String> {
        let output = Command::new("ssh")
            .args(self.options())
            .arg(&self.target)
            .arg(format!("{PATH}{command}"))
            .stdin(Stdio::null())
            .output()
            .context("running ssh")?;
        if output.status.code() == Some(255) {
            bail!("can't reach {}", self.target);
        }
        Ok(without_banner(&String::from_utf8_lossy(&output.stdout)))
    }

    /// Copies `files` into `dir` on the Kindle.
    pub fn put(&self, files: &[&Path], dir: &str) -> anyhow::Result<()> {
        let status = Command::new("scp")
            .args(["-q", "-O"])
            .args(self.options())
            .args(files)
            .arg(format!("{}:{dir}/", self.target))
            .status()
            .context("running scp")?;
        anyhow::ensure!(status.success(), "copying to {}:{dir} failed", self.target);
        Ok(())
    }

    pub fn reachable(&self) -> bool {
        self.run("true").is_ok()
    }

    /// Waits up to `hours` for the Kindle to answer: asleep, dozing or
    /// offline it doesn't, and it is not this tool's place to wake it.
    pub fn wait(&self, hours: u64) -> anyhow::Result<()> {
        let since = Instant::now();
        while !self.reachable() {
            if since.elapsed() >= Duration::from_secs(hours * 3600) {
                bail!("the Kindle didn't answer in {hours} h");
            }
            std::thread::sleep(Duration::from_secs(20));
        }
        Ok(())
    }
}

/// The Kindle's dropbear greets every session; blank lines go too.
pub fn without_banner(output: &str) -> String {
    output
        .lines()
        .filter(|line| !line.trim().is_empty() && !line.contains("Welcome to Kindle"))
        .map(|line| format!("{line}\n"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drops_the_banner_and_blank_lines() {
        assert_eq!(
            without_banner("\n\nWelcome to Kindle!\n\nkindil 0.9.1\n  \nonline\n"),
            "kindil 0.9.1\nonline\n"
        );
        assert_eq!(without_banner(""), "");
    }
}
