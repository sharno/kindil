//! kindil on an emulated Paperwhite 1: `cargo xtask emulate <task>`.
//!
//! The emulator is katadelos/eink-emulator: QEMU with the PW1's
//! `imx50-celeste` board (EPDC panel, cyttsp touch, MC13892 PMIC with its
//! RTC, the fuel gauge, AR6003 Wi-Fi, USB Ethernet), running the PW1's own
//! kernel and system partition. Nothing of it is in this repo, and neither is
//! any firmware: both come from the computer it runs on, named by
//!
//! - `KINDIL_EINK_EMULATOR`: the emulator's checkout, its QEMU built
//!   (`eink build`);
//! - `KINDIL_EMULATOR_FIRMWARE`: a directory with the PW1's `uImage` and
//!   `rootfs.img` (the raw ext3 of mmcblk0p1), and a bootloader:
//!   `u-boot.bin`, the PW1's own, or `barebox-kindle-ey21.img`, upstream
//!   barebox built for the PW1 (docs/DEVELOP.md#emulator), which this task
//!   drives over the serial console.
//!
//! Unset, every task says so and succeeds, so CI and other computers carry
//! on. Optional: `KINDIL_EMULATOR_NAME` (the instance, `kindil-pw1`) and
//! `KINDIL_EMULATOR_SSH_PORT` (4122: USB SSH; the next port reaches the
//! emulator's SSH over Wi-Fi, the one after kindil's own SSH server over
//! Wi-Fi).
//!
//! The instance's files are the emulator's (`machines/NAME/`): the disk, the
//! serial log, `qemu.log`. Its SSH key is the emulator's
//! (`build/ssh/id_ed25519`), installed in every image it prepares.
use crate::dev::{self, run};
use crate::device::Device;
use anyhow::{bail, ensure, Context};
use clap::Subcommand;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const MODEL: &str = "kindle-paperwhite-1";
/// Marks the emulator's firmware directory as filled by this task.
const MARKER: &str = ".kindil-emulate";
/// Where barebox finds the kernel: QEMU puts the uImage there at every
/// reset, as barebox can't read it from the emulated eMMC (its eSDHC DMA
/// reads come back empty).
const KERNEL_IN_RAM: u32 = 0x7200_0000;

#[derive(Subcommand)]
pub enum Emulate {
    /// Prepare the instance from the firmware directory (again with --fresh)
    Setup {
        /// Delete the instance and prepare it anew
        #[arg(long)]
        fresh: bool,
    },
    /// Build kindil and install it, with example settings, on the running instance
    Install {
        /// Install this device binary instead of building one
        #[arg(long)]
        binary: Option<PathBuf>,
    },
    /// Start the instance in the background and wait for SSH
    Run {
        /// Show the emulator's window (otherwise headless)
        #[arg(long)]
        window: bool,
    },
    /// Boot the kernel from barebox's prompt, after the guest rebooted
    Boot,
    /// Sync the guest's disks and stop the instance
    Stop,
    /// Save the panel as a PNG
    Screenshot { path: PathBuf },
    /// Tap the touchscreen at a point of the panel (758 × 1024 pixels)
    Tap { x: u32, y: u32 },
    /// Boot a clean instance, install and start kindil, check its dashboard,
    /// log and SSH server, then a standalone boot
    Test {
        /// Install this device binary instead of building one
        #[arg(long)]
        binary: Option<PathBuf>,
    },
}

pub fn emulate(task: Emulate) -> anyhow::Result<()> {
    let Some(emulator) = Emulator::from_env()? else {
        println!(
            "emulator not configured: set KINDIL_EINK_EMULATOR (an eink-emulator checkout) and \
             KINDIL_EMULATOR_FIRMWARE (a PW1's uImage, rootfs.img and bootloader); \
             see docs/DEVELOP.md#emulator"
        );
        return Ok(());
    };
    let binary = |binary: Option<PathBuf>| match binary {
        Some(binary) => Ok(binary),
        None => build(),
    };
    match task {
        Emulate::Setup { fresh } => emulator.setup(fresh),
        Emulate::Install { binary: b } => emulator.install(&binary(b)?),
        Emulate::Run { window } => emulator.run(window),
        Emulate::Boot => {
            emulator.boot()?;
            emulator.wait_ssh(Duration::from_secs(600))
        }
        Emulate::Stop => emulator.stop(),
        Emulate::Screenshot { path } => emulator.screenshot(&path),
        Emulate::Tap { x, y } => emulator.tap(x, y),
        Emulate::Test { binary: b } => emulator.test(&binary(b)?),
    }
}

/// What starts the kernel.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Bootloader {
    /// The PW1's own: it loads the kernel from the eMMC and boots it.
    UBoot,
    /// Upstream barebox for the PW1 (EY21): it passes Lab126's ATAGs (board
    /// ID, serial) from the emulated IDME, and is told over the serial
    /// console to boot the kernel QEMU put in RAM.
    Barebox,
}

impl Bootloader {
    /// barebox images carry their name at 0x20.
    fn of(image: &[u8]) -> Self {
        if image.get(0x20..0x27) == Some(b"barebox") {
            Self::Barebox
        } else {
            Self::UBoot
        }
    }
}

struct Emulator {
    root: PathBuf,
    firmware: PathBuf,
    name: String,
    ssh_port: u16,
}

impl Emulator {
    fn from_env() -> anyhow::Result<Option<Self>> {
        let (Some(root), Some(firmware)) = (
            std::env::var_os("KINDIL_EINK_EMULATOR"),
            std::env::var_os("KINDIL_EMULATOR_FIRMWARE"),
        ) else {
            return Ok(None);
        };
        let root = std::fs::canonicalize(&root)
            .with_context(|| format!("KINDIL_EINK_EMULATOR={}", root.to_string_lossy()))?;
        let firmware = std::fs::canonicalize(&firmware)
            .with_context(|| format!("KINDIL_EMULATOR_FIRMWARE={}", firmware.to_string_lossy()))?;
        ensure!(
            root.join("eink").is_file() && root.join("models.json").is_file(),
            "{} is not an eink-emulator checkout",
            root.display()
        );
        // Firmware is the owner's, from their own device: never the repo's.
        let repo = std::fs::canonicalize(crate::root())?;
        ensure!(
            !firmware.starts_with(&repo),
            "KINDIL_EMULATOR_FIRMWARE is inside the repo: keep firmware out of it"
        );
        let name = std::env::var("KINDIL_EMULATOR_NAME").unwrap_or_else(|_| "kindil-pw1".into());
        ensure!(
            !name.is_empty()
                && name
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || "-_".contains(c)),
            "KINDIL_EMULATOR_NAME may hold only a-z, 0-9, - and _"
        );
        let ssh_port = match std::env::var("KINDIL_EMULATOR_SSH_PORT") {
            Ok(port) => port
                .parse()
                .with_context(|| format!("KINDIL_EMULATOR_SSH_PORT={port}"))?,
            Err(_) => 4122,
        };
        Ok(Some(Self {
            root,
            firmware,
            name,
            ssh_port,
        }))
    }

    fn eink(&self) -> Command {
        let mut command = Command::new("python3");
        command.arg(self.root.join("eink")).current_dir(&self.root);
        command
    }

    fn machine(&self) -> PathBuf {
        self.root.join("machines").join(&self.name)
    }

    /// The bootloader the instance runs, in the emulator's firmware directory.
    fn bootloader(&self) -> anyhow::Result<Bootloader> {
        let image = self.root.join("firmware").join(MODEL).join("u-boot.bin");
        Ok(Bootloader::of(&std::fs::read(&image).with_context(
            || format!("reading {}: run cargo xtask emulate setup", image.display()),
        )?))
    }

    /// The QMP and serial sockets: in the temporary directory rather than
    /// the machine's, as a socket's path must fit in 104 bytes.
    fn sockets(&self) -> PathBuf {
        std::env::temp_dir().join(format!("kindil-emulate-{}", self.name))
    }

    fn qmp_socket(&self) -> PathBuf {
        self.sockets().join("qmp.sock")
    }

    fn serial_socket(&self) -> PathBuf {
        self.sockets().join("serial.sock")
    }

    fn serial_log(&self) -> PathBuf {
        self.machine().join(format!("{}.serial.log", self.name))
    }

    fn key(&self) -> PathBuf {
        self.root.join("build/ssh/id_ed25519")
    }

    /// USB SSH into the guest: the emulator's dropbear on 22, as root.
    fn device(&self) -> Device {
        Device::emulator(self.ssh_port, self.key())
    }

    /// kindil's own SSH server, over the emulated Wi-Fi.
    fn kindil_sshd(&self) -> Device {
        Device::emulator(self.ssh_port + 2, self.key())
    }

    fn running(&self) -> bool {
        Qmp::connect(&self.qmp_socket()).is_ok()
    }

    /// Copies the firmware into the emulator's firmware directory for the
    /// PW1, then has the emulator prepare the instance's disk from it (its
    /// own copy of the rootfs, with its SSH, USB networking and offline setup).
    fn setup(&self, fresh: bool) -> anyhow::Result<()> {
        let uimage = self.firmware.join("uImage");
        let mut magic = [0u8; 4];
        std::fs::File::open(&uimage)
            .and_then(|mut f| f.read_exact(&mut magic))
            .with_context(|| format!("reading {}", uimage.display()))?;
        ensure!(
            magic == [0x27, 0x05, 0x19, 0x56],
            "{} is not a uImage",
            uimage.display()
        );
        let rootfs = self.firmware.join("rootfs.img");
        ensure!(rootfs.is_file(), "no {}", rootfs.display());
        let bootloader = ["u-boot.bin", "barebox-kindle-ey21.img"]
            .iter()
            .map(|name| self.firmware.join(name))
            .find(|path| path.is_file())
            .with_context(|| {
                format!(
                    "no bootloader in {}: u-boot.bin, or barebox-kindle-ey21.img \
                     (see docs/DEVELOP.md#emulator); the PW1's kernel needs one",
                    self.firmware.display()
                )
            })?;
        let target = self.root.join("firmware").join(MODEL);
        if target.exists() {
            ensure!(
                target.join(MARKER).is_file(),
                "{} was not filled by this task; move it aside first",
                target.display()
            );
        }
        std::fs::create_dir_all(&target)?;
        std::fs::write(
            target.join(MARKER),
            "filled by kindil's cargo xtask emulate setup\n",
        )?;
        copy_if_changed(&uimage, &target.join("uImage"))?;
        copy_if_changed(&rootfs, &target.join("rootfs.img"))?;
        // The emulator runs whatever is here as the bootloader.
        copy_if_changed(&bootloader, &target.join("u-boot.bin"))?;

        if self.machine().join("machine.json").is_file() {
            if !fresh {
                println!("instance {} is ready (--fresh prepares it anew)", self.name);
                return Ok(());
            }
            ensure!(!self.running(), "stop the instance first");
            run(self.eink().args(["delete", &self.name]))?;
        }
        need_tools()?;
        // No opkg (fetched from the internet at first boot) and the stock
        // login message: the guest stays as close to the device as it can.
        run(self.eink().args([
            "create",
            &self.name,
            "--model",
            MODEL,
            "--no-opkg",
            "--no-motd",
        ]))
    }

    /// The emulator's QEMU command for the instance, with the sockets moved,
    /// a Wi-Fi forward to kindil's SSH server and, for barebox, the kernel
    /// in RAM.
    fn command(&self, window: bool) -> anyhow::Result<Vec<String>> {
        let mut eink = self.eink();
        eink.args([
            "run",
            &self.name,
            "--dry-run",
            "--serial-socket",
            "--qmp-socket",
        ])
        .args(["--ssh-port", &self.ssh_port.to_string()])
        .args(["--wifi-ssh-port", &(self.ssh_port + 1).to_string()]);
        if !window {
            eink.arg("--headless");
        }
        let output = eink.output().context("running eink run --dry-run")?;
        ensure!(
            output.status.success(),
            "eink run --dry-run failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let mut argv = split(String::from_utf8(output.stdout)?.trim())?;
        let machine = self.machine().display().to_string();
        for arg in &mut argv {
            *arg = arg
                .replace(
                    &format!("{machine}/{}.qmp.sock", self.name),
                    &self.qmp_socket().display().to_string(),
                )
                .replace(
                    &format!("{machine}/{}.serial.sock", self.name),
                    &self.serial_socket().display().to_string(),
                );
            if arg.starts_with("user,id=wifi,") {
                arg.push_str(&format!(
                    ",hostfwd=tcp:127.0.0.1:{}-:2222",
                    self.ssh_port + 2
                ));
            }
        }
        if self.bootloader()? == Bootloader::Barebox {
            let kernel = self.root.join("firmware").join(MODEL).join("uImage");
            argv.extend([
                "-device".into(),
                format!(
                    "loader,file={},addr={KERNEL_IN_RAM:#x},force-raw=on",
                    kernel.display()
                ),
            ]);
        }
        Ok(argv)
    }

    fn run(&self, window: bool) -> anyhow::Result<()> {
        ensure!(
            self.machine().join("machine.json").is_file(),
            "no instance {}: run cargo xtask emulate setup",
            self.name
        );
        if self.running() {
            println!("instance {} is running", self.name);
        } else {
            self.launch(window)?;
        }
        self.wait_ssh(Duration::from_secs(600))?;
        println!(
            "SSH: ssh -i {} -p {} root@127.0.0.1",
            self.key().display(),
            self.ssh_port
        );
        Ok(())
    }

    fn launch(&self, window: bool) -> anyhow::Result<()> {
        let argv = self.command(window)?;
        let machine = self.machine();
        // What `eink run` does before it hands over to QEMU.
        std::fs::create_dir_all(self.sockets())?;
        let _ = std::fs::remove_file(self.serial_socket());
        let _ = std::fs::remove_file(self.qmp_socket());
        let log = self.serial_log();
        if log.exists() {
            std::fs::rename(&log, log.with_extension("log.previous"))?;
        }
        let qemu_log = std::fs::File::create(machine.join("qemu.log"))?;
        let child = Command::new(&argv[0])
            .args(&argv[1..])
            .current_dir(&self.root)
            .stdin(Stdio::null())
            .stdout(qemu_log.try_clone()?)
            .stderr(qemu_log)
            // Its own process group: a Ctrl-C here doesn't reach it.
            .process_group(0)
            .spawn()
            .with_context(|| format!("starting {}", argv[0]))?;
        println!(
            "started {} (QEMU pid {}); serial log {}",
            self.name,
            child.id(),
            log.display()
        );
        let since = Instant::now();
        while !self.running() {
            ensure!(
                since.elapsed() < Duration::from_secs(20),
                "QEMU didn't open its QMP socket; see {}",
                machine.join("qemu.log").display()
            );
            std::thread::sleep(Duration::from_millis(250));
        }
        self.boot()
    }

    /// With barebox: stops its autoboot (which looks for the kernel on the
    /// eMMC, in vain) and boots the kernel in RAM, the way the PW1's U-Boot
    /// would: ATAGs, the machine type, and the DDR type the kernel reads
    /// from the DDR controller, which U-Boot programs and barebox, loaded
    /// into RAM already set up, doesn't. Without that the kernel's idle
    /// loop calls a function it never set up.
    fn boot(&self) -> anyhow::Result<()> {
        if self.bootloader()? != Bootloader::Barebox {
            return Ok(());
        }
        let size = std::fs::metadata(self.root.join("firmware").join(MODEL).join("uImage"))?.len();
        let mut console = Console::connect(&self.serial_socket())?;
        // Any key stops the autoboot, whose banner may come before we listen;
        // at its prompt (or Linux's console, still going down) keys do no harm.
        let since = Instant::now();
        loop {
            console.send("\r")?;
            if console.saw("barebox@", Duration::from_secs(2))? {
                break;
            }
            ensure!(
                since.elapsed() < Duration::from_secs(300),
                "no barebox prompt; see {}",
                self.serial_log().display()
            );
        }
        for command in [
            "touch /tmp/uImage".to_owned(),
            format!("memcpy -s /dev/mem -d /tmp/uImage {KERNEL_IN_RAM:#x} 0 {size}"),
            // DATABAHN CTL0: LPDDR2, as the PW1's U-Boot leaves it.
            "mw 0x14000000 0x00000501".to_owned(),
            "global.bootm.boot_atag=true".to_owned(),
            // MACH_MX50_YOSHIME, as in barebox's own boot entry for it.
            "armlinux_architecture=2955".to_owned(),
        ] {
            console.send(&format!("{command}\n"))?;
            console.expect("barebox@", Duration::from_secs(30))?;
        }
        console.send("bootm /tmp/uImage\n")?;
        // Its last words before the kernel's.
        console.expect("arch_number:", Duration::from_secs(60))?;
        println!("barebox started the kernel");
        Ok(())
    }

    fn wait_ssh(&self, limit: Duration) -> anyhow::Result<()> {
        let device = self.device();
        let since = Instant::now();
        while !device.reachable() {
            ensure!(
                self.running(),
                "the instance stopped; see {}",
                self.serial_log().display()
            );
            ensure!(
                since.elapsed() < limit,
                "no SSH after {} s; see {}",
                limit.as_secs(),
                self.serial_log().display()
            );
            std::thread::sleep(Duration::from_secs(5));
        }
        Ok(())
    }

    /// Waits until `check` (a guest shell command) succeeds.
    fn wait_for(&self, what: &str, check: &str, limit: Duration) -> anyhow::Result<()> {
        let device = self.device();
        let since = Instant::now();
        loop {
            if device
                .run_once(&format!("({check}) >/dev/null 2>&1 && echo yes"))
                .is_ok_and(|o| o.trim() == "yes")
            {
                println!("{what} after {} s", since.elapsed().as_secs());
                return Ok(());
            }
            ensure!(
                self.running(),
                "the instance stopped; see {}",
                self.serial_log().display()
            );
            ensure!(
                since.elapsed() < limit,
                "no {what} after {} s; see {}",
                limit.as_secs(),
                self.serial_log().display()
            );
            std::thread::sleep(Duration::from_secs(5));
        }
    }

    /// Waits until the panel stops changing: the same pixels 10 s apart.
    fn wait_panel(&self, limit: Duration) -> anyhow::Result<()> {
        let since = Instant::now();
        let mut last = self.panel()?;
        loop {
            std::thread::sleep(Duration::from_secs(10));
            let now = self.panel()?;
            if now == last {
                return Ok(());
            }
            ensure!(since.elapsed() < limit, "the panel didn't settle");
            last = now;
        }
    }

    fn stop(&self) -> anyhow::Result<()> {
        if !self.running() {
            println!("instance {} isn't running", self.name);
            return Ok(());
        }
        // QMP's quit is a power cut: write the guest's caches out first.
        let _ = self.device().run_once("sync");
        Qmp::connect(&self.qmp_socket())?.execute(r#"{"execute":"quit"}"#)?;
        let since = Instant::now();
        while self.running() && since.elapsed() < Duration::from_secs(10) {
            std::thread::sleep(Duration::from_millis(250));
        }
        println!("stopped {}", self.name);
        Ok(())
    }

    /// The panel's pixels, as QEMU's PPM screendump.
    fn panel(&self) -> anyhow::Result<Vec<u8>> {
        let ppm = self.sockets().join("screendump.ppm");
        let _ = std::fs::remove_file(&ppm);
        Qmp::connect(&self.qmp_socket())
            .context("the instance isn't running")?
            .execute(&format!(
                r#"{{"execute":"screendump","arguments":{{"filename":{},"format":"ppm"}}}}"#,
                json_string(&ppm.display().to_string())
            ))?;
        let pixels = std::fs::read(&ppm)?;
        let _ = std::fs::remove_file(&ppm);
        Ok(pixels)
    }

    fn screenshot(&self, path: &Path) -> anyhow::Result<()> {
        let png = ppm_to_png(&self.panel()?)?;
        if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(path, png)?;
        println!("screenshot: {}", path.display());
        Ok(())
    }

    /// A finger down and up at panel pixel (`x`, `y`): QEMU's absolute
    /// pointer, which the emulated cyttsp reports, spans the panel.
    fn tap(&self, x: u32, y: u32) -> anyhow::Result<()> {
        let (x, y) = (pointer(x, PANEL.0)?, pointer(y, PANEL.1)?);
        let mut qmp = Qmp::connect(&self.qmp_socket()).context("the instance isn't running")?;
        qmp.execute(&format!(
            r#"{{"execute":"input-send-event","arguments":{{"events":[{{"type":"abs","data":{{"axis":"x","value":{x}}}}},{{"type":"abs","data":{{"axis":"y","value":{y}}}}},{{"type":"btn","data":{{"down":true,"button":"left"}}}}]}}}}"#
        ))?;
        std::thread::sleep(Duration::from_millis(150));
        qmp.execute(
            r#"{"execute":"input-send-event","arguments":{"events":[{"type":"btn","data":{"down":false,"button":"left"}}]}}"#,
        )?;
        Ok(())
    }

    /// kindil's launcher files and binary, as `cargo xtask deploy` copies
    /// them, and what a first start needs, where there is none yet: example
    /// settings (the dashboard needs a place), the emulator's only access
    /// point, open `Kindle-QEMU`, for kindil's own Wi-Fi, the emulator's key
    /// in USBNetwork's list (which seeds kindil's SSH server's), and "sleep
    /// when idle: never", as a nap's resume loses the emulated USB link.
    fn install(&self, binary: &Path) -> anyhow::Result<()> {
        ensure!(
            self.running(),
            "start the instance first: cargo xtask emulate run"
        );
        let device = self.device();
        dev::install_files(&device, binary)?;
        let staged = self.sockets().join("install");
        std::fs::create_dir_all(&staged)?;
        let file = |name: &str, text: String| -> anyhow::Result<PathBuf> {
            let path = staged.join(name);
            std::fs::write(&path, text)?;
            Ok(path)
        };
        // The example's paths are the repo's; on the device the defaults do.
        let kindil_toml = file(
            "kindil.toml",
            std::fs::read_to_string("examples/kindil.toml")?
                .lines()
                .filter(|l| !l.starts_with("miqat_config") && !l.starts_with("books"))
                .map(|l| format!("{l}\n"))
                .collect(),
        )?;
        let prefs = file("prefs.toml", "auto_sleep = \"never\"\n".into())?;
        let networks = file(
            "networks.toml",
            "[[network]]\nssid = \"Kindle-QEMU\"\n".into(),
        )?;
        let keys = file(
            "authorized_keys",
            std::fs::read_to_string(self.key().with_extension("pub"))?,
        )?;
        let miqat = PathBuf::from("examples/miqat.toml");
        for (path, dir, mode) in [
            (&miqat, "/mnt/us/kindil", None),
            (&kindil_toml, "/mnt/us/kindil", None),
            (&prefs, "/mnt/us/kindil", None),
            (&networks, "/var/local/kindil", Some("600")),
            (&keys, "/mnt/us/usbnet/etc", None),
        ] {
            let name = path.file_name().context("a file")?.to_string_lossy();
            let target = format!("{dir}/{name}");
            if device
                .run_once(&format!("[ -e {target} ] && echo yes"))?
                .trim()
                == "yes"
            {
                continue;
            }
            device.run(&format!("mkdir -p {dir}"))?;
            device.put(&[path], dir)?;
            if let Some(mode) = mode {
                device.run(&format!("chmod {mode} {target}"))?;
            }
        }
        let _ = std::fs::remove_dir_all(&staged);
        println!("installed; start it with: /mnt/us/extensions/kindil/bin/start.sh");
        Ok(())
    }

    fn test(&self, binary: &Path) -> anyhow::Result<()> {
        let out = PathBuf::from("dist/emulator");
        std::fs::create_dir_all(&out)?;
        // A clean instance: the prepared image, nothing of earlier runs.
        if self.running() {
            self.stop()?;
        }
        self.setup(false)?;
        run(self.eink().args(["reset", &self.name]))?;
        self.launch(false)?;
        self.wait_ssh(Duration::from_secs(600))?;
        // Started the way KUAL starts it: over Amazon's UI, once it is up.
        self.wait_for(
            "Amazon's UI",
            "status framework | grep -q start/running",
            Duration::from_secs(900),
        )?;
        self.wait_panel(Duration::from_secs(300))?;
        self.screenshot(&out.join("stock.png"))?;
        self.install(binary)?;
        let device = self.device();
        print!(
            "{}",
            device.run_once("/mnt/us/extensions/kindil/bin/start.sh; echo start.sh: $?")?
        );
        self.wait_dashboard()?;
        self.screenshot(&out.join("dashboard.png"))?;
        self.check_log()?;
        // kindil's own Wi-Fi joined, and its SSH server answers over it.
        let answer = self.kindil_sshd().run_once("echo kindil-sshd")?;
        ensure!(
            answer.trim() == "kindil-sshd",
            "kindil's SSH server over Wi-Fi said {answer:?}"
        );
        println!("kindil's SSH server answers over the emulated Wi-Fi");

        // Start at boot, standalone: Amazon's framework never starts.
        print!(
            "{}",
            device.run_once(
                "sh /mnt/us/extensions/kindil/boot/install.sh install && \
                 /mnt/us/extensions/kindil/bin/kindil boot-mode standalone"
            )?
        );
        device.run_once(": > /var/local/kindil/kindil.log; sync")?;
        let _ = device.run_once("(sleep 2; reboot) >/dev/null 2>&1 &");
        self.boot()?;
        self.wait_ssh(Duration::from_secs(600))?;
        self.wait_dashboard()?;
        self.screenshot(&out.join("standalone.png"))?;
        let framework = device.run_once("status framework; status x")?;
        print!("{framework}");
        ensure!(
            !framework.contains("start/running"),
            "Amazon's framework or X runs after a standalone boot"
        );
        self.check_log()?;
        println!("emulator test passed; screenshots in {}", out.display());
        Ok(())
    }

    /// kindil runs, its Wi-Fi is up, and the panel has settled.
    fn wait_dashboard(&self) -> anyhow::Result<()> {
        self.wait_for(
            "kindil and its Wi-Fi",
            "pidof kindil && grep -q 'Wi-Fi up' /var/local/kindil/kindil.log",
            Duration::from_secs(300),
        )?;
        self.wait_panel(Duration::from_secs(120))
    }

    /// The log since kindil started holds no error and no panic; network
    /// lines are left out, as the first fetches can come before the network.
    fn check_log(&self) -> anyhow::Result<()> {
        let errors = self.device().run_once(
            // The PW1's BusyBox grep has no -E.
            "grep -i -e panic -e error -e fail /var/local/kindil/kindil.log | \
             grep -v -e weather: -e radar: -e clock: | tail -n 20",
        )?;
        if !errors.trim().is_empty() {
            bail!("kindil.log has errors:\n{errors}");
        }
        Ok(())
    }
}

/// The PW1's panel as the emulator shows it, portrait.
const PANEL: (u32, u32) = (758, 1024);

/// A panel coordinate on QEMU's absolute pointer scale, 0 to 0x7fff.
fn pointer(at: u32, size: u32) -> anyhow::Result<u32> {
    ensure!(at < size, "{at} is off the panel (0 to {})", size - 1);
    Ok((at * 0x7fff + (size - 1) / 2) / (size - 1))
}

/// The device binary, built by the flake (works on macOS too: Zig links it).
fn build() -> anyhow::Result<PathBuf> {
    let output = Command::new("nix")
        .args(["build", "--no-link", "--print-out-paths", ".#kindil-device"])
        .stderr(Stdio::inherit())
        .output()
        .context("running nix build .#kindil-device")?;
    ensure!(output.status.success(), "nix build .#kindil-device failed");
    let out = String::from_utf8(output.stdout)?;
    Ok(PathBuf::from(out.trim()).join("bin/kindil"))
}

/// `create` prepares the rootfs with e2fsprogs' tools.
fn need_tools() -> anyhow::Result<()> {
    for tool in ["debugfs", "mke2fs", "e2fsck"] {
        let found = std::env::var_os("PATH")
            .is_some_and(|path| std::env::split_paths(&path).any(|d| d.join(tool).is_file()));
        ensure!(
            found,
            "{tool} isn't on PATH: the emulator prepares images with e2fsprogs \
             (brew install e2fsprogs, and its sbin on PATH)"
        );
    }
    Ok(())
}

fn copy_if_changed(from: &Path, to: &Path) -> anyhow::Result<()> {
    let same = match (std::fs::metadata(from), std::fs::metadata(to)) {
        (Ok(a), Ok(b)) => a.len() == b.len() && a.modified()? <= b.modified()?,
        _ => false,
    };
    if !same {
        let part = to.with_extension("part");
        std::fs::copy(from, &part).with_context(|| format!("copying {}", from.display()))?;
        std::fs::rename(&part, to)?;
    }
    Ok(())
}

/// The guest's serial console, through QEMU's socket (its log keeps
/// everything said on it).
struct Console {
    stream: UnixStream,
    seen: String,
}

impl Console {
    fn connect(socket: &Path) -> anyhow::Result<Self> {
        let stream = UnixStream::connect(socket)
            .with_context(|| format!("connecting to {}", socket.display()))?;
        stream.set_read_timeout(Some(Duration::from_millis(500)))?;
        Ok(Self {
            stream,
            seen: String::new(),
        })
    }

    fn send(&mut self, text: &str) -> anyhow::Result<()> {
        self.seen.clear();
        Ok(self.stream.write_all(text.as_bytes())?)
    }

    /// Reads until `text` has been said since the last [`Console::send`].
    fn expect(&mut self, text: &str, limit: Duration) -> anyhow::Result<()> {
        ensure!(
            self.saw(text, limit)?,
            "the console didn't say {text:?} in {} s",
            limit.as_secs()
        );
        Ok(())
    }

    /// Whether `text` is said, since the last [`Console::send`], within `limit`.
    fn saw(&mut self, text: &str, limit: Duration) -> anyhow::Result<bool> {
        let since = Instant::now();
        let mut buffer = [0u8; 4096];
        while !self.seen.contains(text) {
            if since.elapsed() >= limit {
                return Ok(false);
            }
            match self.stream.read(&mut buffer) {
                Ok(0) => bail!("the console closed"),
                Ok(n) => self.seen.push_str(&String::from_utf8_lossy(&buffer[..n])),
                Err(e)
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) => {}
                Err(e) => return Err(e.into()),
            }
        }
        Ok(true)
    }
}

/// A QMP connection: the greeting read and capabilities negotiated.
struct Qmp {
    reader: BufReader<UnixStream>,
    writer: UnixStream,
}

impl Qmp {
    fn connect(socket: &Path) -> anyhow::Result<Self> {
        let stream = UnixStream::connect(socket)?;
        stream.set_read_timeout(Some(Duration::from_secs(30)))?;
        let mut qmp = Self {
            reader: BufReader::new(stream.try_clone()?),
            writer: stream,
        };
        let mut greeting = String::new();
        qmp.reader.read_line(&mut greeting)?;
        ensure!(greeting.contains("QMP"), "not a QMP socket");
        qmp.execute(r#"{"execute":"qmp_capabilities"}"#)?;
        Ok(qmp)
    }

    /// Sends one command; its answer, skipping events.
    fn execute(&mut self, command: &str) -> anyhow::Result<String> {
        writeln!(self.writer, "{command}")?;
        loop {
            let mut line = String::new();
            if self.reader.read_line(&mut line)? == 0 {
                // quit closes the socket as it answers.
                return Ok(line);
            }
            if line.contains("\"return\"") {
                return Ok(line);
            }
            if line.contains("\"error\"") {
                bail!("QMP {command}: {}", line.trim());
            }
        }
    }
}

fn json_string(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Splits a command line as Python's `shlex.join` writes it.
fn split(line: &str) -> anyhow::Result<Vec<String>> {
    let mut words = Vec::new();
    let mut word: Option<String> = None;
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        match c {
            ' ' | '\t' => {
                if let Some(w) = word.take() {
                    words.push(w);
                }
            }
            '\'' => {
                let w = word.get_or_insert_with(String::new);
                loop {
                    match chars.next() {
                        Some('\'') => break,
                        Some(c) => w.push(c),
                        None => bail!("unterminated quote in {line}"),
                    }
                }
            }
            '"' => {
                let w = word.get_or_insert_with(String::new);
                loop {
                    match chars.next() {
                        Some('"') => break,
                        Some('\\') => w.extend(chars.next()),
                        Some(c) => w.push(c),
                        None => bail!("unterminated quote in {line}"),
                    }
                }
            }
            '\\' => word.get_or_insert_with(String::new).extend(chars.next()),
            c => word.get_or_insert_with(String::new).push(c),
        }
    }
    words.extend(word);
    Ok(words)
}

/// QEMU's binary PPM (P6, 8 bits) as a PNG, so the screendump needs no
/// libpng in QEMU's build.
fn ppm_to_png(ppm: &[u8]) -> anyhow::Result<Vec<u8>> {
    // Header: P6, width, height, maxval, each after whitespace.
    let mut fields = Vec::new();
    let mut at = 0;
    while fields.len() < 4 {
        while at < ppm.len() && ppm[at].is_ascii_whitespace() {
            at += 1;
        }
        let start = at;
        while at < ppm.len() && !ppm[at].is_ascii_whitespace() {
            at += 1;
        }
        ensure!(start < at, "short PPM header");
        fields.push(std::str::from_utf8(&ppm[start..at])?.to_owned());
    }
    ensure!(
        fields[0] == "P6" && fields[3] == "255",
        "not an 8-bit P6 PPM"
    );
    let width: u32 = fields[1].parse()?;
    let height: u32 = fields[2].parse()?;
    let pixels = &ppm[at + 1..];
    let row = width as usize * 3;
    ensure!(pixels.len() >= row * height as usize, "short PPM");

    let mut raw = Vec::with_capacity((row + 1) * height as usize);
    for line in pixels.chunks(row).take(height as usize) {
        raw.push(0); // no filter
        raw.extend_from_slice(line);
    }
    let mut zlib = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
    zlib.write_all(&raw)?;
    let idat = zlib.finish()?;

    let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut ihdr = Vec::new();
    ihdr.extend(width.to_be_bytes());
    ihdr.extend(height.to_be_bytes());
    ihdr.extend([8, 2, 0, 0, 0]); // 8-bit RGB
    for (kind, data) in [
        (b"IHDR", &ihdr[..]),
        (b"IDAT", &idat[..]),
        (b"IEND", &[][..]),
    ] {
        png.extend((data.len() as u32).to_be_bytes());
        let mut crc = flate2::Crc::new();
        crc.update(kind);
        crc.update(data);
        png.extend(kind);
        png.extend(data);
        png.extend(crc.sum().to_be_bytes());
    }
    Ok(png)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_shlex_join() {
        assert_eq!(
            split(
                "/q/qemu-system-arm -machine imx50-celeste \
                 -drive 'file=/a b/disk.qcow2,if=sd' -name 'it'\"'\"'s'"
            )
            .unwrap(),
            [
                "/q/qemu-system-arm",
                "-machine",
                "imx50-celeste",
                "-drive",
                "file=/a b/disk.qcow2,if=sd",
                "-name",
                "it's"
            ]
        );
        assert!(split("'open").is_err());
    }

    #[test]
    fn converts_a_ppm() {
        let png = ppm_to_png(b"P6\n2 1\n255\n\x00\x00\x00\xff\xff\xff").unwrap();
        assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
        assert_eq!(&png[12..16], b"IHDR");
        assert_eq!(&png[16..24], &[0, 0, 0, 2, 0, 0, 0, 1]);
        assert!(png.ends_with(&[0xae, 0x42, 0x60, 0x82])); // IEND's CRC
        assert!(ppm_to_png(b"P5\n2 1\n255\n\x00\x00").is_err());
    }

    #[test]
    fn tells_barebox_from_u_boot() {
        let mut image = vec![0u8; 64];
        assert_eq!(Bootloader::of(&image), Bootloader::UBoot);
        image[0x20..0x27].copy_from_slice(b"barebox");
        assert_eq!(Bootloader::of(&image), Bootloader::Barebox);
        assert_eq!(Bootloader::of(b"short"), Bootloader::UBoot);
    }

    #[test]
    fn scales_taps_to_the_pointer() {
        assert_eq!(pointer(0, 758).unwrap(), 0);
        assert_eq!(pointer(757, 758).unwrap(), 0x7fff);
        assert_eq!(pointer(511, 1023 + 1).unwrap(), 0x3fef);
        assert!(pointer(1024, 1024).is_err());
    }

    #[test]
    fn quotes_json() {
        assert_eq!(json_string("/a \"b\"\\c"), r#""/a \"b\"\\c""#);
    }
}
