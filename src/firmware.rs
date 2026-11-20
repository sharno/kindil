//! The kernel's firmware loader's timeout, 0 while kindil runs, so udev
//! serves the Wi-Fi driver's firmware requests with the workers it has.
//!
//! Each resume, the ar6003 driver asks for its four firmware files again
//! (src/health.rs). The kernel's request carries `TIMEOUT=60` (the sysfs
//! `timeout`), and the PW1's udev 146 starts a new worker for every event
//! with a timeout (udevd.c, `event_queue_insert`: "run all events with a
//! timeout set immediately"), without looking for an idle one or at its
//! limit of workers. It ends idle workers only after 3 s awake with no
//! event queued, which a doze wake, back asleep a second after its panel
//! update, never gives it: on the device, about 8 more workers a wake, 240
//! by each 30-minute fetch (whose few seconds online finally let them go),
//! about 35 MiB at their peak. On 2026-11-20, 228 of them were alive when
//! the kernel ran out of memory.
//!
//! With the timeout 0 the request has none: udev queues it and gives it
//! to an idle worker like any other event, and the kernel waits for the
//! file as long as it takes (udev's firmware.sh always answers, with the
//! file or with -1). kindil writes 0 at its start and the kernel's
//! default back as it exits; the launcher writes it back too on the way
//! to Amazon's UI, after a crash. The setting lives in the running kernel
//! only: a boot starts at 60 again.
use std::path::Path;

/// The firmware loader's timeout, in seconds.
pub const TIMEOUT: &str = "/sys/class/firmware/timeout";
/// The kernel's own (drivers/base/firmware_class.c `loading_timeout`).
pub const KERNEL_DEFAULT: u32 = 60;

/// No timeout: udev queues firmware requests as other events.
pub fn untimed(path: &Path) -> std::io::Result<()> {
    std::fs::write(path, "0")
}

/// The kernel's default back, for Amazon's UI.
pub fn restore(path: &Path) -> std::io::Result<()> {
    std::fs::write(path, KERNEL_DEFAULT.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_while_kindil_runs_and_the_kernels_default_after() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("timeout");
        untimed(&path).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "0");
        restore(&path).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "60");
    }
}
