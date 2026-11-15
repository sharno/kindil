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
use std::net::{TcpStream, ToSocketAddrs};
use std::process::Command;
use std::time::{Duration, SystemTime};

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

/// kindil's processes, by name.
fn kindil_pids() -> Vec<i32> {
    std::fs::read_dir("/proc")
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| {
            let pid: i32 = entry.file_name().to_str()?.parse().ok()?;
            let comm = std::fs::read_to_string(entry.path().join("comm")).ok()?;
            (comm.trim() == "kindil").then_some(pid)
        })
        .collect()
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

fn run(program: &str, args: &[&str]) {
    let _ = Command::new(program)
        .args(args)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
}

pub fn lifeline(deadline: Duration, test: i32) {
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
        run("killall", &["kindil"]);
        std::thread::sleep(SETTLE);
        run(START, &[]);
        if online(40) {
            return log("online after restarting kindil");
        }
        log("STILL OFFLINE: stopping kindil; Amazon's UI and wifid take over");
        run("killall", &["kindil"]);
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
