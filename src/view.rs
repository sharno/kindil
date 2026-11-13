//! Domain state → what the screen shows. Pure, so wording and units are tested
//! without a window.
use crate::battery::{Battery, Charge};
use crate::host::{BootStart, DeviceInfo, Power, Ssh, Wifi};
use crate::prefs::Progress;
use crate::reader::Book;
use crate::sync::Service;
use crate::ui::{BookItem, Fact, NetworkItem, Switch};
use crate::update::{Updates, Version};
use crate::wifi::{Network, Security};
use chrono::{DateTime, TimeZone};
use std::net::Ipv4Addr;
use std::time::{Duration, Instant};

pub fn clock<Tz: TimeZone>(now: &DateTime<Tz>) -> (String, String)
where
    Tz::Offset: std::fmt::Display,
{
    (
        now.format("%H:%M").to_string(),
        now.format("%A %-d %B").to_string(),
    )
}

/// Status bar battery: level for the icon (-1 unknown) and its label.
pub fn battery(battery: Battery) -> (i32, String) {
    match battery {
        Battery::Unknown => (-1, "—".into()),
        Battery::Known {
            percent,
            charge: Charge::Charging,
        } => (i32::from(percent), format!("{percent}% +")),
        Battery::Known {
            percent,
            charge: Charge::Discharging,
        } => (i32::from(percent), format!("{percent}%")),
    }
}

pub fn switch(power: Power) -> Switch {
    match power {
        Power::On => Switch::On,
        Power::Off => Switch::Off,
    }
}

pub fn ip(wifi: &Wifi) -> Option<Ipv4Addr> {
    match wifi {
        Wifi::Connected { ip, .. } => *ip,
        Wifi::Off | Wifi::Searching | Wifi::Unavailable(_) => None,
    }
}

/// Status bar bars: -1 hides the icon (radio off or unknown).
pub fn wifi_bars(wifi: &Wifi) -> i32 {
    match wifi {
        Wifi::Connected { signal, .. } => signal.map_or(4, |s| i32::from(s.bars())),
        Wifi::Searching => 0,
        Wifi::Off | Wifi::Unavailable(_) => -1,
    }
}

pub fn wifi(wifi: &Wifi) -> (Switch, String) {
    match wifi {
        Wifi::Off => (Switch::Off, "Off".into()),
        Wifi::Searching => (Switch::On, "Not connected".into()),
        Wifi::Connected { ssid, ip, .. } => (
            Switch::On,
            match ip {
                Some(ip) => format!("{ssid} · {ip}"),
                None => format!("{ssid} · waiting for an address"),
            },
        ),
        Wifi::Unavailable(reason) => (Switch::Unavailable, reason.clone()),
    }
}

/// (server, at boot, detail)
pub fn ssh(ssh: Ssh, ip: Option<Ipv4Addr>) -> (Switch, Switch, String) {
    match ssh {
        Ssh::NotInstalled => (
            Switch::Unavailable,
            Switch::Unavailable,
            "Install USBNetwork to enable SSH".into(),
        ),
        Ssh::Installed { server, at_boot } => (
            switch(server),
            switch(at_boot),
            match (server, ip) {
                (Power::On, Some(ip)) => format!("ssh root@{ip} · keys only"),
                (Power::On, None) => "Listening on port 22 · keys only".into(),
                (Power::Off, _) => "Off".into(),
            },
        ),
    }
}

pub fn boot_start(boot: BootStart) -> (Switch, String) {
    match boot {
        BootStart::NotInstalled => (
            Switch::Unavailable,
            "Needs a one-time install over SSH; see the README".into(),
        ),
        BootStart::Installed(power) => (
            switch(power),
            match power {
                Power::On => "Instead of Amazon's UI, which stays one Exit away".into(),
                Power::Off => "Off: the Kindle starts with Amazon's UI".into(),
            },
        ),
    }
}

pub fn sync(sync: &Service, now: Instant) -> (Switch, String) {
    match sync {
        Service::NotInstalled => (
            Switch::Unavailable,
            "Add [syncthing] to kindil.toml to enable".into(),
        ),
        Service::Stopped(_) => (Switch::Off, "Off".into()),
        Service::Running(running) => (
            Switch::On,
            format!(
                "Running {} · low priority",
                duration(now.saturating_duration_since(running.since))
            ),
        ),
        Service::Failed(failed) => (Switch::Off, format!("Stopped: {}", failed.reason)),
    }
}

pub fn bytes(bytes: u64) -> String {
    const UNITS: [(&str, u64); 3] = [("GB", 1 << 30), ("MB", 1 << 20), ("KB", 1 << 10)];
    UNITS
        .iter()
        .find(|(_, size)| bytes >= *size)
        .map(|(unit, size)| match bytes / size {
            0..10 => format!("{:.1} {unit}", bytes as f64 / *size as f64),
            whole => format!("{whole} {unit}"),
        })
        .unwrap_or_else(|| format!("{bytes} B"))
}

pub fn duration(duration: Duration) -> String {
    let minutes = duration.as_secs() / 60;
    match (minutes / (24 * 60), minutes / 60 % 24, minutes % 60) {
        (0, 0, m) => format!("{m} min"),
        (0, h, m) => format!("{h} h {m} min"),
        (d, h, _) => format!("{d} d {h} h"),
    }
}

/// The About page's doze row: the draw and how long a charge lasts at it.
pub fn doze_draw(drain: crate::drain::Drain) -> String {
    match drain.hours_left() {
        Some(hours) if hours >= 48 => format!("{} mA · {} days left", drain.milliamps, hours / 24),
        Some(hours) => format!("{} mA · {hours} h left", drain.milliamps),
        None => format!("{} mA", drain.milliamps),
    }
}

pub fn facts(info: &DeviceInfo, version: &str) -> Vec<Fact> {
    let fact = |label: &str, value: String| Fact {
        label: label.into(),
        value: value.into(),
    };
    let unknown = || "—".to_string();
    vec![
        fact(
            "Battery",
            match info.battery {
                Battery::Unknown => unknown(),
                Battery::Known {
                    percent,
                    charge: Charge::Charging,
                } => format!("{percent}% · charging"),
                Battery::Known { percent, .. } => format!("{percent}%"),
            },
        ),
        fact(
            "Memory free",
            info.memory.map_or_else(unknown, |m| {
                format!(
                    "{} of {}",
                    bytes(m.available_kib * 1024),
                    bytes(m.total_kib * 1024)
                )
            }),
        ),
        fact(
            "Storage free",
            info.storage.map_or_else(unknown, |s| {
                format!("{} of {}", bytes(s.free_bytes), bytes(s.total_bytes))
            }),
        ),
        fact("Up for", info.uptime.map_or_else(unknown, duration)),
        fact("kindil", version.into()),
    ]
}

pub fn book(book: &Book, progress: Option<Progress>) -> BookItem {
    let progress = progress.map_or(0.0, f32::from);
    let read = match (progress * 100.0).round() as u32 {
        0 => String::new(),
        percent => format!(" · {percent}% read"),
    };
    BookItem {
        title: book.title().into(),
        format: match book.format {
            crate::reader::Format::Epub => "EPUB",
            crate::reader::Format::Text => "TXT",
        }
        .into(),
        detail: format!("{} · {}{read}", book.format.label(), bytes(book.bytes)).into(),
        progress,
        file: book.file_name().into(),
    }
}

pub fn network(network: &Network) -> NetworkItem {
    NetworkItem {
        ssid: network.ssid.as_str().into(),
        detail: match network.security {
            Security::Open => "Open",
            Security::Protected => "Secured",
        }
        .into(),
        bars: i32::from(network.signal.bars()),
    }
}

/// The Updates row: status line, button label, whether the button works.
pub fn updates(updates: &Updates, current: Version) -> (String, String, bool) {
    match updates {
        Updates::Idle => (format!("Version {current}"), "Check".into(), true),
        Updates::Checking => ("Checking GitHub…".into(), "Check".into(), false),
        Updates::UpToDate => (format!("{current} is the latest"), "Check".into(), true),
        Updates::Available(release) => (
            format!("{} is available; you have {current}", release.version),
            "Install".into(),
            true,
        ),
        Updates::Installing(version) => (
            format!("Installing {version}; kindil restarts when it's ready"),
            "Install".into(),
            false,
        ),
        Updates::Failed(error) => (error.clone(), "Check".into(), true),
    }
}

pub fn position(page: usize, pages: usize) -> String {
    format!("{} of {}", page + 1, pages)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::{Memory, Signal};

    #[test]
    fn units() {
        assert_eq!(bytes(512), "512 B");
        assert_eq!(bytes(1_420_000_000), "1.3 GB");
        assert_eq!(bytes(250 * 1024 * 1024), "250 MB");
        assert_eq!(duration(Duration::from_secs(59)), "0 min");
        assert_eq!(
            duration(Duration::from_secs(3 * 3600 + 25 * 60)),
            "3 h 25 min"
        );
        assert_eq!(duration(Duration::from_secs(50 * 3600)), "2 d 2 h");
    }

    #[test]
    fn wifi_and_ssh_wording() {
        let connected = Wifi::Connected {
            ssid: "Home".into(),
            ip: Some(Ipv4Addr::new(192, 168, 1, 120)),
            signal: Some(Signal(-70)),
        };
        assert_eq!(wifi_bars(&connected), 2);
        assert_eq!(wifi(&connected).1, "Home · 192.168.1.120");
        assert_eq!(wifi_bars(&Wifi::Off), -1);
        let (server, boot, detail) = ssh(
            Ssh::Installed {
                server: Power::On,
                at_boot: Power::Off,
            },
            ip(&connected),
        );
        assert_eq!((server, boot), (Switch::On, Switch::Off));
        assert_eq!(detail, "ssh root@192.168.1.120 · keys only");
        assert_eq!(ssh(Ssh::NotInstalled, None).0, Switch::Unavailable);
    }

    #[test]
    fn device_facts() {
        let info = DeviceInfo {
            battery: Battery::Known {
                percent: 82,
                charge: Charge::Charging,
            },
            charge: None,
            memory: Some(Memory {
                total_kib: 255_000,
                available_kib: 171_000,
            }),
            storage: None,
            uptime: Some(Duration::from_secs(600)),
        };
        let facts = facts(&info, "0.4.0");
        let values: Vec<String> = facts.iter().map(|f| f.value.to_string()).collect();
        assert_eq!(
            values,
            ["82% · charging", "166 MB of 249 MB", "—", "10 min", "0.4.0"]
        );
    }
}
