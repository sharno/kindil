//! Whether the kernel can take another suspend, and what kindil does when
//! it can't: reboot rather than suspend.
//!
//! **An oops.** An oops kills the task it hit and leaves the kernel
//! running, often with a driver half-way through something and its locks
//! held. On 2026-11-20 the ar6003 Wi-Fi driver oopsed in its "AR6K enable"
//! thread (a NULL dereference in its firmware download, out of memory);
//! kindil retried Wi-Fi for three minutes, then napped, and the suspend
//! never returned: the device froze until a long press of the power
//! button. A suspend asks every driver to suspend its device, the broken
//! one too, so after an oops none is safe. The kernel says so itself:
//! ARM's `die()` (arch/arm/kernel/traps.c) sets `TAINT_DIE` in
//! `/proc/sys/kernel/tainted`, and nothing clears it before the next boot.
//!
//! **Memory the kernel lost.** What ran it out of memory: each resume
//! powers the Wi-Fi chip up and loads its four firmware files again
//! (Lab126's ar6003 cuts its power at every suspend), and the PW1's 2.6.31
//! firmware loader never frees a loaded image's pages (it `vfree`s a
//! `vmap`ed area, which frees none; fixed upstream in 2.6.35 by
//! dd336c554d89): 17 pages (68 KiB) a resume, about 4 MiB an hour of
//! doze, measured on the device. Nothing short of a reboot gives them
//! back, and the driver can't be kept from reloading: its own WLAN-off
//! state (ioctl 35), tried on the device, keeps the chip off through a
//! suspend, but the SDIO core still cycles its power, finds the card gone,
//! and the driver refuses it as a second one (Wi-Fi then needs a reboot);
//! and unloading the driver can hang (src/net.rs). So
//! kindil counts the pages the kernel holds outside its free lists, page
//! cache, anonymous memory, slab and page tables (from `/proc/zoneinfo`):
//! about 6,800 after a boot, and 47,600 at the out-of-memory kill of
//! 2026-11-20. Past [`LEAK_LIMIT`] it reboots before suspending again, at
//! about a day of doze.
//!
//! Instead of hanging (or being killed by the out-of-memory killer, and
//! then hanging), kindil reboots: a dashboard that comes back by itself
//! (with start at boot, as kindil; without it, as Amazon's UI) is worth
//! more than one that stays up, awake and draining its battery, with
//! Wi-Fi gone. Powering off would keep it dark until someone came by. A
//! cause that comes back soon after each boot must not make the Kindle
//! reboot all day: at most one such reboot in [`REBOOT_SPACING`]; in
//! between it stays awake, refusing every suspend, and says why in the
//! log.
use std::path::Path;
use std::time::Duration;

/// The kernel's taint flags, a decimal bit mask.
pub const TAINTED: &str = "/proc/sys/kernel/tainted";
/// `TAINT_DIE` (include/linux/kernel.h): an oops or BUG since boot.
const DIE: u64 = 1 << 7;
/// Each memory zone's page counts.
pub const ZONEINFO: &str = "/proc/zoneinfo";
/// Past this many pages held by the kernel outside its lists (see the
/// module's head), kindil reboots instead of suspending: 144 MiB of the
/// PW1's 254, leaving about 40 MiB more than the 2026-11-20 kill had for
/// userspace; reached after about a day of doze.
pub const LEAK_LIMIT: Pages = Pages(36_864);

/// When kindil last rebooted for its kernel's health, in epoch seconds.
pub const LAST_REBOOT: &str = "/var/local/kindil/health-reboot";
/// No second reboot for the kernel's health sooner than this after the last.
pub const REBOOT_SPACING: Duration = Duration::from_secs(6 * 60 * 60);
/// A reboot through init that hasn't happened this long after it was
/// asked for is forced: init's shutdown may wait on a broken driver.
pub const REBOOT_GRACE: Duration = Duration::from_secs(60);

/// A count of 4 KiB pages.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Pages(pub u64);

impl std::fmt::Display for Pages {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} pages ({} MiB)", self.0, self.0 * 4 / 1024)
    }
}

/// Whether the kernel can take another suspend.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Health {
    Sound,
    Unfit(Unfit),
}

/// Why the kernel can't take another suspend: a suspend refused for it
/// (src/suspend.rs) carries this as its error.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unfit {
    /// It has oopsed since boot: no suspend is safe.
    Oopsed,
    /// It holds this much memory outside its lists, past [`LEAK_LIMIT`].
    Leaked(Pages),
}

/// `/proc/sys/kernel/tainted`'s text: whether `TAINT_DIE` is set.
/// Unreadable flags count as no oops, as before kindil looked.
pub fn oopsed(tainted: Option<&str>) -> bool {
    tainted
        .and_then(|text| text.trim().parse::<u64>().ok())
        .is_some_and(|flags| flags & DIE != 0)
}

/// The pages `/proc/zoneinfo` shows present but on none of the kernel's
/// lists: what drivers and the kernel allocated for themselves, the
/// firmware loader's lost pages among them. None if the text has no zone.
pub fn held(zoneinfo: &str) -> Option<Pages> {
    const LISTED: [&str; 8] = [
        "nr_free_pages",
        "nr_inactive_anon",
        "nr_active_anon",
        "nr_inactive_file",
        "nr_active_file",
        "nr_slab_reclaimable",
        "nr_slab_unreclaimable",
        "nr_page_table_pages",
    ];
    let count = |name: &str| -> u64 {
        zoneinfo
            .lines()
            .filter_map(|line| {
                let mut words = line.split_whitespace();
                (words.next() == Some(name))
                    .then(|| words.next()?.parse::<u64>().ok())
                    .flatten()
            })
            .sum()
    };
    let present = count("present");
    let listed: u64 = LISTED.iter().map(|name| count(name)).sum();
    (present > 0).then(|| Pages(present.saturating_sub(listed)))
}

/// The kernel's health from its taint flags and zone counts.
pub fn health(tainted: Option<&str>, zoneinfo: Option<&str>) -> Health {
    if oopsed(tainted) {
        return Health::Unfit(Unfit::Oopsed);
    }
    match zoneinfo.and_then(held) {
        Some(pages) if pages > LEAK_LIMIT => Health::Unfit(Unfit::Leaked(pages)),
        Some(_) | None => Health::Sound,
    }
}

/// The kernel's health now.
pub fn now() -> Health {
    let read = |path: &str| std::fs::read_to_string(path).ok();
    health(read(TAINTED).as_deref(), read(ZONEINFO).as_deref())
}

impl std::fmt::Display for Unfit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Unfit::Oopsed => {
                f.write_str("the kernel has oopsed since boot (TAINT_DIE); a suspend could hang")
            }
            Unfit::Leaked(pages) => write!(
                f,
                "the kernel holds {pages} outside its lists, past {LEAK_LIMIT}: \
                 out of memory soon"
            ),
        }
    }
}

impl std::error::Error for Unfit {}

/// What to do about an unfit kernel, given when kindil last rebooted for one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Remedy {
    Reboot,
    /// Rebooted for one less than [`REBOOT_SPACING`] ago: stay up, awake.
    StayAwake {
        rebooted_ago: Duration,
    },
}

/// The remedy at `now` (epoch seconds), with the last reboot for the
/// kernel's health at `last`, if any. A last reboot in the future (the
/// clock went back) counts as long ago.
pub fn remedy(last: Option<i64>, now: i64) -> Remedy {
    let ago = last.and_then(|last| u64::try_from(now - last).ok());
    match ago.map(Duration::from_secs) {
        Some(ago) if ago < REBOOT_SPACING => Remedy::StayAwake { rebooted_ago: ago },
        Some(_) | None => Remedy::Reboot,
    }
}

/// The last reboot for the kernel's health, as [`LAST_REBOOT`] records it.
pub fn last_reboot(path: &Path) -> Option<i64> {
    std::fs::read_to_string(path).ok()?.trim().parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_taint_die_is_an_oops() {
        assert!(!oopsed(Some("0\n")));
        // TAINT_WARN, as a WARN_ON leaves it (the firmware loader's
        // duplicate sysfs name on 2026-11-20): not an oops.
        assert!(!oopsed(Some("512\n")));
        assert!(oopsed(Some("128\n")));
        assert!(oopsed(Some("641\n")), "with others");
        assert!(!oopsed(Some("garbage")));
        assert!(!oopsed(None));
    }

    /// The PW1's two zones (the fields kindil reads, of many), 25 minutes
    /// after a boot on 2026-11-20.
    const AFTER_BOOT: &str = "Node 0, zone      DMA
  pages free     8353
        min      390
  nr_free_pages 8353
  nr_inactive_anon 2
  nr_active_anon 76
  nr_inactive_file 8414
  nr_active_file 2881
  nr_mapped    634
  nr_slab_reclaimable 653
  nr_slab_unreclaimable 31
  nr_page_table_pages 3
        spanned  24576
        present  24384
Node 0, zone   Normal
  pages free     7621
  nr_free_pages 7621
  nr_inactive_anon 756
  nr_active_anon 2469
  nr_inactive_file 16063
  nr_active_file 8198
  nr_slab_reclaimable 1628
  nr_slab_unreclaimable 941
  nr_page_table_pages 135
        spanned  40960
        present  40640
";

    #[test]
    fn counts_the_pages_on_no_list() {
        // DMA 3,971 + Normal 2,829.
        assert_eq!(held(AFTER_BOOT), Some(Pages(6_800)));
        assert_eq!(held(""), None);
        assert_eq!(
            health(Some("0"), Some(AFTER_BOOT)),
            Health::Sound,
            "a fresh boot is sound"
        );
    }

    #[test]
    fn past_the_limit_the_kernel_is_unfit() {
        // The out-of-memory kill of 2026-11-20: 65,024 pages present, 17,450
        // on the lists.
        let killed = "Node 0, zone Normal\n  nr_free_pages 1197\n  nr_active_anon 6281\n  \
            nr_inactive_anon 4109\n  nr_active_file 15\n  nr_inactive_file 18\n  \
            nr_slab_reclaimable 783\n  nr_slab_unreclaimable 783\n  \
            nr_page_table_pages 803\n        present 65024\n";
        assert_eq!(held(killed), Some(Pages(51_035)));
        assert_eq!(
            health(Some("0"), Some(killed)),
            Health::Unfit(Unfit::Leaked(Pages(51_035)))
        );
        assert_eq!(
            health(Some("128"), Some(killed)),
            Health::Unfit(Unfit::Oopsed),
            "oops first"
        );
        assert_eq!(health(None, None), Health::Sound);
    }

    #[test]
    fn reboots_at_most_once_in_six_hours() {
        let now = 1_795_000_000;
        assert_eq!(remedy(None, now), Remedy::Reboot);
        assert_eq!(
            remedy(Some(now - 3600), now),
            Remedy::StayAwake {
                rebooted_ago: Duration::from_secs(3600)
            }
        );
        assert_eq!(remedy(Some(now - 6 * 3600), now), Remedy::Reboot);
        assert_eq!(
            remedy(Some(now + 60), now),
            Remedy::Reboot,
            "clock went back"
        );
    }

    #[test]
    fn reads_the_last_reboot() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("health-reboot");
        assert_eq!(last_reboot(&path), None);
        std::fs::write(&path, "1795000000\n").unwrap();
        assert_eq!(last_reboot(&path), Some(1_795_000_000));
    }
}
