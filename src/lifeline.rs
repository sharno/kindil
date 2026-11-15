//! `kindil lifeline DEADLINE PID`: brings the Kindle back online after an
//! experiment (`cargo xtask run` starts both). When the experiment's process
//! exits, or at the deadline (wall clock, suspend included), it kills the
//! experiment if still running, clears any wake alarm and resumes a paused
//! kindil. If the Kindle is then offline: with kindil running, kindil is
//! restarted (its Wi-Fi afresh), and if still offline stopped, which hands
//! Wi-Fi to Amazon's wifid; with no kindil, wifid is started afresh. It
//! never starts wifid behind a running kindil's back.
//!
//! It only acts while the Kindle is awake: no help for an experiment that
//! keeps it asleep or dozing.
//!
//! It is kindil's own binary, run as `kindil-lifeline` (a link beside it,
//! which `cargo xtask run` makes), so it never takes itself for kindil: it
//! leaves out its own process and ends kindil by pid, never `killall`. An
//! experiment's `killall kindil` still finds it (busybox also matches the
//! binary a process runs, links resolved), so it ignores the signals that
//! end a process politely; only KILL takes it down.
use std::net::{TcpStream, ToSocketAddrs};
use std::path::Path;
use std::process::Command;
use std::time::{Duration, SystemTime};

/// The name kindil answers to as the lifeline: `kindil-lifeline DEADLINE PID`.
pub const NAME: &str = "kindil-lifeline";

const START: &str = "/mnt/us/extensions/kindil/bin/start.sh";
/// kindil stopping fully: its session restores Amazon's UI on the way out.
const SETTLE: Duration = Duration::from_secs(25);

fn log(message: &str) {
    eprintln!(
        "{} lifeline: {message}",
        chrono::Local::now().format("%F %T")
    );
}

fn alive(pid: i32) -> bool {
    // SAFETY: signal 0 only checks that the process exists.
    unsafe { libc::kill(pid, 0) == 0 }
}

fn signal(pid: i32, signal: i32) {
    // SAFETY: plain kill(2).
    unsafe { libc::kill(pid, signal) };
}

/// kindil's processes, by name: never this one, which is kindil's binary too.
fn kindil_pids() -> Vec<i32> {
    app_pids(
        Path::new("/proc"),
        i32::try_from(std::process::id()).unwrap_or(0),
    )
}

/// The processes under `proc` named kindil (not its hook, nor a lifeline),
/// but `own`.
fn app_pids(proc: &Path, own: i32) -> Vec<i32> {
    let mut pids: Vec<i32> = std::fs::read_dir(proc)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| {
            let pid: i32 = entry.file_name().to_str()?.parse().ok()?;
            // stat, not comm: Linux 2.6.31 has no /proc/PID/comm.
            let stat = std::fs::read_to_string(entry.path().join("stat")).ok()?;
            (name(&stat)? == "kindil" && pid != own).then_some(pid)
        })
        .collect();
    pids.sort_unstable();
    pids
}

/// The process name in a /proc/PID/stat line: "pid (name) state ...", where
/// the name may itself hold spaces or parentheses.
fn name(stat: &str) -> Option<&str> {
    let start = stat.find('(')? + 1;
    let end = stat.rfind(')')?;
    stat.get(start..end)
}

/// Asks every kindil process to end, as `killall kindil` would, but by pid.
fn stop_kindil() {
    for pid in kindil_pids() {
        signal(pid, libc::SIGTERM);
    }
}

/// A connection to the outside world within `tries` attempts, 3 s apart.
fn online(tries: u32) -> bool {
    (0..tries).any(|attempt| {
        if attempt > 0 {
            std::thread::sleep(Duration::from_secs(3));
        }
        ("example.com", 80)
            .to_socket_addrs()
            .ok()
            .and_then(|mut addrs| addrs.next())
            .is_some_and(|addr| TcpStream::connect_timeout(&addr, Duration::from_secs(3)).is_ok())
    })
}

/// The signals the lifeline ignores, which an exec'd child would inherit
/// ignored: its children get them back.
const POLITE: [i32; 3] = [libc::SIGTERM, libc::SIGHUP, libc::SIGINT];

fn run(program: &str, args: &[&str]) {
    use std::os::unix::process::CommandExt;
    let mut command = Command::new(program);
    command
        .args(args)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    // SAFETY: signal(2) is async-signal-safe.
    unsafe {
        command.pre_exec(|| {
            for sig in POLITE {
                libc::signal(sig, libc::SIG_DFL);
            }
            Ok(())
        });
    }
    let _ = command.status();
}

/// `kindil-lifeline DEADLINE PID`'s arguments.
pub fn parse(args: &[String]) -> Option<(Duration, i32)> {
    match args {
        [deadline, pid] => Some((
            Duration::from_secs(deadline.parse().ok()?),
            pid.parse().ok().filter(|&pid: &i32| pid > 0)?,
        )),
        _ => None,
    }
}

pub fn lifeline(deadline: Duration, test: i32) {
    for sig in POLITE {
        // SAFETY: signal(2) with SIG_IGN.
        unsafe { libc::signal(sig, libc::SIG_IGN) };
    }
    log(&format!(
        "test {test}, deadline in {} s",
        deadline.as_secs()
    ));
    let until = SystemTime::now() + deadline;
    while SystemTime::now() < until && alive(test) {
        std::thread::sleep(Duration::from_secs(10));
    }
    if alive(test) {
        log(&format!("deadline: killing test {test}"));
        signal(test, libc::SIGTERM);
        std::thread::sleep(Duration::from_secs(2));
        signal(test, libc::SIGKILL);
    }
    for n in 0..4 {
        let _ = std::fs::write(format!("/sys/class/rtc/rtc{n}/wakealarm"), "0");
    }
    kindil_pids()
        .into_iter()
        .for_each(|pid| signal(pid, libc::SIGCONT));
    if online(20) {
        return log("online, nothing to do");
    }
    if !kindil_pids().is_empty() {
        log("offline: restarting kindil");
        stop_kindil();
        std::thread::sleep(SETTLE);
        run(START, &[]);
        if online(40) {
            return log("online after restarting kindil");
        }
        log("STILL OFFLINE: stopping kindil; Amazon's UI and wifid take over");
        stop_kindil();
        std::thread::sleep(SETTLE);
    } else {
        log("offline, no kindil: starting wifid afresh");
        run("killall", &["wpa_supplicant", "udhcpc"]);
        for (verb, job) in [("stop", "wifid"), ("stop", "wifis"), ("start", "wifid")] {
            run(&format!("/sbin/{verb}"), &[job]);
        }
    }
    if online(40) {
        log("online with Amazon's wifid");
    } else {
        log("offline even with wifid");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn process(proc: &Path, pid: i32, comm: &str) {
        let dir = proc.join(pid.to_string());
        std::fs::create_dir_all(&dir).unwrap();
        // As 2.6.31 has it: stat only, no comm file.
        std::fs::write(
            dir.join("stat"),
            format!("{pid} ({comm}) S 1 {pid} {pid} 0\n"),
        )
        .unwrap();
    }

    #[test]
    fn kindil_is_its_app_processes_never_the_lifeline_itself() {
        let dir = tempfile::tempdir().unwrap();
        let proc = dir.path();
        process(proc, 10, "kindil");
        process(proc, 11, "kindil");
        process(proc, 12, "kindil-dhcp");
        process(proc, 13, NAME);
        process(proc, 14, "sh");
        std::fs::create_dir_all(proc.join("self")).unwrap();
        // Run as `kindil lifeline` (before the link), its comm is kindil.
        assert_eq!(app_pids(proc, 11), [10]);
        assert_eq!(app_pids(proc, 13), [10, 11]);
    }

    #[test]
    fn reads_the_name_from_stat_even_with_spaces_or_parentheses() {
        assert_eq!(name("1640 (kindil) S 1563 1563"), Some("kindil"));
        assert_eq!(name("7 (a (b) c) R 1 7"), Some("a (b) c"));
        assert_eq!(name("garbage"), None);
    }

    #[test]
    fn reads_its_arguments() {
        let args = |a: &[&str]| a.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
        assert_eq!(
            parse(&args(&["600", "42"])),
            Some((Duration::from_secs(600), 42))
        );
        assert_eq!(parse(&args(&["600"])), None);
        assert_eq!(parse(&args(&["x", "42"])), None);
        assert_eq!(parse(&args(&["600", "0"])), None);
    }
}
