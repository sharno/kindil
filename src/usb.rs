//! [`crate::drive`] on the PW1: `/proc`, sysfs, Amazon's own tools
//! (`modprobe`, `losetup`, `mount`, `fsp`, `fsck.vfat`), and the thread that
//! runs USB drive mode from kernel uevents.
use crate::drive::{self, Driver, Gadget, Holder, Mount, Name, Ops, Ui};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant};

/// The mass storage gadget's LUN, as volumd's config names it
/// (`kdb get system/daemon/volumd/SYS_MOUNT_FILE_NAME`).
const LUN: &str = "/sys/devices/platform/fsl-usb2-udc/gadget/gadget-lun0/file";
/// `1` while a cable is in: a computer or a wall charger.
const CONNECTED: &str = "/sys/devices/platform/aplite_charger.0/connected";
/// USBNetwork's `g_ether` parameters, kept to load it again as it was.
const ETHER_PARAMETERS: &str = "/sys/module/g_ether/parameters";
/// Where the FAT32 starts in [`drive::DEVICE`] (Amazon's `mntus`).
const OFFSET: &str = "8192";
/// `/etc/fstab`'s options for `/mnt/base-us` and for `fsp`'s `/mnt/us`, as
/// the running `fsp` shows them.
const BASE_OPTIONS: &str = "noatime,nodiratime,utf8,noexec,uid=0,gid=0,umask=0,shortname=mixed";
const FSP_OPTIONS: &str = "rw,nosuid,nodev,noatime,allow_other,umask=0,uid=0,gid=0,\
                           max_write=65536,max_readahead=65536,nonempty";
const PATH: &str = "/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin";
/// How long volumd gets to go after the launcher stops it.
const VOLUMD_GONE: Duration = Duration::from_secs(120);
/// How long the UI gets to let go of the storage.
const RELEASE: Duration = Duration::from_secs(10);
/// How often to look at the LUN and the cable while a computer has the
/// storage.
const POLL: Duration = Duration::from_secs(1);

/// The PW1's storage and gadget.
#[derive(Default)]
pub struct Device {
    /// `g_ether`'s parameters from before it was unloaded.
    ether: Mutex<Vec<String>>,
}

/// `program args`, its output into the log; fails unless it exits 0.
fn run(program: &str, args: &[&str]) -> std::io::Result<()> {
    let output = Command::new(program)
        .args(args)
        .env("PATH", PATH)
        .stdin(Stdio::null())
        .output()?;
    let text = String::from_utf8_lossy(&output.stderr);
    if !text.trim().is_empty() {
        crate::log!("kindil: {program}: {}", text.trim());
    }
    if output.status.success() {
        Ok(())
    } else {
        Err(std::io::Error::other(format!(
            "{program} {} exited with {}",
            args.join(" "),
            output.status
        )))
    }
}

/// Whether `path` is `mount` or under it.
fn under(path: &str, mount: &str) -> bool {
    path.strip_prefix(mount)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
}

/// The process name in `/proc/PID/stat`: "pid (name) state ...". 2.6.31
/// has no `/proc/PID/comm`.
pub fn stat_name(stat: &str) -> Option<&str> {
    let start = stat.find('(')? + 1;
    let end = stat.rfind(')')?;
    stat.get(start..end)
}

/// The path in a line of `/proc/PID/maps`.
pub fn maps_path(line: &str) -> Option<&str> {
    line.split_whitespace().nth(5)
}

/// Whether `/proc/mounts` mounts something on `path`.
pub fn mounts_on(mounts: &str, path: &str) -> bool {
    mounts
        .lines()
        .any(|line| line.split_whitespace().nth(1) == Some(path))
}

/// The gadget `/proc/modules` lists.
pub fn loaded_gadget(modules: &str) -> Gadget {
    let names: Vec<&str> = modules
        .lines()
        .filter_map(|line| line.split_whitespace().next())
        .collect();
    if names.contains(&"g_file_storage") {
        Gadget::Storage
    } else if names.contains(&"g_ether") {
        Gadget::Ether
    } else {
        Gadget::None
    }
}

/// Processes whose name is `name`.
fn pids_named(name: &str) -> Vec<u32> {
    std::fs::read_dir("/proc")
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| {
            let pid: u32 = entry.file_name().to_str()?.parse().ok()?;
            let stat = std::fs::read_to_string(entry.path().join("stat")).ok()?;
            (stat_name(&stat)? == name).then_some(pid)
        })
        .collect()
}

impl Device {
    fn mount_base(&self, read_only: bool) -> std::io::Result<()> {
        // Amazon's mntus attaches the loop device on each mount; unmounting
        // leaves it attached.
        if run("losetup", &[drive::LOOP]).is_err() {
            run("losetup", &["-o", OFFSET, drive::LOOP, drive::DEVICE])?;
        }
        let options = format!("{BASE_OPTIONS}{}", if read_only { ",ro" } else { "" });
        run(
            "mount",
            &[
                "-t",
                "vfat",
                "-o",
                &options,
                drive::LOOP,
                Mount::Base.path(),
            ],
        )
    }

    fn ether_parameters(&self) -> Vec<String> {
        let mut parameters: Vec<String> = std::fs::read_dir(ETHER_PARAMETERS)
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|entry| {
                let value = std::fs::read_to_string(entry.path()).ok()?;
                Some(format!(
                    "{}={}",
                    entry.file_name().to_string_lossy(),
                    value.trim()
                ))
            })
            .collect();
        parameters.sort();
        parameters
    }
}

impl Ops for Device {
    fn sync(&self) {
        // SAFETY: sync(2) takes nothing and can't fail.
        unsafe { libc::sync() };
    }

    fn holders(&self, mount: Mount) -> Vec<Holder> {
        let path = mount.path();
        let mut holders = Vec::new();
        for entry in std::fs::read_dir("/proc").into_iter().flatten().flatten() {
            let Some(pid) = entry
                .file_name()
                .to_str()
                .and_then(|n| n.parse::<u32>().ok())
            else {
                continue;
            };
            let dir = entry.path();
            let link = |name: &str| {
                std::fs::read_link(dir.join(name))
                    .ok()
                    .map(|p| p.to_string_lossy().into_owned())
            };
            let fds = std::fs::read_dir(dir.join("fd"))
                .into_iter()
                .flatten()
                .flatten()
                .filter_map(|fd| std::fs::read_link(fd.path()).ok())
                .map(|p| p.to_string_lossy().into_owned());
            let maps = std::fs::read_to_string(dir.join("maps")).unwrap_or_default();
            let mapped = maps.lines().filter_map(maps_path).map(str::to_owned);
            let held = ["cwd", "root", "exe"]
                .into_iter()
                .filter_map(link)
                .chain(fds)
                .chain(mapped)
                .find(|p| under(p, path));
            if let Some(held) = held {
                let stat = std::fs::read_to_string(dir.join("stat")).unwrap_or_default();
                holders.push(Holder {
                    pid,
                    name: Name::new(stat_name(&stat).unwrap_or("?")),
                    path: held,
                });
            }
        }
        holders
    }

    fn mounted(&self, mount: Mount) -> bool {
        std::fs::read_to_string("/proc/mounts").is_ok_and(|text| mounts_on(&text, mount.path()))
    }

    fn umount(&self, mount: Mount) -> std::io::Result<()> {
        // Never `-l`: a lazy unmount would hand the computer a FAT still in use.
        run("umount", &[mount.path()])
    }

    fn mount(
        &self,
        mount: Mount,
        read_only: bool,
        _: &drive::proof::LunEmpty,
    ) -> std::io::Result<()> {
        match mount {
            Mount::Base => self.mount_base(read_only),
            // fsp forks into the background once mounted, and outlives
            // kindil: no pipes for it to hold open.
            Mount::Fsp => {
                let status = Command::new("fsp")
                    .args([Mount::Base.path(), Mount::Fsp.path(), "-o", FSP_OPTIONS])
                    .env("PATH", PATH)
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status()?;
                if status.success() && self.mounted(Mount::Fsp) {
                    Ok(())
                } else {
                    Err(std::io::Error::other(format!("fsp exited with {status}")))
                }
            }
        }
    }

    fn gadget(&self) -> Gadget {
        loaded_gadget(&std::fs::read_to_string("/proc/modules").unwrap_or_default())
    }

    fn load(&self, gadget: Gadget) -> std::io::Result<()> {
        match gadget {
            Gadget::None => Ok(()),
            Gadget::Storage => run("modprobe", &["g_file_storage", "removable=y"]),
            Gadget::Ether => {
                let parameters = self.ether.lock().map(|p| p.clone()).unwrap_or_default();
                let mut args = vec!["g_ether"];
                args.extend(parameters.iter().map(String::as_str));
                run("modprobe", &args)
            }
        }
    }

    fn unload(&self, gadget: Gadget) -> std::io::Result<()> {
        if gadget == Gadget::Ether {
            let parameters = self.ether_parameters();
            if let Ok(mut kept) = self.ether.lock() {
                *kept = parameters;
            }
        }
        match gadget.module() {
            Some(module) => run("modprobe", &["-r", module]),
            None => Ok(()),
        }
    }

    fn lun(&self) -> std::io::Result<Option<String>> {
        match std::fs::read_to_string(LUN) {
            Ok(text) => Ok(Some(text.trim().to_owned())),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error),
        }
    }

    fn export_lun(&self, _: &drive::proof::NotMounted) -> std::io::Result<()> {
        std::fs::write(LUN, format!("{}\n", drive::DEVICE))
    }

    fn clear_lun(&self) -> std::io::Result<()> {
        std::fs::write(LUN, "\n")
    }

    fn flush(&self) {
        #[cfg(target_os = "linux")]
        {
            use std::os::fd::AsRawFd;
            /// `_IO(0x12, 97)`.
            const BLKFLSBUF: libc::c_ulong = 0x1261;
            if let Ok(device) = std::fs::File::open(drive::LOOP) {
                // SAFETY: an ioctl without an argument on an open fd.
                unsafe { libc::ioctl(device.as_raw_fd(), BLKFLSBUF as _, 0) };
            }
        }
    }

    fn fsck(&self) -> std::io::Result<i32> {
        let output = Command::new("fsck.vfat")
            .args(["-a", "-v", drive::LOOP])
            .env("PATH", PATH)
            .stdin(Stdio::null())
            .output()?;
        for line in String::from_utf8_lossy(&output.stdout)
            .lines()
            .chain(String::from_utf8_lossy(&output.stderr).lines())
        {
            crate::log!("kindil: fsck.vfat: {line}");
        }
        output
            .status
            .code()
            .ok_or_else(|| std::io::Error::other("fsck.vfat killed"))
    }

    fn exported(&self) -> bool {
        Path::new(drive::EXPORTED).exists()
    }

    fn set_exported(&self, on: bool) -> std::io::Result<()> {
        if on {
            let file = std::fs::File::create(drive::EXPORTED)?;
            // Through to the disk: it is there for a yank and a reboot.
            file.sync_all()
        } else {
            match std::fs::remove_file(drive::EXPORTED) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
                _ => Ok(()),
            }
        }
    }

    fn plugged(&self) -> bool {
        std::fs::read_to_string(CONNECTED).is_ok_and(|text| text.trim() == "1")
    }

    fn wait(&self, time: Duration) {
        std::thread::sleep(time);
    }
}

/// The UI's acknowledgement of [`crate::system::Event::StorageLeaving`]:
/// how many times it released the storage.
#[derive(Default)]
pub struct Released {
    count: Mutex<u64>,
    changed: Condvar,
}

impl Released {
    pub fn notify(&self) {
        let mut count = self.count.lock().unwrap_or_else(|p| p.into_inner());
        *count += 1;
        self.changed.notify_all();
    }

    fn count(&self) -> u64 {
        *self.count.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Waits for a release after `seen`; false after `time`.
    fn wait_past(&self, seen: u64, time: Duration) -> bool {
        let count = self.count.lock().unwrap_or_else(|p| p.into_inner());
        let (count, _) = self
            .changed
            .wait_timeout_while(count, time, |count| *count <= seen)
            .unwrap_or_else(|p| p.into_inner());
        *count > seen
    }
}

struct Screen<'a> {
    deliver: &'a (dyn Fn(crate::system::Event) + Send + Sync),
    released: &'a Released,
}

impl Ui for Screen<'_> {
    fn release(&self) {
        let seen = self.released.count();
        (self.deliver)(crate::system::Event::StorageLeaving);
        if !self.released.wait_past(seen, RELEASE) {
            crate::log!("kindil: USB drive: the UI didn't release the storage in time");
        }
    }

    fn deliver(&self, event: crate::system::Event) {
        (self.deliver)(event);
    }
}

/// Whether Amazon's volumd still runs; the launcher stops it in the
/// background once kindil runs.
pub fn volumd_running() -> bool {
    !pids_named("volumd").is_empty()
}

/// Waits for volumd to go; false if it is still there after [`VOLUMD_GONE`]
/// (a launcher from before kindil ran USB drive mode, or a stop that failed).
pub fn volumd_gone() -> bool {
    let since = Instant::now();
    while volumd_running() {
        if since.elapsed() > VOLUMD_GONE {
            return false;
        }
        std::thread::sleep(Duration::from_secs(1));
    }
    true
}

/// Runs USB drive mode for good, from kernel uevents.
pub fn run_drive(
    deliver: &(dyn Fn(crate::system::Event) + Send + Sync),
    released: &Released,
) -> anyhow::Result<()> {
    let listener = crate::uevent::Listener::open()?;
    let device = Device::default();
    let ui = Screen { deliver, released };
    let mut driver = Driver::start(&device, &ui);
    loop {
        let wait = match driver.state().due() {
            Some((_, due)) => due.saturating_duration_since(Instant::now()).min(POLL),
            None => POLL,
        };
        if let Some(event) = listener.next_within(wait)? {
            if let Some(signal) = drive::signal(&event) {
                crate::log!("kindil: USB drive: {event}");
                driver.signal(signal, Instant::now(), &ui);
            }
        }
        driver.poll(Instant::now(), &ui);
    }
}

/// `kindil uevents --usb`: a line for the state the USB devices are in.
pub fn describe() -> String {
    let read = |path: &str| {
        std::fs::read_to_string(path).map_or_else(|_| "-".to_owned(), |t| t.trim().to_owned())
    };
    let device = Device::default();
    let mounts = std::fs::read_to_string("/proc/mounts").unwrap_or_default();
    format!(
        "  state: connected={} lun={:?} lun-online={} gadget={:?} mounted[us={} base-us={}] volumd={}",
        read(CONNECTED),
        read(LUN),
        read("/sys/devices/platform/fsl-usb2-udc/gadget/gadget-lun0/online"),
        device.gadget(),
        mounts_on(&mounts, Mount::Fsp.path()),
        mounts_on(&mounts, Mount::Base.path()),
        volumd_running(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_proc() {
        assert_eq!(stat_name("937 (fsp) S 1 937 937 0"), Some("fsp"));
        assert_eq!(stat_name("12 (a (b) c) S 1"), Some("a (b) c"));
        assert_eq!(
            maps_path("40000000-40017000 r-xp 00000000 b3:01 10299      /mnt/us/x.so"),
            Some("/mnt/us/x.so")
        );
        assert_eq!(maps_path("00008000-00010000 rw-p 00000000 00:00 0"), None);
        assert!(under("/mnt/us", "/mnt/us"));
        assert!(under("/mnt/us/books/a.epub", "/mnt/us"));
        assert!(!under("/mnt/usb", "/mnt/us"));
        let mounts = "/dev/loop/0 /mnt/base-us vfat rw 0 0\nfsp /mnt/us fuse.fsp rw 0 0\n";
        assert!(mounts_on(mounts, "/mnt/us") && mounts_on(mounts, "/mnt/base-us"));
        assert!(!mounts_on("/dev/root / ext3 ro 0 0\n", "/mnt/us"));
        let modules = "g_ether 27468 0 - Live 0xbf0\narcotg_udc_yoshime 17436 1 g_ether, Live\n";
        assert_eq!(loaded_gadget(modules), Gadget::Ether);
        assert_eq!(loaded_gadget("g_file_storage 1 0\n"), Gadget::Storage);
        assert_eq!(loaded_gadget("fuse 59208 2\n"), Gadget::None);
    }

    #[test]
    fn a_release_wakes_the_waiting_driver() {
        let released = std::sync::Arc::new(Released::default());
        let seen = released.count();
        let other = released.clone();
        let t = std::thread::spawn(move || other.notify());
        assert!(released.wait_past(seen, Duration::from_secs(5)));
        t.join().unwrap();
        assert!(!released.wait_past(released.count(), Duration::from_millis(10)));
    }
}
