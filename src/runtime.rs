//! Minimal standalone bootstrap. Never invoked in previews or on the stock OS.
//! Kernel drivers/firmware remain supplied by the matched PW1 Linux image.
use std::{ffi::CString, io, path::Path, process::Command};

pub fn prepare_pid1() -> io::Result<()> {
    if std::process::id() != 1 {
        return Err(io::Error::other("standalone bootstrap requires PID 1"));
    }
    for dir in [
        "/dev",
        "/proc",
        "/sys",
        "/run",
        "/tmp",
        "/dev/input",
        "/var/log/kindil",
        "/etc/dropbear",
        "/root/.ssh",
    ] {
        std::fs::create_dir_all(dir)?;
    }
    mount("proc", "/proc", "proc")?;
    mount("sysfs", "/sys", "sysfs")?;
    mount("tmpfs", "/run", "tmpfs")?;
    mount("tmpfs", "/tmp", "tmpfs")?;
    // Linux 2.6.31 may lack devtmpfs. Static nodes remain a supported fallback.
    let _ = mount("devtmpfs", "/dev", "devtmpfs");
    std::fs::create_dir_all("/dev/input")?;
    std::fs::create_dir_all("/dev/pts")?;
    mount("devpts", "/dev/pts", "devpts")?;
    for (path, major, minor) in [
        ("/dev/console", 5, 1),
        ("/dev/tty", 5, 0),
        ("/dev/ptmx", 5, 2),
        ("/dev/null", 1, 3),
        ("/dev/random", 1, 8),
        ("/dev/urandom", 1, 9),
        ("/dev/fb0", 29, 0),
        ("/dev/yoshime_fl", 10, 162),
    ] {
        node(path, major, minor)?;
    }
    for i in 0..16 {
        node(&format!("/dev/input/event{i}"), 13, 64 + i)?;
    }
    Ok(())
}
fn mount(source: &str, target: &str, fs: &str) -> io::Result<()> {
    let source = CString::new(source)?;
    let target = CString::new(target)?;
    let fs = CString::new(fs)?;
    let rc = unsafe {
        libc::mount(
            source.as_ptr(),
            target.as_ptr(),
            fs.as_ptr(),
            libc::MS_NOSUID,
            std::ptr::null(),
        )
    };
    if rc != 0 {
        let e = io::Error::last_os_error();
        if e.raw_os_error() != Some(libc::EBUSY) {
            return Err(e);
        }
    }
    Ok(())
}
fn node(path: &str, major: u32, minor: u32) -> io::Result<()> {
    let path = CString::new(path)?;
    let rc = unsafe {
        libc::mknod(
            path.as_ptr(),
            libc::S_IFCHR | 0o600,
            libc::makedev(major, minor),
        )
    };
    if rc != 0 {
        let e = io::Error::last_os_error();
        if e.raw_os_error() != Some(libc::EEXIST) {
            return Err(e);
        }
    }
    Ok(())
}

/// udhcpc callback implemented in Rust. Uses BusyBox's proven netlink/ip client,
/// never a shell, and validates all DHCP-controlled values before execution.
pub fn dhcp_hook(event: &str) -> io::Result<()> {
    use std::net::Ipv4Addr;
    let interface = std::env::var("interface").map_err(io::Error::other)?;
    if interface.is_empty()
        || interface.len() > 15
        || !interface
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"_-.:".contains(&c))
    {
        return Err(io::Error::other("invalid interface"));
    }
    let run = |args: &[&str]| -> io::Result<()> {
        if !Command::new("/bin/busybox")
            .arg("ip")
            .args(args)
            .status()?
            .success()
        {
            return Err(io::Error::other("BusyBox ip failed"));
        }
        Ok(())
    };
    match event {
        "deconfig" => run(&["addr", "flush", "dev", &interface])?,
        "bound" | "renew" => {
            let ip = std::env::var("ip")
                .map_err(io::Error::other)?
                .parse::<Ipv4Addr>()
                .map_err(io::Error::other)?;
            let mask = std::env::var("subnet")
                .unwrap_or_else(|_| "255.255.255.0".into())
                .parse::<Ipv4Addr>()
                .map_err(io::Error::other)?;
            let bits = u32::from(mask);
            let prefix = bits.count_ones();
            if bits != u32::MAX.checked_shl(32 - prefix).unwrap_or(0) {
                return Err(io::Error::other("non-contiguous netmask"));
            }
            run(&[
                "addr",
                "replace",
                &format!("{ip}/{prefix}"),
                "dev",
                &interface,
            ])?;
            run(&["link", "set", "dev", &interface, "up"])?;
            if let Some(router) = std::env::var("router")
                .ok()
                .and_then(|s| s.split_whitespace().next().map(str::to_owned))
            {
                let router = router.parse::<Ipv4Addr>().map_err(io::Error::other)?;
                run(&[
                    "route",
                    "replace",
                    "default",
                    "via",
                    &router.to_string(),
                    "dev",
                    &interface,
                ])?;
            }
            let dns = std::env::var("dns")
                .unwrap_or_default()
                .split_whitespace()
                .map(|s| s.parse::<Ipv4Addr>())
                .collect::<Result<Vec<_>, _>>()
                .map_err(io::Error::other)?;
            std::fs::write(
                Path::new("/etc/resolv.conf"),
                dns.iter()
                    .map(|ip| format!("nameserver {ip}\n"))
                    .collect::<String>(),
            )?;
        }
        _ => {}
    }
    Ok(())
}
