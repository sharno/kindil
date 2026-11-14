//! Wi-Fi, run by kindil itself.
//!
//! With networks of its own (src/networks.rs), kindil owns the whole stack:
//! it loads the ar6003 driver, runs its own wpa_supplicant from its own
//! config, loads its networks into it and does DHCP with its own hook. None
//! of Amazon's Wi-Fi pieces (wifid, and the wifim and wifis jobs it drives)
//! run. Off unloads the driver, which powers the chip down. wpa_supplicant
//! and udhcpc run as kindil's children: one that exits without being told is
//! started again (wpa_supplicant with the networks loaded again), with a
//! growing pause if it keeps dying. After its own suspends kindil rejoins.
//!
//! Amazon's wifid runs Wi-Fi in one case only: a Kindle where kindil has no
//! network yet (first run). Leaving kindil starts it again (bin/session.sh).
use crate::host::Power;
use crate::lipc;
use crate::wifi::{self, Credentials, Link};
use std::process::{Child, Command};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const INTERFACE: &str = "wlan0";
const WPA: &str = "/usr/bin/wpa_supplicant";
/// kindil's own wpa_supplicant config: no networks in it, they are loaded
/// over the control socket from kindil's store.
const WPA_CONF: &str = "/var/local/kindil/wpa_supplicant.conf";
const WPA_CONF_TEXT: &str = "ctrl_interface=/var/run/wpa_supplicant\nap_scan=1\nupdate_config=0\n";
const WPA_PID: &str = "/var/run/kindil-wpa.pid";
/// udhcpc's hook, on the system storage: /mnt/us goes away in USB drive
/// mode, and a renewal then would find no script.
const DHCP_SCRIPT: &str = "/var/local/kindil/dhcp.sh";
const DHCP_PID: &str = "/var/run/kindil-udhcpc.pid";
const SCRIPT: &str = include_str!("../kindle/extensions/kindil/bin/dhcp.sh");
/// The PW1's (yoshime3) calibration data for the AR6003, as Amazon's
/// wifim job finds it from the board id.
const BOARD_DATA: &str = "/opt/ar6k/target/AR6003/hw2.1.1/bin";
const BOARD_FILE: &str = "wfo/AR6003_wfo_calfile.bin";
/// The driver's MAC before it has loaded its firmware.
const NO_MAC: &str = "00:00:00:00:00:00";
/// Loading the driver and its firmware takes a few seconds.
const STARTUP_WAIT: Duration = Duration::from_secs(30);
/// Joining usually takes under a second; a far access point, a few.
const JOIN_WAIT: Duration = Duration::from_secs(15);
/// A new network gets this long to prove its password.
const TRY_WAIT: Duration = Duration::from_secs(30);
/// A child that lived this long was fine; a restart after it waits only
/// the shortest pause.
const HEALTHY: Duration = Duration::from_secs(60);
const PAUSE_MIN: Duration = Duration::from_secs(1);
const PAUSE_MAX: Duration = Duration::from_secs(60);
/// wifid's switch settles asynchronously.
const WIFID_SETTLE: Duration = Duration::from_secs(2);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Owner {
    /// First run: no network of kindil's own yet.
    Amazon,
    Kindil(Power),
}

pub struct Net {
    owner: Mutex<Owner>,
    /// The networks wpa_supplicant was last given: a restarted one gets them.
    saved: Mutex<Vec<Credentials>>,
    /// Bumped whenever kindil stops its stack: children of an older one
    /// exited because they were told to, and stay down.
    generation: AtomicU64,
}

impl Default for Net {
    fn default() -> Self {
        Self {
            owner: Mutex::new(Owner::Amazon),
            saved: Mutex::default(),
            generation: AtomicU64::new(0),
        }
    }
}

impl Net {
    fn owner(&self) -> Owner {
        *self.owner.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn set_owner(&self, owner: Owner) {
        *self.owner.lock().unwrap_or_else(|e| e.into_inner()) = owner;
    }

    /// Runs Wi-Fi with `saved`, kindil's networks: takes it from Amazon the
    /// first time, reloads the list after. With none, Wi-Fi is Amazon's.
    pub fn load(self: &Arc<Self>, saved: &[Credentials]) -> anyhow::Result<()> {
        match self.owner() {
            Owner::Amazon if saved.is_empty() => Ok(()),
            Owner::Amazon => {
                // Amazon's Wi-Fi on means kindil's on; with no wifid, on.
                let power = wifid_power().unwrap_or(Power::On);
                stop_amazon();
                self.set_owner(Owner::Kindil(Power::Off));
                match power {
                    Power::On => self.start(saved),
                    Power::Off => Ok(()),
                }
            }
            Owner::Kindil(_) if saved.is_empty() => self.hand_back(),
            Owner::Kindil(Power::Off) => Ok(()),
            Owner::Kindil(Power::On) => {
                *lock(&self.saved) = saved.to_vec();
                let wpa = wifi::open(INTERFACE)?;
                wpa.load(saved)?;
                wpa.command("REASSOCIATE").map(drop)
            }
        }
    }

    /// Tries `candidate` as a new first network: Ok once it has joined it,
    /// otherwise back to `saved` and an error.
    pub fn try_join(
        self: &Arc<Self>,
        candidate: &[Credentials],
        saved: &[Credentials],
    ) -> anyhow::Result<()> {
        let ssid = candidate
            .first()
            .map(|c| c.ssid().as_str().to_owned())
            .ok_or_else(|| anyhow::anyhow!("no network to join"))?;
        if self.owner() == Owner::Kindil(Power::Off) {
            self.set(Power::On, candidate)?;
        } else {
            self.load(candidate)?;
        }
        let since = Instant::now();
        let wpa = wifi::open(INTERFACE)?;
        if let Some(id) = wpa
            .command("LIST_NETWORKS")
            .ok()
            .and_then(|list| network_id(&list, &ssid))
        {
            // Join this one now, not just whenever it wins.
            wpa.command(&format!("SELECT_NETWORK {id}"))?;
            wpa.command("ENABLE_NETWORK all")?;
        }
        while since.elapsed() < TRY_WAIT {
            if matches!(wpa.link(), Ok(Link::Associated { ssid: ref joined, .. }) if *joined == ssid)
            {
                renew_dhcp()?;
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(250));
        }
        self.load(saved)?;
        anyhow::bail!("couldn't join {ssid}: check the password and that it is in range")
    }

    pub fn power(&self) -> anyhow::Result<Power> {
        match self.owner() {
            Owner::Amazon => wifid_power(),
            Owner::Kindil(power) => Ok(power),
        }
    }

    pub fn set(self: &Arc<Self>, power: Power, saved: &[Credentials]) -> anyhow::Result<()> {
        match self.owner() {
            Owner::Amazon => lipc::set("com.lab126.wifid", "enable", wifid_value(power)),
            Owner::Kindil(_) => match power {
                Power::On => self.start(saved),
                Power::Off => {
                    self.stop();
                    Ok(())
                }
            },
        }
    }

    /// After a suspend: join again, then renew the address (usually kept).
    pub fn reconnect(&self) -> anyhow::Result<()> {
        match self.owner() {
            Owner::Amazon => match wifid_power()? {
                Power::Off => Ok(()),
                Power::On => {
                    lipc::set("com.lab126.wifid", "enable", "0")?;
                    // An immediate switch back can land before it went down.
                    std::thread::sleep(WIFID_SETTLE);
                    lipc::set("com.lab126.wifid", "enable", "1")
                }
            },
            Owner::Kindil(Power::Off) => Ok(()),
            Owner::Kindil(Power::On) => {
                let wpa = wifi::open(INTERFACE)?;
                wpa.command("REASSOCIATE")?;
                let since = Instant::now();
                while !matches!(wpa.link(), Ok(Link::Associated { .. })) {
                    anyhow::ensure!(since.elapsed() < JOIN_WAIT, "no access point joined");
                    std::thread::sleep(Duration::from_millis(100));
                }
                renew_dhcp()
            }
        }
    }

    /// kindil's whole stack up, with `saved` loaded.
    fn start(self: &Arc<Self>, saved: &[Credentials]) -> anyhow::Result<()> {
        // Off even if this fails part way, so On tries it all again.
        self.stop();
        *lock(&self.saved) = saved.to_vec();
        load_driver()?;
        std::fs::write(WPA_CONF, WPA_CONF_TEXT)?;
        std::fs::write(DHCP_SCRIPT, SCRIPT)?;
        set_executable(DHCP_SCRIPT)?;
        let generation = self.generation.load(Ordering::SeqCst);
        self.supervise("wpa_supplicant", generation, |net| {
            let child = spawn_supplicant()?;
            let wpa = wifi::open(INTERFACE)?;
            wpa.load(&lock(&net.saved))?;
            wpa.command("REASSOCIATE")?;
            Ok(child)
        })?;
        self.supervise("udhcpc", generation, |_| spawn_dhcp())?;
        self.set_owner(Owner::Kindil(Power::On));
        Ok(())
    }

    /// Starts a child with `spawn`, and again whenever it exits while this
    /// stack (`generation`) is still wanted.
    fn supervise(
        self: &Arc<Self>,
        name: &'static str,
        generation: u64,
        spawn: fn(&Net) -> anyhow::Result<Child>,
    ) -> anyhow::Result<()> {
        let mut child = spawn(self)?;
        let net = Arc::clone(self);
        let wanted = move || net.generation.load(Ordering::SeqCst) == generation;
        let net = Arc::clone(self);
        std::thread::spawn(move || {
            let mut pause = PAUSE_MIN;
            loop {
                let since = Instant::now();
                let status = child.wait();
                if !wanted() {
                    return;
                }
                pause = if since.elapsed() >= HEALTHY {
                    PAUSE_MIN
                } else {
                    (pause * 2).min(PAUSE_MAX)
                };
                eprintln!("kindil: {name} exited ({status:?}); starting it again in {pause:?}");
                loop {
                    std::thread::sleep(pause);
                    if !wanted() {
                        return;
                    }
                    match spawn(&net) {
                        Ok(next) => {
                            child = next;
                            break;
                        }
                        Err(error) => {
                            eprintln!("kindil: {name}: {error}");
                            pause = (pause * 2).min(PAUSE_MAX);
                        }
                    }
                }
            }
        });
        Ok(())
    }

    /// kindil's stack down and the chip powered off; never fails halfway
    /// into a state On can't recover from.
    fn stop(&self) {
        // Its children now exit for good.
        self.generation.fetch_add(1, Ordering::SeqCst);
        terminate(DHCP_PID);
        terminate(WPA_PID);
        let _ = run(Command::new("/sbin/ifconfig").args([INTERFACE, "down"]));
        let _ = run(Command::new("/sbin/rmmod").arg("ar6003"));
        self.set_owner(Owner::Kindil(Power::Off));
    }

    /// No network of kindil's own left: Wi-Fi goes back to wifid and
    /// Amazon's networks, as on a first run.
    fn hand_back(&self) -> anyhow::Result<()> {
        self.stop();
        self.set_owner(Owner::Amazon);
        run(Command::new("/sbin/start").arg("wifid"))
    }
}

/// wifid and the supplicant it drives; the driver stays, as kindil's own
/// start loads it anyway and reloading costs seconds.
fn stop_amazon() {
    for job in ["wifid", "wifis"] {
        if running(job) {
            let _ = run(Command::new("/sbin/stop").arg(job));
        }
    }
    // Amazon's DHCP client, if wifid left one.
    let _ = Command::new("/usr/bin/killall").arg("udhcpc").status();
}

/// Starting or running, by upstart's goal.
fn running(job: &str) -> bool {
    Command::new("/sbin/status")
        .arg(job)
        .output()
        .is_ok_and(|out| String::from_utf8_lossy(&out.stdout).contains("start/"))
}

fn mac() -> Option<String> {
    let address = std::fs::read_to_string(format!("/sys/class/net/{INTERFACE}/address")).ok()?;
    Some(address.trim().to_owned())
}

/// The ar6003 driver as Amazon's wifim job loads it on a PW1, waited on
/// until its firmware is up and the interface has its MAC.
fn load_driver() -> anyhow::Result<()> {
    if mac().is_some_and(|mac| mac != NO_MAC) {
        return Ok(());
    }
    let board = board_file()?;
    run(Command::new("/sbin/modprobe").args([
        "ar6003",
        &format!("board_data_file={board}"),
        "bmienable=1",
        "wlaninitmode=3",
        // yoshime3 boards have two antennas.
        "enable_diversity=1",
    ]))?;
    let since = Instant::now();
    while !mac().is_some_and(|mac| mac != NO_MAC) {
        anyhow::ensure!(
            since.elapsed() < STARTUP_WAIT,
            "the Wi-Fi driver didn't come up"
        );
        std::thread::sleep(Duration::from_millis(250));
    }
    Ok(())
}

/// `<BOARD_DATA>/<platform>/<board>/wfo/...`: the one the PW1 ships.
fn board_file() -> anyhow::Result<String> {
    let platforms = std::fs::read_dir(BOARD_DATA)?.flatten();
    platforms
        .flat_map(|platform| {
            std::fs::read_dir(platform.path())
                .into_iter()
                .flatten()
                .flatten()
        })
        .map(|board| board.path().join(BOARD_FILE))
        .find(|path| path.is_file())
        .map(|path| path.display().to_string())
        .ok_or_else(|| anyhow::anyhow!("no Wi-Fi calibration data under {BOARD_DATA}"))
}

fn pid(file: &str) -> Option<i32> {
    std::fs::read_to_string(file).ok()?.trim().parse().ok()
}

fn alive(pid: i32) -> bool {
    // SAFETY: signal 0 only checks that the process exists.
    unsafe { libc::kill(pid, 0) == 0 }
}

fn terminate(file: &str) {
    if let Some(pid) = pid(file).filter(|&pid| alive(pid)) {
        // SAFETY: plain kill(2) on a process kindil started.
        unsafe { libc::kill(pid, libc::SIGTERM) };
        let since = Instant::now();
        while alive(pid) && since.elapsed() < Duration::from_secs(3) {
            std::thread::sleep(Duration::from_millis(50));
        }
    }
    let _ = std::fs::remove_file(file);
}

/// wpa_supplicant in the foreground, as kindil's child; returns once it
/// answers on its control socket.
fn spawn_supplicant() -> anyhow::Result<Child> {
    let mut child = Command::new(WPA)
        .args(["-i", INTERFACE, "-D", "wext", "-c", WPA_CONF])
        .stdout(std::process::Stdio::null())
        .spawn()?;
    // wpa_supplicant writes a pid file only when it daemonizes; kindil's
    // own stop and bin/session.sh find it by this one.
    std::fs::write(WPA_PID, child.id().to_string())?;
    let since = Instant::now();
    while wifi::open(INTERFACE).is_err() {
        if let Some(status) = child.try_wait()? {
            anyhow::bail!("wpa_supplicant exited at start ({status})");
        }
        if since.elapsed() >= STARTUP_WAIT {
            let _ = child.kill();
            let _ = child.wait();
            anyhow::bail!("wpa_supplicant didn't start");
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Ok(child)
}

/// udhcpc in the foreground, as kindil's child: it keeps trying until it
/// gets a lease, then keeps the lease renewed.
fn spawn_dhcp() -> anyhow::Result<Child> {
    Ok(Command::new("/sbin/udhcpc")
        .args([
            "-f",
            "-i",
            INTERFACE,
            "-s",
            DHCP_SCRIPT,
            "-p",
            DHCP_PID,
            "-t",
            "5",
            "-T",
            "2",
        ])
        .stdout(std::process::Stdio::null())
        .spawn()?)
}

/// Asks udhcpc to renew the lease now; it was kept through a nap.
fn renew_dhcp() -> anyhow::Result<()> {
    let pid = pid(DHCP_PID)
        .filter(|&pid| alive(pid))
        .ok_or_else(|| anyhow::anyhow!("udhcpc isn't running"))?;
    // SAFETY: plain kill(2) on udhcpc's own pid.
    unsafe { libc::kill(pid, libc::SIGUSR1) };
    Ok(())
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn wifid_power() -> anyhow::Result<Power> {
    match lipc::get("com.lab126.wifid", "enable")?.as_str() {
        "1" => Ok(Power::On),
        "0" => Ok(Power::Off),
        other => anyhow::bail!("unexpected wifid enable value {other:?}"),
    }
}

fn wifid_value(power: Power) -> &'static str {
    match power {
        Power::On => "1",
        Power::Off => "0",
    }
}

/// The id of the network named `ssid` in a `LIST_NETWORKS` reply.
pub fn network_id(reply: &str, ssid: &str) -> Option<u32> {
    reply.lines().skip(1).find_map(|line| {
        let mut columns = line.split('\t');
        let id = columns.next()?.trim().parse().ok()?;
        // The PW1's wpa_supplicant shows names in brackets.
        let name = columns.next()?;
        let name = name
            .strip_prefix('[')
            .and_then(|n| n.strip_suffix(']'))
            .unwrap_or(name);
        (name == ssid).then_some(id)
    })
}

fn set_executable(path: &str) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
}

fn run(command: &mut Command) -> anyhow::Result<()> {
    let status = command.status()?;
    anyhow::ensure!(status.success(), "{command:?} exited with {status}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_a_network_by_name() {
        let reply =
            "network id / ssid / bssid / flags\n0\tHome\tany\t[CURRENT]\n1\tCafe Bar\tany\t\n";
        assert_eq!(network_id(reply, "Home"), Some(0));
        assert_eq!(network_id(reply, "Cafe Bar"), Some(1));
        assert_eq!(network_id(reply, "Cafe"), None);
        assert_eq!(
            network_id("network id / ssid / bssid / flags\n", "Home"),
            None
        );
    }

    #[test]
    fn starts_out_with_amazon_until_kindil_has_a_network() {
        let net = Arc::new(Net::default());
        assert_eq!(net.owner(), Owner::Amazon);
        net.load(&[]).unwrap();
        assert_eq!(
            net.owner(),
            Owner::Amazon,
            "no networks: nothing to take over"
        );
    }
}
