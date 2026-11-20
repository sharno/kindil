# Charging on a charger left plugged in: the device test

On 0.17.0, dozing on a charger all night, the PW1 stopped charging at 100%
and never started again: at 05:04 UTC on 2026-11-19 the chip read 90% and
discharging with `connected` at 1. The charger driver's own stop-and-restart
rule only runs while its work is scheduled, and each suspend cancels it.
kindil now runs that rule itself on every reading of the gauge chip (see
**Charging** in [HOW-IT-WORKS.md](HOW-IT-WORKS.md)). This is the check on the
device, with kindil dozing (the default) unless a step says otherwise.

```sh
K="ssh -i ~/.ssh/sharno-desktop -o IdentitiesOnly=yes -o IdentityAgent=none root@192.168.1.130"
P='export PATH=/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin;'
C=/sys/devices/platform/aplite_charger.0
# The switch and the charger; the chip beside the driver's cache.
$K "$P cat $C/connected $C/charging; /mnt/us/extensions/kindil/bin/kindil gauge"
$K "$P grep -e 'kindil: charg' /var/local/kindil/kindil.log | tail"
```

SSH reaches a dozing Kindle only in its 2-minute window every 2 hours; a
press of the power button wakes it for the checks (it then dozes again
after 2 minutes), and each charging line stays in kindil.log meanwhile.

## 1. Plugged in at 95% or below: charging starts within 5 minutes

Plug in with the battery at 95% or below (the overnight state: the driver
stopped at full earlier, the chip discharging) and let it doze.

- Within 5 minutes (the next full redraw), kindil.log has `kindil: charging
  restarted at N%: stopped on a charger (N%, -M mA, T °F)`, unless the
  driver's plug-in already started charging: then nothing is logged, which
  is right.
- A later `kindil gauge` shows the chip's current positive (`+` mA) and the
  percentage climbing. A positive current is also the first sight of the
  chip's charging flag (FLAGS bit 7) set while charging, which the current's
  sign comes from: if it stays negative while the percentage climbs, the
  flag is wrong while charging, and kindil would restart every 30 minutes
  and never stop at full: report it.
- A `restarted` line again 30 minutes later, with the chip still
  discharging, means writing the switch doesn't restart the charger.

## 2. At 100%: charging stops

Leave it plugged in until it reaches 100%.

- kindil.log: `kindil: charging stopped at full (100%, +N mA, T °F)`, with N
  under 55. `charging` then reads 0, and the chip's current goes negative
  (the device runs on the cell).
- Over the next hours, as it drains to 95%: `kindil: charging restarted at
  95% (95%, -N mA, T °F)`, `charging` reads 1 again and the current turns
  positive. On the charger all night, the percentage stays between 95 and
  100.

## 3. Unplug and plug in again

- Unplug while it is held at full (`charging` 0 on the charger): the next
  reading logs `kindil: charger unplugged; charging switch back on`.
  Unplugged, `charging` reads 0 whatever was written.
- Plug in again at 96-99%: the driver charges on its plug-in, kindil logs
  nothing, and at 100% section 2 applies again.
- Unplug and plug in again within the 5 minutes between two readings while
  held: the switch reads 1 again (the driver's plug-in set the charge
  current), kindil drops its hold silently and stops again at full.

## 4. Leaving kindil, and a crash

- Held at full on the charger, choose Exit to Kindle: kindil.log ends with
  `kindil: charging switch back on for the driver`, and `charging` reads 1
  under Amazon's UI.
- Held at full, `kill -9` kindil: the launcher restarts it, and its first
  reading logs `kindil: charging found off; held until 95% (...)`; below
  95% it logs `charging restarted at N%` instead.
