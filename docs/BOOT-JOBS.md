# Amazon's upstart jobs at boot, and which an overlay could keep from starting

The question: on a standalone boot, could runtime overlays (overlay.rs:
Amazon's job file with its `start on` removed, bound over the original by
`launcher boot`) keep Amazon's services that kindil stops today (main.rs's
SERVICES, plus `lab126_gui`, `x`, `powerd`) and Amazon's telemetry (`cron`,
`syslog`) from ever starting?

**Answer: no, not while kindil's job starts at `started volumd`.** Every one
of them is committed to start before kindil's job can lay an overlay.
Overlaying a job after that changes nothing for this boot. Upstart keeps a
class whose instance exists and uses the new file only for the next start.

Sources: the 109 files of `/etc/upstart` from the system-partition backup of
2026-11-15 (`framework.conf` md5 `cf6613a4…`, as on the device), and the
upstart 0.6.6 source (`init/event.c`, `init/job.c`, `init/job_class.c`).

## Why "the same event" means "too late"

`kindil.conf` starts on `started volumd`. `volumd` starts on
`started lab126`, and so do all eleven of SERVICES, plus `powerd`, `wifid`,
`usbnetd`, `testd` and `wand`.

In upstart 0.6.6, `event_poll` keeps handling the event queue until it is
empty (`do { … } while (poll_again)`) before it returns to the main loop.
Handling `started lab126` gives every class whose `start on` matches a start
goal (`event_pending` → `job_change_goal`), all in that one pass. A job with
no pre-start or post-start process goes from `starting` through spawning
its main process to `running`, and emits `started JOB`, without leaving
that pass (`job_change_state`: SPAWNED → POST_START → RUNNING when there is
nothing to wait for). That covers `cmd`, `powerd` and `volumd`, so
`started cmd` (→ `tmd`, `phd`), `started powerd` (→ `deviced`) and
`started volumd` (→ `kindil`, `usbnet`, `usbnet-autostart`) are handled in
the same pass too. kindil's job only forks its script there. The launcher's
`initctl reload-configuration` is a D-Bus request, which upstart serves only
from the main loop, after the pass. By then every job above has its start
goal.

A job with no `start on` is fine in 0.6.6, for what it's worth:
`job_class_new` leaves `start_on` NULL, `event_pending` skips such a class
(`if (class->start_on && …)`), and `job_class_start` (behind `start JOB`)
never looks at it. The mechanism works; there is nothing left for it to act
on.

## The map

### Before kindil's job runs (still start on every boot, standalone too)

```
startup → system
  started system → syslog (syslog-ng), cron (crond), console, recevent,
                   network, sshd, system_cramfs_loopbacks
    mounted_cramfs_loopbacks → modules → loaded_modules_dependencies, …
  started system + loaded_fuse + started syslog → filesystems
    → filesystems_var_local (mounted_varLocal → display, touch, prereg;
      migrate_DB → migrate_db), printklogs
    → filesystems_userstore (+ display_ready)
    → filesystems_setup
  display_ready + loaded_modules_dependencies → battery → battery_ready
  battery_ready + loaded_touch + … + started filesystems_setup → system_setup
    → devcap → devcap_ready → dbus, early_progress (+ display_ready)
    → system_setup_after_shpm
  dbus post-start emits dbus_ready (unless ota-update runs)
    → lab126 (no processes: started at once)
      started lab126 → appmgrd archive cmd dpmd dynconfig otaupd otav3 rcm
                       stackdumpd todo        (SERVICES, but deviced)
                       powerd wifid volumd usbnetd testd wand
        started cmd    → tmd, phd
        started powerd → deviced              (SERVICES)
        starting volumd → usbnet-preinit
        started volumd → kindil, usbnet; + started usbnetd → usbnet-autostart
  started dbus (after dbus's post-start, a moment after dbus_ready)
    → x, lipcd
```

- **Telemetry:** `syslog` (syslog-ng, `expect fork`) and `cron`
  (`crond -f -c /etc/crontab`) start on `started system`, among the very
  first jobs, long before `volumd`. The session's stopper stops `cron`
  (in SERVICES) and `restore()` starts it again; `syslog`
  stays.
- **`x`** starts on `started dbus`, which follows `dbus_ready` by the rest
  of dbus's post-start script (one `f_log`). In principle that is a race
  with kindil's job. In practice it is lost: the launcher has `/bin/sh`,
  `boot.sh`, the launcher's exec, the binds and a D-Bus round trip to get
  through first. `launcher boot` keeps stopping `x` (during its slow
  pre-start, `makexconfig`), as it does today.
- **`wand`'s** pre-start fails on the Wi-Fi-only PW1 (`f_wan`), so
  `started wand` (→ `mcsd`, `monitor_wand_threads`) never comes.

### After kindil's job (already cut by stopping `x`)

Only events a process emits later, or a pre/post-start process finishing,
come after kindil's job. On this firmware those are all in X's tree:

```
x emits lxinit_ready → x_setup, vnc
  x_setup_ready + blanket_loaded_langpicker → langpicker
    langpicker_ready → lab126_gui, x_cleanup
      started lab126_gui → kb, pillow, splash, contentpackd,
                           contentpack_fonts_dependency
        contentpack_font_ready → framework (skipped by DONT_START_FRAMEWORK),
                                 webreader
          started framework → framework_setup → framework_ready
            → acxmgrd, bridge, dmld, poll_daemons, scanner, stored,
              lab126_gui_setup (+ started kb, started pillow)
            → kindle (started poll_daemons, kb, pillow, acxmgrd, cmd)
  on failure: x_monitor, lab126_gui_monitor
```

A standalone boot's `stop x` cuts this tree at its root: none of it
starts. Overlaying any of these jobs would only repeat that.

Manual jobs (no `start on`): `ota-update`, `wifim`, `wifis` (wifid starts
them), `wfmdelete`, `wfmupdate`. Also `tmpfs_scan` (its own event) and
`shutdown` (runlevel 0/6).

## What would work

Overlays have to be laid before `started lab126` is handled. With
`kindil.conf` fixed at `started volumd`, kindil never gets a chance. The
one change that would give it one is a job file on the system partition,
which this step rules out:

- A boot job on **`starting lab126`**, as a `task` that runs the overlays
  in the foreground: upstart holds `lab126` in `starting` until the task
  finishes. `/mnt/us` and `/var/local` are up by then (`filesystems_setup`
  comes before `system_setup` → `dbus`). Upstart keeps serving D-Bus while
  the event is blocked, so `reload-configuration` lands before
  `started lab126` is handled. The quiet jobs would then never get a start
  goal, and nor would `tmd`/`phd`/`deviced` behind `cmd`/`powerd`. Any
  failure in that task delays Amazon's whole boot, so it must be tiny and
  bounded, and it changes the one file kindil adds to the system.
- `cron` and `syslog` would need the task earlier still (`starting system`,
  before `/mnt/us` is mounted), so they stay out of reach either way.

Until then, the session's stopper (session.rs) stays the way these jobs go
away, and `restore()` brings them back.

## Checking the order on the device (read-only)

Process ids are given out in order. After a standalone boot:

```sh
$K "$P ps | grep -E 'syslog-ng|crond|dbus-daemon|cmd -f|powerd -f|volumd -f|deviced -f|tmd|phd|kindil/bin/boot.sh|/var/local/kindil/launcher' | grep -v grep"
```

Expect `syslog-ng` and `crond` far below the rest, and every service's pid
below that of `boot.sh` (or of the `launcher boot` it runs). `initctl list`
shows which jobs ran. Expect all of the "before" list (bar `wand`, `x`, and
the SERVICES the session has since stopped) and none of the "after" tree.
