//! The Kindle over SSH.
//!
//! `KINDIL_DEVICE` names its SSH target (e.g. `root@192.168.1.10`);
//! `KINDIL_SSH_KEY`, optionally, the only key to use; `KINDIL_SSH_PORT`,
//! optionally, its port (ssh's own, 22, otherwise: kindil's own server
//! listens on 2222 for now). Files go over plain ssh, piped into `cat` on
//! the Kindle, so neither scp nor sftp is needed there. The PW1's dropbear
//! greets each session with a banner, which [`Device::run`] drops. Commands
//! run with the usual PATH: SSH sessions lack sbin, where upstart's tools
//! live.
use crate::dev::shell_quote;
use anyhow::{bail, Context};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const PATH: &str = "export PATH=/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin; ";

pub struct Device {
    target: String,
    key: Option<PathBuf>,
    port: Option<u16>,
}

impl Device {
    pub fn from_env() -> anyhow::Result<Self> {
        let target = std::env::var("KINDIL_DEVICE")
            .context("set KINDIL_DEVICE to the Kindle's SSH target, e.g. root@192.168.1.10")?;
        let key = std::env::var_os("KINDIL_SSH_KEY").map(PathBuf::from);
        let port = match std::env::var("KINDIL_SSH_PORT") {
            Ok(port) => Some(
                port.parse()
                    .with_context(|| format!("KINDIL_SSH_PORT={port} is not a port"))?,
            ),
            Err(_) => None,
        };
        Ok(Self { target, key, port })
    }

    fn options(&self) -> Vec<String> {
        let mut options = vec![
            "-o".into(),
            "BatchMode=yes".into(),
            "-o".into(),
            "ConnectTimeout=6".into(),
        ];
        if let Some(port) = self.port {
            options.extend(["-p".into(), port.to_string()]);
        }
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

    /// ssh to the Kindle, to run `command` in its shell with the usual PATH.
    fn ssh(&self, command: &str) -> Command {
        let mut ssh = Command::new("ssh");
        ssh.args(self.options())
            .arg(&self.target)
            .arg(format!("{PATH}{command}"));
        ssh
    }

    /// Runs `command` in the Kindle's shell; its output without the banner.
    /// Fails if SSH can't reach it.
    pub fn run(&self, command: &str) -> anyhow::Result<String> {
        let output = self
            .ssh(command)
            .stdin(Stdio::null())
            .output()
            .context("running ssh")?;
        if output.status.code() == Some(255) {
            bail!("can't reach {}", self.target);
        }
        Ok(without_banner(&String::from_utf8_lossy(&output.stdout)))
    }

    /// Copies `files` into `dir` on the Kindle, each with its mode, written
    /// beside its place and moved there once whole.
    pub fn put(&self, files: &[&Path], dir: &str) -> anyhow::Result<()> {
        for file in files {
            let name = file
                .file_name()
                .with_context(|| format!("no file name in {}", file.display()))?
                .to_string_lossy();
            let mode = {
                use std::os::unix::fs::PermissionsExt;
                std::fs::metadata(file)
                    .with_context(|| format!("reading {}", file.display()))?
                    .permissions()
                    .mode()
            };
            let stdin =
                std::fs::File::open(file).with_context(|| format!("reading {}", file.display()))?;
            let status = self
                .ssh(&put_command(dir, &name, mode))
                .stdin(stdin)
                .stdout(Stdio::null())
                .status()
                .context("running ssh")?;
            anyhow::ensure!(
                status.success(),
                "copying {} to {}:{dir} failed ({status})",
                file.display(),
                self.target
            );
        }
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

/// The Kindle's side of [`Device::put`]: stdin into `dir/name.part`, given
/// the file's permissions, then moved over `dir/name`.
pub fn put_command(dir: &str, name: &str, mode: u32) -> String {
    let dir = dir.trim_end_matches('/');
    let part = shell_quote(&format!("{dir}/{name}.part"));
    let path = shell_quote(&format!("{dir}/{name}"));
    format!(
        "cat > {part} && chmod {:o} {part} && mv {part} {path}",
        mode & 0o777
    )
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

    fn args(command: &Command) -> Vec<String> {
        command
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn drops_the_banner_and_blank_lines() {
        assert_eq!(
            without_banner("\n\nWelcome to Kindle!\n\nkindil 0.9.1\n  \nonline\n"),
            "kindil 0.9.1\nonline\n"
        );
        assert_eq!(without_banner(""), "");
    }

    #[test]
    fn puts_a_file_whole_or_not_at_all() {
        assert_eq!(
            put_command("/var/local/kindil", "kindil", 0o100755),
            "cat > /var/local/kindil/kindil.part && chmod 755 /var/local/kindil/kindil.part && \
             mv /var/local/kindil/kindil.part /var/local/kindil/kindil"
        );
        assert_eq!(
            put_command("/mnt/us/extensions/kindil/", "menu.json", 0o644),
            "cat > /mnt/us/extensions/kindil/menu.json.part && \
             chmod 644 /mnt/us/extensions/kindil/menu.json.part && \
             mv /mnt/us/extensions/kindil/menu.json.part /mnt/us/extensions/kindil/menu.json"
        );
        // Names the shell would split are quoted.
        assert_eq!(
            put_command("/mnt/us/my dir", "it's", 0o600),
            "cat > '/mnt/us/my dir/it'\\''s.part' && chmod 600 '/mnt/us/my dir/it'\\''s.part' && \
             mv '/mnt/us/my dir/it'\\''s.part' '/mnt/us/my dir/it'\\''s'"
        );
    }

    #[test]
    fn runs_ssh_with_its_key_port_and_path() {
        let device = Device {
            target: "root@kindle".into(),
            key: Some("/k/id".into()),
            port: Some(2222),
        };
        let command = device.ssh("uptime");
        assert_eq!(command.get_program(), "ssh");
        assert_eq!(
            args(&command).join(" "),
            "-o BatchMode=yes -o ConnectTimeout=6 -p 2222 -i /k/id -o IdentitiesOnly=yes \
             -o IdentityAgent=none root@kindle \
             export PATH=/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin; uptime"
        );
        // No port given: ssh's own.
        let device = Device {
            target: "root@kindle".into(),
            key: None,
            port: None,
        };
        assert_eq!(
            args(&device.ssh("true")),
            [
                "-o",
                "BatchMode=yes",
                "-o",
                "ConnectTimeout=6",
                "root@kindle",
                &format!("{PATH}true")
            ]
        );
    }
}
