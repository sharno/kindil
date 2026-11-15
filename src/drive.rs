//! USB drive mode run by kindil itself instead of Amazon's volumd: the user
//! storage goes to a computer over USB and comes back, checked.
//!
//! Off unless [`SWITCH`] holds `kindil` (see [`Owner`]); then the launcher
//! stops volumd once kindil runs, and this module does its work. Without it,
//! volumd keeps doing it and kindil only follows its events (src/kindle.rs).
//!
//! The storage is the FAT32 at byte 8192 of [`DEVICE`]: the loop device
//! [`LOOP`] mounted on `/mnt/base-us` ([`Mount::Base`]), which Amazon's FUSE
//! filesystem `fsp` shows at `/mnt/us` ([`Mount::Fsp`]). The computer gets
//! the whole of [`DEVICE`] through the mass storage gadget's LUN. One rule
//! keeps the FAT safe, checked before every LUN write and every mount: the
//! LUN holds the storage and `/mnt/base-us` is mounted never at the same
//! time.
//!
//! Three layers, each tested against [`Ops`] faked:
//! - [`State::step`]: pure, from kernel signals to what to do. A computer
//!   gets the storage once it has configured the gadget for
//!   [`SETTLE`]; a wall charger never configures it.
//! - [`export`], [`bring_back`] and [`recover`]: the sequences, over [`Ops`].
//! - [`Driver`]: runs the two, and tells the UI ([`Ui`]).
use crate::system::Event;
use crate::uevent::{Action, Uevent};
use std::path::Path;
use std::time::{Duration, Instant};

/// Holding `kindil`: kindil runs USB drive mode. Absent, or anything else:
/// Amazon's volumd does. On the system storage, so a computer that has the
/// user storage never takes the switch with it.
pub const SWITCH: &str = "/var/local/kindil/usb-drive";
/// Present from just before the computer gets the storage until it is
/// back and checked: if kindil or the Kindle stops in between, the next
/// start checks the storage before using it.
pub const EXPORTED: &str = "/var/local/kindil/usb-exported";
/// What the computer gets: the whole user partition, as volumd gives it.
pub const DEVICE: &str = "/dev/mmcblk0p4";
/// The FAT32 inside [`DEVICE`], which `/mnt/base-us` mounts.
pub const LOOP: &str = "/dev/loop/0";
/// How long a computer must keep the gadget configured before it gets the
/// storage: a flaky cable doesn't unmount it every second.
pub const SETTLE: Duration = crate::system::DRIVE_SETTLE;
/// Syncthing, just stopped, may still be closing its files.
const HOLDERS_GONE: Duration = Duration::from_secs(5);
const HOLDERS_EVERY: Duration = Duration::from_millis(500);
/// How long the LUN gets to let go before the gadget is unloaded instead.
const LUN_TRIES: u32 = 5;
const LUN_EVERY: Duration = Duration::from_millis(400);

/// Who runs USB drive mode.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Owner {
    /// Amazon's volumd, as on a stock Kindle.
    #[default]
    Amazon,
    Kindil,
}

impl Owner {
    pub fn parse(text: &str) -> Self {
        match text.trim() {
            "kindil" => Owner::Kindil,
            _ => Owner::Amazon,
        }
    }

    /// [`Owner::Amazon`] unless `switch` says otherwise.
    pub fn read(switch: &Path) -> Self {
        std::fs::read_to_string(switch).map_or(Owner::Amazon, |text| Self::parse(&text))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mount {
    /// `/mnt/us`: `fsp` over `/mnt/base-us`; what kindil and everything
    /// else use.
    Fsp,
    /// `/mnt/base-us`: the FAT32 itself.
    Base,
}

impl Mount {
    pub fn path(self) -> &'static str {
        match self {
            Mount::Fsp => "/mnt/us",
            Mount::Base => "/mnt/base-us",
        }
    }
}

/// The USB gadget driver loaded; at most one is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Gadget {
    None,
    /// `g_ether`: USBNetwork's.
    Ether,
    /// `g_file_storage removable=y`: USB drive mode's.
    Storage,
}

impl Gadget {
    pub fn module(self) -> Option<&'static str> {
        match self {
            Gadget::None => None,
            Gadget::Ether => Some("g_ether"),
            Gadget::Storage => Some("g_file_storage"),
        }
    }
}

/// A process's name as the kernel keeps it: at most 15 bytes. Fixed-size,
/// so events carrying it stay `Copy`.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Name {
    bytes: [u8; 16],
    len: u8,
}

impl Name {
    pub fn new(name: &str) -> Self {
        let mut bytes = [0; 16];
        // Whole characters only.
        let len = name
            .char_indices()
            .map(|(i, c)| i + c.len_utf8())
            .take_while(|&end| end <= 15)
            .last()
            .unwrap_or(0);
        bytes[..len].copy_from_slice(&name.as_bytes()[..len]);
        Self {
            bytes,
            len: len as u8,
        }
    }

    pub fn as_str(&self) -> &str {
        std::str::from_utf8(&self.bytes[..usize::from(self.len)]).unwrap_or("?")
    }
}

impl std::fmt::Debug for Name {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}", self.as_str())
    }
}

impl std::fmt::Display for Name {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A process with a file, a mapping or its working directory on a mount.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Holder {
    pub pid: u32,
    pub name: Name,
    pub path: String,
}

/// Why the computer didn't get the storage. The Kindle keeps it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// A program other than kindil has files open on the storage.
    Busy { pid: u32, name: Name },
    /// The storage wouldn't unmount, or something mounted it again.
    Unmount(Mount),
    /// The mass storage gadget wouldn't load or take the storage.
    Gadget,
    /// kindil couldn't mark the storage as away, to check it after a yank.
    Flag,
    /// The cable went before the computer got the storage.
    Unplugged,
}

/// `fsck.vfat -a` on the storage back from a computer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Check {
    /// Exit 0.
    Clean,
    /// Exit 1: errors found and repaired.
    Repaired,
    /// Exit 2 or more, or it didn't run (`None`): the storage is mounted
    /// read-only.
    Failed(Option<i32>),
}

impl Check {
    pub fn of(exit: std::io::Result<i32>) -> Self {
        match exit {
            Ok(0) => Check::Clean,
            Ok(1) => Check::Repaired,
            Ok(code) => Check::Failed(Some(code)),
            Err(_) => Check::Failed(None),
        }
    }

    pub fn read_only(self) -> bool {
        matches!(self, Check::Failed(_))
    }
}

/// What the UI says about USB drive mode beyond its screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Notice {
    /// The computer didn't get the storage; the Kindle keeps it.
    Refused(Refusal),
    /// Back from the computer, but `fsck` couldn't make it clean: mounted
    /// read-only until the next start checks it again.
    ReadOnly(Check),
    /// Back from the computer, and repaired.
    Repaired,
    /// The storage couldn't be taken back from the gadget or mounted.
    Lost,
    /// Checked at start, after an export that never came back (a crash, or a
    /// reboot with the computer attached).
    Checked(Check),
    /// That check couldn't run: the storage stays as it is, unchecked.
    Unchecked(Refusal),
}

impl Notice {
    /// The storage left kindil's hands (or might have) and is back: reload
    /// the library, resume Syncthing.
    pub fn storage_back(self) -> bool {
        match self {
            Notice::Refused(_) | Notice::Checked(_) | Notice::Unchecked(_) => true,
            Notice::ReadOnly(_) | Notice::Repaired | Notice::Lost => false,
        }
    }
}

/// What the sequences do to the device. Every call is one plain step that
/// either happened or reports why not; none is lazy (no `umount -l`).
pub trait Ops {
    /// Writes every dirty buffer out.
    fn sync(&self);
    /// Every process holding something on `mount`.
    fn holders(&self, mount: Mount) -> Vec<Holder>;
    /// From `/proc/mounts`.
    fn mounted(&self, mount: Mount) -> bool;
    fn umount(&self, mount: Mount) -> std::io::Result<()>;
    fn mount(&self, mount: Mount, read_only: bool) -> std::io::Result<()>;
    fn gadget(&self) -> Gadget;
    fn load(&self, gadget: Gadget) -> std::io::Result<()>;
    fn unload(&self, gadget: Gadget) -> std::io::Result<()>;
    /// The LUN's backing file, `""` when empty; `None` without a LUN (the
    /// storage gadget isn't loaded).
    fn lun(&self) -> std::io::Result<Option<String>>;
    /// `""` empties it.
    fn set_lun(&self, file: &str) -> std::io::Result<()>;
    /// Drops [`LOOP`]'s cached blocks, which predate the computer's writes.
    fn flush(&self);
    /// `fsck.vfat -a` on [`LOOP`]: its exit code.
    fn fsck(&self) -> std::io::Result<i32>;
    /// [`EXPORTED`].
    fn exported(&self) -> bool;
    fn set_exported(&self, on: bool) -> std::io::Result<()>;
    /// The cable: a computer or a wall charger.
    fn plugged(&self) -> bool;
    fn wait(&self, time: Duration);
}

fn empty(lun: &std::io::Result<Option<String>>) -> bool {
    match lun {
        Ok(None) => true,
        Ok(Some(file)) => file.is_empty(),
        // Unknown: as good as set.
        Err(_) => false,
    }
}

/// The invariant's two halves.
fn may_set_lun(ops: &dyn Ops) -> bool {
    !ops.mounted(Mount::Base) && !ops.mounted(Mount::Fsp)
}

fn may_mount(ops: &dyn Ops) -> bool {
    empty(&ops.lun())
}

/// The first process other than kindil's own that holds the storage, once
/// [`HOLDERS_GONE`] has passed. `fsp` holds `/mnt/base-us` for `/mnt/us`,
/// and goes when `/mnt/us` is unmounted.
fn busy(ops: &dyn Ops) -> Option<Holder> {
    let tries = HOLDERS_GONE.as_millis() / HOLDERS_EVERY.as_millis();
    let mut attempt = 0;
    loop {
        let holder = ops
            .holders(Mount::Fsp)
            .into_iter()
            .chain(
                ops.holders(Mount::Base)
                    .into_iter()
                    .filter(|h| h.name.as_str() != "fsp"),
            )
            .next()?;
        if attempt == tries {
            return Some(holder);
        }
        attempt += 1;
        ops.wait(HOLDERS_EVERY);
    }
}

/// Mounts whatever isn't, as kindil had it; no check: nothing else wrote it.
fn remount(ops: &dyn Ops) {
    if !may_mount(ops) {
        eprintln!("kindil: USB drive: the LUN holds the storage; not mounting it");
        return;
    }
    for mount in [Mount::Base, Mount::Fsp] {
        if !ops.mounted(mount) {
            if let Err(error) = ops.mount(mount, false) {
                eprintln!("kindil: USB drive: mounting {}: {error}", mount.path());
            }
        }
    }
}

/// The storage gadget in place of `previous`.
fn swap_in(ops: &dyn Ops, previous: Gadget) -> std::io::Result<()> {
    match previous {
        Gadget::Storage => {}
        Gadget::Ether => {
            ops.unload(Gadget::Ether)?;
            ops.load(Gadget::Storage)?;
        }
        Gadget::None => ops.load(Gadget::Storage)?,
    }
    match ops.lun()? {
        Some(_) => Ok(()),
        None => Err(std::io::Error::other("the storage gadget has no LUN")),
    }
}

/// `previous` back, if it was USBNetwork's; otherwise the storage gadget,
/// with an empty LUN: with it loaded, plugging into a computer is noticed.
fn restore_gadget(ops: &dyn Ops, previous: Gadget) {
    let want = match previous {
        Gadget::Ether => Gadget::Ether,
        Gadget::None | Gadget::Storage => Gadget::Storage,
    };
    let now = ops.gadget();
    if now == want {
        return;
    }
    let swapped = (|| {
        if now != Gadget::None {
            ops.unload(now)?;
        }
        ops.load(want)
    })();
    if let Err(error) = swapped {
        eprintln!("kindil: USB drive: loading {want:?}: {error}");
    }
}

/// Undoes an export that stopped before the computer got the storage.
fn undo(ops: &dyn Ops, previous: Gadget, refusal: Refusal) -> Refusal {
    restore_gadget(ops, previous);
    remount(ops);
    refusal
}

/// The computer has the storage; what to restore when it's back.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Exported {
    pub previous: Gadget,
}

/// Hands the storage to the computer: refused while anything but kindil
/// holds it (kindil releases its own first); synced; `/mnt/us` then
/// `/mnt/base-us` unmounted and seen unmounted; the storage gadget in
/// place of USBNetwork's; and only then the partition in the LUN.
pub fn export(ops: &dyn Ops) -> Result<Exported, Refusal> {
    if let Some(holder) = busy(ops) {
        eprintln!(
            "kindil: USB drive: {} ({}) holds {}",
            holder.name, holder.pid, holder.path
        );
        return Err(Refusal::Busy {
            pid: holder.pid,
            name: holder.name,
        });
    }
    ops.sync();
    for mount in [Mount::Fsp, Mount::Base] {
        if ops.mounted(mount) {
            if let Err(error) = ops.umount(mount) {
                eprintln!("kindil: USB drive: unmounting {}: {error}", mount.path());
                remount(ops);
                return Err(Refusal::Unmount(mount));
            }
        }
    }
    if let Some(mount) = [Mount::Fsp, Mount::Base]
        .into_iter()
        .find(|&m| ops.mounted(m))
    {
        eprintln!("kindil: USB drive: {} still mounted", mount.path());
        remount(ops);
        return Err(Refusal::Unmount(mount));
    }
    let previous = ops.gadget();
    if let Err(error) = swap_in(ops, previous) {
        eprintln!("kindil: USB drive: the storage gadget: {error}");
        return Err(undo(ops, previous, Refusal::Gadget));
    }
    if !ops.plugged() {
        return Err(undo(ops, previous, Refusal::Unplugged));
    }
    if !may_set_lun(ops) {
        eprintln!("kindil: USB drive: the storage was mounted again; not exporting");
        return Err(undo(ops, previous, Refusal::Unmount(Mount::Base)));
    }
    if let Err(error) = ops.set_exported(true) {
        eprintln!("kindil: USB drive: marking the storage away: {error}");
        return Err(undo(ops, previous, Refusal::Flag));
    }
    if let Err(error) = ops.set_lun(DEVICE) {
        eprintln!("kindil: USB drive: writing the LUN: {error}");
        if empty(&ops.lun()) {
            let _ = ops.set_exported(false);
        }
        return Err(undo(ops, previous, Refusal::Gadget));
    }
    eprintln!("kindil: USB drive: the computer has the storage");
    Ok(Exported { previous })
}

/// The storage out of the LUN: emptied, or, if the gadget won't let go
/// (the computer locked it), the gadget unloaded. True once it's out.
fn release_lun(ops: &dyn Ops) -> bool {
    for attempt in 0..LUN_TRIES {
        if empty(&ops.lun()) {
            return true;
        }
        if let Err(error) = ops.set_lun("") {
            eprintln!("kindil: USB drive: emptying the LUN: {error}");
        }
        if empty(&ops.lun()) {
            return true;
        }
        if attempt + 1 < LUN_TRIES {
            ops.wait(LUN_EVERY);
        }
    }
    eprintln!("kindil: USB drive: the LUN won't empty; unloading the gadget");
    if let Err(error) = ops.unload(Gadget::Storage) {
        eprintln!("kindil: USB drive: unloading g_file_storage: {error}");
    }
    empty(&ops.lun())
}

/// The storage is back with the Kindle.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Back {
    /// `None`: the computer never had it, nothing to check.
    pub check: Option<Check>,
}

/// The storage couldn't come back: the LUN wouldn't let go, or it wouldn't
/// mount.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Lost;

/// Takes the storage back from the computer: out of the LUN and seen out,
/// checked if the computer had it, mounted (read-only if the check failed),
/// and `previous` gadget restored.
pub fn bring_back(ops: &dyn Ops, previous: Gadget) -> Result<Back, Lost> {
    if !release_lun(ops) {
        return Err(Lost);
    }
    let back = check_and_mount(ops)?;
    restore_gadget(ops, previous);
    Ok(back)
}

/// With the storage unmounted and out of the LUN.
fn check_and_mount(ops: &dyn Ops) -> Result<Back, Lost> {
    let check = ops.exported().then(|| {
        ops.flush();
        let check = Check::of(ops.fsck());
        eprintln!("kindil: USB drive: storage check: {check:?}");
        check
    });
    let read_only = check.is_some_and(Check::read_only);
    // The invariant, again: nothing may have set the LUN since.
    if !may_mount(ops) {
        eprintln!("kindil: USB drive: the LUN holds the storage; not mounting it");
        return Err(Lost);
    }
    for mount in [Mount::Base, Mount::Fsp] {
        if !ops.mounted(mount) {
            // fsp over a read-only FAT is read-only too.
            let read_only = read_only && mount == Mount::Base;
            if let Err(error) = ops.mount(mount, read_only) {
                eprintln!("kindil: USB drive: mounting {}: {error}", mount.path());
                return Err(Lost);
            }
        }
    }
    // A storage left unchecked is checked again at the next start.
    if !read_only {
        if let Err(error) = ops.set_exported(false) {
            eprintln!("kindil: USB drive: clearing {EXPORTED}: {error}");
        }
    }
    Ok(Back { check })
}

/// How kindil found the storage at start.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Found {
    /// Mounted, nothing to do.
    Mounted,
    /// Still in the LUN: an earlier kindil exported it and stopped.
    Exported,
    /// Mounted again after an earlier kindil stopped mid-way.
    Remounted,
    /// [`EXPORTED`] was set: unmounted, checked and mounted again.
    Checked(Check),
    /// The check couldn't run.
    Unchecked(Refusal),
    Lost,
}

/// Puts the storage right at start, after a kindil or a Kindle that stopped
/// while a computer had it (the boot's own mount may cover a FAT the
/// computer left half-written). The UI has released the storage.
pub fn recover(ops: &dyn Ops) -> Found {
    if !empty(&ops.lun()) {
        return Found::Exported;
    }
    let mounted = ops.mounted(Mount::Fsp) || ops.mounted(Mount::Base);
    if !ops.exported() {
        if ops.mounted(Mount::Fsp) && ops.mounted(Mount::Base) {
            return Found::Mounted;
        }
        remount(ops);
        return Found::Remounted;
    }
    if mounted {
        if let Some(holder) = busy(ops) {
            eprintln!(
                "kindil: USB drive: can't check the storage: {} ({}) holds {}",
                holder.name, holder.pid, holder.path
            );
            return Found::Unchecked(Refusal::Busy {
                pid: holder.pid,
                name: holder.name,
            });
        }
        ops.sync();
        for mount in [Mount::Fsp, Mount::Base] {
            if ops.mounted(mount) {
                if let Err(error) = ops.umount(mount) {
                    eprintln!("kindil: USB drive: unmounting {}: {error}", mount.path());
                    remount(ops);
                    return Found::Unchecked(Refusal::Unmount(mount));
                }
            }
        }
    }
    match check_and_mount(ops) {
        Ok(Back { check: Some(check) }) => Found::Checked(check),
        Ok(Back { check: None }) => Found::Remounted,
        Err(Lost) => Found::Lost,
    }
}

/// What a kernel uevent says about the cable, if anything.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Signal {
    /// The charger saw the cable come or go: read whether it's there.
    Cable,
    /// A computer configured the gadget.
    Online,
    /// It deconfigured it.
    Offline,
    /// The storage gadget changed: read the LUN (an eject empties it).
    Gadget,
}

/// The PW1's USB devices, as its udev rules name them
/// (`/etc/udev/rules.d/10-hw.rules`).
pub fn signal(event: &Uevent) -> Option<Signal> {
    let name = event.devpath.rsplit('/').next()?;
    match (name, &event.action) {
        ("fsl-usb2-udc", Action::Online) => Some(Signal::Online),
        ("fsl-usb2-udc", Action::Offline) => Some(Signal::Offline),
        ("fsl-usb2-udc" | "aplite_charger.0", _) => Some(Signal::Cable),
        ("g_file_storage" | "g_ether" | "gadget" | "gadget-lun0", _) => Some(Signal::Gadget),
        _ => None,
    }
}

/// Whether a uevent concerns USB drive mode: `kindil uevents --usb`.
pub fn is_usb(event: &Uevent) -> bool {
    signal(event).is_some()
        || ["usb", "udc", "gadget", "charger", "storage", "lun"]
            .iter()
            .any(|word| event.devpath.contains(word))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Input {
    Plugged,
    Unplugged,
    Online,
    Offline,
    /// The computer ejected the storage: the LUN emptied while exported.
    Ejected,
    /// [`SETTLE`] since this instant.
    Settled(Instant),
    /// How [`Command::Export`] went: the gadget to restore, or why not.
    Exported(Result<Gadget, Refusal>),
    /// [`Command::Return`] is done; whether the cable is still in.
    Returned {
        plugged: bool,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Command {
    /// Deliver [`Input::Settled`] with this instant after [`SETTLE`].
    Await(Instant),
    Export,
    /// Bring the storage back, then restore this gadget.
    Return(Gadget),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    /// The Kindle has the storage.
    Kindle {
        plugged: bool,
    },
    /// A computer configured the gadget; it gets the storage once that held
    /// for [`SETTLE`].
    Settling {
        since: Instant,
    },
    Exporting,
    /// The computer has the storage.
    Computer {
        previous: Gadget,
    },
    Returning,
    /// Back, or refused, with the cable still in: the computer gets the
    /// storage again only after a new plug. An eject means "done".
    Held,
}

impl State {
    /// The next state and what to do, or `None` if `input` changes nothing.
    pub fn step(self, input: Input, now: Instant) -> Option<(State, Option<Command>)> {
        use Input as I;
        use State as S;
        Some(match (self, input) {
            (S::Kindle { .. }, I::Plugged) => (S::Kindle { plugged: true }, None),
            (S::Kindle { .. }, I::Unplugged) => (S::Kindle { plugged: false }, None),
            (S::Kindle { .. }, I::Online) => {
                (S::Settling { since: now }, Some(Command::Await(now)))
            }
            (S::Settling { since }, I::Settled(at)) if at == since => {
                (S::Exporting, Some(Command::Export))
            }
            (S::Settling { .. }, I::Offline) => (S::Kindle { plugged: true }, None),
            (S::Settling { .. }, I::Unplugged) => (S::Kindle { plugged: false }, None),
            (S::Exporting, I::Exported(Ok(previous))) => (S::Computer { previous }, None),
            (S::Exporting, I::Exported(Err(Refusal::Unplugged))) => {
                (S::Kindle { plugged: false }, None)
            }
            (S::Exporting, I::Exported(Err(_))) => (S::Held, None),
            (S::Computer { previous }, I::Unplugged | I::Ejected) => {
                (S::Returning, Some(Command::Return(previous)))
            }
            (S::Returning, I::Returned { plugged: true }) => (S::Held, None),
            (S::Returning, I::Returned { plugged: false }) => (S::Kindle { plugged: false }, None),
            (S::Held, I::Unplugged) => (S::Kindle { plugged: false }, None),
            // The gadget swap, or the computer, repeating itself; or a
            // timer or result for a state already left.
            _ => return None,
        })
    }

    /// When [`Input::Settled`] is due.
    pub fn due(self) -> Option<(Instant, Instant)> {
        match self {
            State::Settling { since } => Some((since, since + SETTLE)),
            _ => None,
        }
    }
}

/// The UI's side, from the driver's thread.
pub trait Ui {
    /// Delivers [`Event::StorageLeaving`] and returns once the UI has let go
    /// of the storage (Syncthing, saved choices), or after a while.
    fn release(&self);
    fn deliver(&self, event: Event);
}

/// Runs USB drive mode: feed it [`Input`]s, call [`Driver::poll`] while a
/// computer has the storage.
pub struct Driver<'a> {
    ops: &'a dyn Ops,
    state: State,
}

impl<'a> Driver<'a> {
    /// Recovers the storage and loads the storage gadget, if no gadget is
    /// loaded, so a computer plugged in is noticed.
    pub fn start(ops: &'a dyn Ops, ui: &dyn Ui) -> Self {
        // The check unmounts the storage the UI has open.
        if ops.exported() && empty(&ops.lun()) {
            ui.release();
        }
        let found = recover(ops);
        eprintln!("kindil: USB drive: started; storage {found:?}");
        let notice = match found {
            Found::Checked(check) => Some(Notice::Checked(check)),
            Found::Unchecked(refusal) => Some(Notice::Unchecked(refusal)),
            Found::Lost => Some(Notice::Lost),
            Found::Mounted | Found::Exported | Found::Remounted => None,
        };
        if let Some(notice) = notice {
            ui.deliver(Event::DriveNotice(notice));
        }
        if ops.gadget() == Gadget::None {
            if let Err(error) = ops.load(Gadget::Storage) {
                eprintln!("kindil: USB drive: loading g_file_storage: {error}");
            }
        }
        let state = if found == Found::Exported {
            ui.deliver(Event::Drive(crate::system::Drive::Computer));
            State::Computer {
                previous: Gadget::Storage,
            }
        } else {
            State::Kindle {
                plugged: ops.plugged(),
            }
        };
        Self { ops, state }
    }

    pub fn state(&self) -> State {
        self.state
    }

    /// A kernel signal.
    pub fn signal(&mut self, signal: Signal, now: Instant, ui: &dyn Ui) {
        match signal {
            Signal::Cable => {
                let input = if self.ops.plugged() {
                    Input::Plugged
                } else {
                    Input::Unplugged
                };
                self.input(input, now, ui);
            }
            Signal::Online => self.input(Input::Online, now, ui),
            Signal::Offline => self.input(Input::Offline, now, ui),
            Signal::Gadget => self.poll(now, ui),
        }
    }

    /// While a computer has the storage, notices an eject; also a timer
    /// that is due. Call every second or so.
    pub fn poll(&mut self, now: Instant, ui: &dyn Ui) {
        if let Some((since, due)) = self.state.due() {
            if now >= due {
                self.input(Input::Settled(since), now, ui);
            }
        }
        if let State::Computer { .. } = self.state {
            if empty(&self.ops.lun()) {
                eprintln!("kindil: USB drive: ejected by the computer");
                self.input(Input::Ejected, now, ui);
            } else if !self.ops.plugged() {
                // A remove event lost in a burst.
                self.input(Input::Unplugged, now, ui);
            }
        }
    }

    pub fn input(&mut self, input: Input, now: Instant, ui: &dyn Ui) {
        let Some((next, command)) = self.state.step(input, now) else {
            return;
        };
        if next != self.state {
            eprintln!(
                "kindil: USB drive: {:?} -> {next:?} on {input:?}",
                self.state
            );
        }
        self.state = next;
        match command {
            None | Some(Command::Await(_)) => {}
            Some(Command::Export) => {
                ui.release();
                let result = export(self.ops);
                match result {
                    Ok(_) => ui.deliver(Event::Drive(crate::system::Drive::Computer)),
                    Err(refusal) => ui.deliver(Event::DriveNotice(Notice::Refused(refusal))),
                }
                self.input(Input::Exported(result.map(|e| e.previous)), now, ui);
            }
            Some(Command::Return(previous)) => {
                let back = bring_back(self.ops, previous);
                ui.deliver(Event::Drive(crate::system::Drive::Kindle));
                let notice = match back {
                    Ok(Back {
                        check: Some(check @ Check::Failed(_)),
                    }) => Some(Notice::ReadOnly(check)),
                    Ok(Back {
                        check: Some(Check::Repaired),
                    }) => Some(Notice::Repaired),
                    Ok(_) => None,
                    Err(Lost) => Some(Notice::Lost),
                };
                if let Some(notice) = notice {
                    ui.deliver(Event::DriveNotice(notice));
                }
                let plugged = self.ops.plugged();
                self.input(Input::Returned { plugged }, now, ui);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::system::Drive;
    use std::cell::{Cell, RefCell};

    /// The device, played: mounts, the gadget, the LUN and the cable, with
    /// every operation logged. It panics if the invariant ever breaks.
    struct Fake {
        fsp: Cell<bool>,
        base: Cell<bool>,
        /// Read-only, as last mounted.
        read_only: Cell<bool>,
        gadget: Cell<Gadget>,
        lun: RefCell<String>,
        /// The computer locked the medium: the LUN won't empty.
        locked: Cell<bool>,
        exported: Cell<bool>,
        plugged: Cell<bool>,
        holders: RefCell<Vec<Holder>>,
        /// Holders that let go after this many looks.
        holders_for: Cell<u32>,
        fsck: Cell<i32>,
        fail: RefCell<Vec<&'static str>>,
        /// The cable goes as this operation runs.
        yank_at: Cell<Option<&'static str>>,
        log: RefCell<Vec<String>>,
    }

    impl Default for Fake {
        /// Mounted, `g_file_storage` loaded with an empty LUN, unplugged.
        fn default() -> Self {
            Self {
                fsp: Cell::new(true),
                base: Cell::new(true),
                read_only: Cell::new(false),
                gadget: Cell::new(Gadget::Storage),
                lun: RefCell::new(String::new()),
                locked: Cell::new(false),
                exported: Cell::new(false),
                plugged: Cell::new(false),
                holders: RefCell::new(Vec::new()),
                holders_for: Cell::new(u32::MAX),
                fsck: Cell::new(0),
                fail: RefCell::new(Vec::new()),
                yank_at: Cell::new(None),
                log: RefCell::new(Vec::new()),
            }
        }
    }

    impl Fake {
        fn note(&self, op: &'static str, detail: &str) -> std::io::Result<()> {
            self.log.borrow_mut().push(if detail.is_empty() {
                op.to_owned()
            } else {
                format!("{op} {detail}")
            });
            if self.yank_at.get() == Some(op) {
                self.plugged.set(false);
            }
            if self.fail.borrow().contains(&op) {
                return Err(std::io::Error::other(format!("{op} failed")));
            }
            Ok(())
        }

        fn log(&self) -> Vec<String> {
            self.log.borrow().clone()
        }

        fn did(&self, op: &str) -> bool {
            self.log.borrow().iter().any(|l| l == op)
        }

        fn failing(self, op: &'static str) -> Self {
            self.fail.borrow_mut().push(op);
            self
        }

        fn holding(self, pid: u32, name: &str, mount: Mount) -> Self {
            self.holders.borrow_mut().push(Holder {
                pid,
                name: Name::new(name),
                path: format!("{}/books/a.epub", mount.path()),
            });
            self
        }

        fn computer(self) -> Self {
            self.plugged.set(true);
            self
        }

        fn at_home(&self) -> bool {
            self.fsp.get() && self.base.get() && self.lun.borrow().is_empty()
        }

        fn mount_cell(&self, mount: Mount) -> &Cell<bool> {
            match mount {
                Mount::Fsp => &self.fsp,
                Mount::Base => &self.base,
            }
        }
    }

    impl Ops for Fake {
        fn sync(&self) {
            let _ = self.note("sync", "");
        }
        fn holders(&self, mount: Mount) -> Vec<Holder> {
            let left = self.holders_for.get();
            if mount == Mount::Base {
                self.holders_for.set(left.saturating_sub(1));
            }
            if left == 0 {
                self.holders.borrow_mut().clear();
            }
            self.holders
                .borrow()
                .iter()
                .filter(|h| h.path.starts_with(mount.path()))
                .cloned()
                .collect()
        }
        fn mounted(&self, mount: Mount) -> bool {
            self.mount_cell(mount).get()
        }
        fn umount(&self, mount: Mount) -> std::io::Result<()> {
            self.note("umount", mount.path())?;
            if mount == Mount::Base {
                assert!(!self.fsp.get(), "base-us unmounted under fsp");
            }
            self.mount_cell(mount).set(false);
            Ok(())
        }
        fn mount(&self, mount: Mount, read_only: bool) -> std::io::Result<()> {
            let detail = format!("{}{}", mount.path(), if read_only { " ro" } else { "" });
            self.note("mount", &detail)?;
            assert!(
                self.lun.borrow().is_empty(),
                "INVARIANT: mounting while the LUN holds the storage"
            );
            if mount == Mount::Fsp {
                assert!(self.base.get(), "fsp mounted over nothing");
            } else {
                self.read_only.set(read_only);
            }
            self.mount_cell(mount).set(true);
            Ok(())
        }
        fn gadget(&self) -> Gadget {
            self.gadget.get()
        }
        fn load(&self, gadget: Gadget) -> std::io::Result<()> {
            self.note("load", gadget.module().unwrap_or("none"))?;
            assert_eq!(self.gadget.get(), Gadget::None, "two gadgets at once");
            self.gadget.set(gadget);
            Ok(())
        }
        fn unload(&self, gadget: Gadget) -> std::io::Result<()> {
            self.note("unload", gadget.module().unwrap_or("none"))?;
            if self.gadget.get() == gadget {
                self.gadget.set(Gadget::None);
                self.lun.borrow_mut().clear();
                self.locked.set(false);
            }
            Ok(())
        }
        fn lun(&self) -> std::io::Result<Option<String>> {
            Ok((self.gadget.get() == Gadget::Storage).then(|| self.lun.borrow().clone()))
        }
        fn set_lun(&self, file: &str) -> std::io::Result<()> {
            self.note("lun", file)?;
            assert_eq!(self.gadget.get(), Gadget::Storage, "no LUN to write");
            if !file.is_empty() {
                assert!(
                    !self.base.get() && !self.fsp.get(),
                    "INVARIANT: exporting a mounted storage"
                );
                assert!(self.exported.get(), "exported before the flag");
            } else if self.locked.get() {
                return Err(std::io::Error::from_raw_os_error(16));
            }
            *self.lun.borrow_mut() = file.to_owned();
            Ok(())
        }
        fn flush(&self) {
            let _ = self.note("flush", "");
        }
        fn fsck(&self) -> std::io::Result<i32> {
            self.note("fsck", "")?;
            assert!(!self.base.get(), "fsck on a mounted storage");
            assert!(self.lun.borrow().is_empty(), "fsck while exported");
            Ok(self.fsck.get())
        }
        fn exported(&self) -> bool {
            self.exported.get()
        }
        fn set_exported(&self, on: bool) -> std::io::Result<()> {
            self.note("flag", if on { "on" } else { "off" })?;
            self.exported.set(on);
            Ok(())
        }
        fn plugged(&self) -> bool {
            self.plugged.get()
        }
        fn wait(&self, _: Duration) {}
    }

    #[derive(Default)]
    struct Screen {
        events: RefCell<Vec<Event>>,
        releases: Cell<u32>,
    }

    impl Ui for Screen {
        fn release(&self) {
            self.releases.set(self.releases.get() + 1);
            self.events.borrow_mut().push(Event::StorageLeaving);
        }
        fn deliver(&self, event: Event) {
            self.events.borrow_mut().push(event);
        }
    }

    impl Screen {
        fn events(&self) -> Vec<Event> {
            self.events.borrow().clone()
        }
    }

    const EXPORT: [&str; 5] = [
        "sync",
        "umount /mnt/us",
        "umount /mnt/base-us",
        "flag on",
        "lun /dev/mmcblk0p4",
    ];

    /// A computer plugged in and configured, held past the settle time.
    fn plug_in(driver: &mut Driver, fake: &Fake, ui: &Screen, t0: Instant) {
        fake.plugged.set(true);
        driver.signal(Signal::Cable, t0, ui);
        driver.signal(Signal::Online, t0, ui);
        driver.poll(t0 + SETTLE, ui);
    }

    #[test]
    fn a_computer_gets_the_storage_and_gives_it_back_checked() {
        let fake = Fake::default();
        let ui = Screen::default();
        let t0 = Instant::now();
        let mut driver = Driver::start(&fake, &ui);
        plug_in(&mut driver, &fake, &ui, t0);
        assert_eq!(
            driver.state(),
            State::Computer {
                previous: Gadget::Storage
            }
        );
        assert_eq!(fake.log()[..5], EXPORT);
        assert!(!fake.fsp.get() && !fake.base.get());
        fake.plugged.set(false);
        driver.signal(Signal::Cable, t0 + SETTLE * 10, &ui);
        assert_eq!(driver.state(), State::Kindle { plugged: false });
        assert_eq!(
            fake.log()[5..],
            [
                "lun",
                "flush",
                "fsck",
                "mount /mnt/base-us",
                "mount /mnt/us",
                "flag off"
            ]
        );
        assert!(fake.at_home() && !fake.exported.get() && !fake.read_only.get());
        assert_eq!(
            ui.events(),
            [
                Event::StorageLeaving,
                Event::Drive(Drive::Computer),
                Event::Drive(Drive::Kindle)
            ]
        );
    }

    #[test]
    fn usbnetworks_gadget_makes_way_and_comes_back() {
        let fake = Fake::default();
        fake.gadget.set(Gadget::Ether);
        let ui = Screen::default();
        let t0 = Instant::now();
        let mut driver = Driver::start(&fake, &ui);
        assert!(!fake.did("load g_file_storage"), "g_ether stays at start");
        plug_in(&mut driver, &fake, &ui, t0);
        let log = fake.log();
        let at = |op: &str| log.iter().position(|l| l == op).unwrap();
        assert!(at("umount /mnt/base-us") < at("unload g_ether"));
        assert!(at("unload g_ether") < at("load g_file_storage"));
        assert!(at("load g_file_storage") < at("lun /dev/mmcblk0p4"));
        fake.plugged.set(false);
        driver.signal(Signal::Cable, t0, &ui);
        assert_eq!(fake.gadget.get(), Gadget::Ether);
        assert!(fake.at_home());
    }

    #[test]
    fn nothing_loaded_loads_the_storage_gadget_to_notice_a_computer() {
        let fake = Fake::default();
        fake.gadget.set(Gadget::None);
        let _ = Driver::start(&fake, &Screen::default());
        assert_eq!(fake.gadget.get(), Gadget::Storage);
        assert!(fake.at_home());
    }

    #[test]
    fn another_program_holding_the_storage_refuses_and_nothing_changes() {
        let fake = Fake::default().holding(812, "cvm", Mount::Fsp);
        let ui = Screen::default();
        let t0 = Instant::now();
        let mut driver = Driver::start(&fake, &ui);
        plug_in(&mut driver, &fake, &ui, t0);
        assert!(fake.at_home());
        assert!(!fake.did("sync") && !fake.did("flag on"));
        assert_eq!(driver.state(), State::Held);
        assert_eq!(
            ui.events(),
            [
                Event::StorageLeaving,
                Event::DriveNotice(Notice::Refused(Refusal::Busy {
                    pid: 812,
                    name: Name::new("cvm")
                }))
            ]
        );
        // Not again while plugged in: it would only be refused again.
        driver.signal(Signal::Online, t0 + SETTLE * 3, &ui);
        driver.poll(t0 + SETTLE * 5, &ui);
        assert_eq!(ui.releases.get(), 1);
        // Plugged in again with the program gone: exported.
        fake.holders.borrow_mut().clear();
        fake.plugged.set(false);
        driver.signal(Signal::Cable, t0 + SETTLE * 6, &ui);
        plug_in(&mut driver, &fake, &ui, t0 + SETTLE * 7);
        assert!(matches!(driver.state(), State::Computer { .. }));
    }

    #[test]
    fn a_holder_that_lets_go_in_time_doesnt_refuse() {
        // Syncthing, stopped by the UI, closing its files.
        let fake = Fake::default().holding(900, "syncthing", Mount::Base);
        fake.holders_for.set(3);
        assert!(export(&fake.computer()).is_ok());
    }

    #[test]
    fn fsp_itself_doesnt_count_as_a_holder() {
        let fake = Fake::default().holding(937, "fsp", Mount::Base).computer();
        assert!(export(&fake).is_ok());
    }

    #[test]
    fn a_storage_that_wont_unmount_is_mounted_again_and_refused() {
        let fake = Fake::default().computer().failing("umount");
        assert_eq!(export(&fake), Err(Refusal::Unmount(Mount::Fsp)));
        assert!(fake.at_home());
        assert!(!fake.did("flag on"));
        // base-us refuses after fsp went: fsp comes back over it.
        let fake = Fake::default().computer();
        fake.fail.borrow_mut().push("umount");
        fake.fsp.set(false);
        assert_eq!(export(&fake), Err(Refusal::Unmount(Mount::Base)));
        assert!(fake.at_home());
    }

    #[test]
    fn a_gadget_that_wont_load_puts_everything_back() {
        let fake = Fake::default().computer().failing("load");
        fake.gadget.set(Gadget::Ether);
        assert_eq!(export(&fake), Err(Refusal::Gadget));
        assert!(fake.at_home());
        assert!(!fake.exported.get());
    }

    #[test]
    fn a_yank_mid_export_before_the_lun_mounts_it_again_unchecked() {
        let fake = Fake::default().computer();
        fake.yank_at.set(Some("umount"));
        let ui = Screen::default();
        let t0 = Instant::now();
        let mut driver = Driver::start(&fake, &ui);
        driver.signal(Signal::Online, t0, &ui);
        driver.poll(t0 + SETTLE, &ui);
        assert!(!fake.did("lun /dev/mmcblk0p4"));
        assert!(!fake.did("fsck"));
        assert!(fake.at_home());
        assert_eq!(driver.state(), State::Kindle { plugged: false });
        assert_eq!(
            ui.events().last(),
            Some(&Event::DriveNotice(Notice::Refused(Refusal::Unplugged)))
        );
    }

    #[test]
    fn a_yank_just_after_the_lun_is_written_comes_back_checked() {
        let fake = Fake::default().computer();
        fake.yank_at.set(Some("lun"));
        let ui = Screen::default();
        let t0 = Instant::now();
        let mut driver = Driver::start(&fake, &ui);
        plug_in(&mut driver, &fake, &ui, t0);
        fake.plugged.set(false);
        // The remove uevent queued behind the export.
        driver.signal(Signal::Cable, t0 + SETTLE, &ui);
        assert!(fake.did("fsck"));
        assert!(fake.at_home());
        assert_eq!(driver.state(), State::Kindle { plugged: false });
    }

    #[test]
    fn a_lost_remove_event_is_caught_by_the_poll() {
        let fake = Fake::default();
        let ui = Screen::default();
        let t0 = Instant::now();
        let mut driver = Driver::start(&fake, &ui);
        plug_in(&mut driver, &fake, &ui, t0);
        fake.plugged.set(false);
        driver.poll(t0 + SETTLE * 2, &ui);
        assert!(fake.at_home());
    }

    #[test]
    fn an_eject_from_the_computer_returns_and_waits_for_a_new_plug() {
        let fake = Fake::default();
        let ui = Screen::default();
        let t0 = Instant::now();
        let mut driver = Driver::start(&fake, &ui);
        plug_in(&mut driver, &fake, &ui, t0);
        // The gadget closes the backing file on eject.
        fake.lun.borrow_mut().clear();
        driver.signal(Signal::Gadget, t0 + SETTLE * 2, &ui);
        assert!(fake.at_home());
        assert!(fake.did("fsck"));
        assert_eq!(driver.state(), State::Held);
        // Still plugged: the computer configuring the gadget again changes
        // nothing.
        driver.signal(Signal::Offline, t0 + SETTLE * 3, &ui);
        driver.signal(Signal::Online, t0 + SETTLE * 3, &ui);
        driver.poll(t0 + SETTLE * 5, &ui);
        assert_eq!(driver.state(), State::Held);
        fake.plugged.set(false);
        driver.signal(Signal::Cable, t0 + SETTLE * 6, &ui);
        assert_eq!(driver.state(), State::Kindle { plugged: false });
    }

    #[test]
    fn a_locked_lun_is_released_by_unloading_the_gadget() {
        let fake = Fake::default().computer();
        export(&fake).unwrap();
        fake.locked.set(true);
        fake.plugged.set(false);
        assert_eq!(
            bring_back(&fake, Gadget::Storage),
            Ok(Back {
                check: Some(Check::Clean)
            })
        );
        assert!(fake.did("unload g_file_storage"));
        assert!(fake.fsp.get() && fake.base.get());
        assert_eq!(fake.gadget.get(), Gadget::Storage, "loaded again, empty");
    }

    #[test]
    fn a_lun_that_never_empties_is_never_mounted_over() {
        struct Stuck(Fake);
        impl Ops for Stuck {
            fn sync(&self) {}
            fn holders(&self, m: Mount) -> Vec<Holder> {
                self.0.holders(m)
            }
            fn mounted(&self, m: Mount) -> bool {
                self.0.mounted(m)
            }
            fn umount(&self, m: Mount) -> std::io::Result<()> {
                self.0.umount(m)
            }
            fn mount(&self, m: Mount, ro: bool) -> std::io::Result<()> {
                self.0.mount(m, ro)
            }
            fn gadget(&self) -> Gadget {
                Gadget::Storage
            }
            fn load(&self, _: Gadget) -> std::io::Result<()> {
                Ok(())
            }
            fn unload(&self, _: Gadget) -> std::io::Result<()> {
                Err(std::io::Error::other("in use"))
            }
            fn lun(&self) -> std::io::Result<Option<String>> {
                Ok(Some(DEVICE.into()))
            }
            fn set_lun(&self, _: &str) -> std::io::Result<()> {
                Err(std::io::Error::from_raw_os_error(16))
            }
            fn flush(&self) {}
            fn fsck(&self) -> std::io::Result<i32> {
                self.0.fsck()
            }
            fn exported(&self) -> bool {
                true
            }
            fn set_exported(&self, _: bool) -> std::io::Result<()> {
                Ok(())
            }
            fn plugged(&self) -> bool {
                false
            }
            fn wait(&self, _: Duration) {}
        }
        let fake = Fake::default();
        fake.fsp.set(false);
        fake.base.set(false);
        let stuck = Stuck(fake);
        assert_eq!(bring_back(&stuck, Gadget::Storage), Err(Lost));
        assert!(!stuck.0.did("fsck"));
        assert!(!stuck.0.base.get() && !stuck.0.fsp.get());
    }

    #[test]
    fn fsck_exit_codes_decide_how_the_storage_comes_back() {
        for (code, check, read_only, flag) in [
            (0, Check::Clean, false, false),
            (1, Check::Repaired, false, false),
            (2, Check::Failed(Some(2)), true, true),
            (4, Check::Failed(Some(4)), true, true),
        ] {
            let fake = Fake::default().computer();
            fake.fsck.set(code);
            export(&fake).unwrap();
            fake.plugged.set(false);
            assert_eq!(
                bring_back(&fake, Gadget::Storage),
                Ok(Back { check: Some(check) })
            );
            assert_eq!(fake.read_only.get(), read_only, "exit {code}");
            assert_eq!(
                fake.exported.get(),
                flag,
                "exit {code}: checked again next start"
            );
            assert!(fake.fsp.get() && fake.base.get());
        }
        // fsck that doesn't run at all: read-only too.
        let fake = Fake::default().computer();
        export(&fake).unwrap();
        fake.fail.borrow_mut().push("fsck");
        assert_eq!(
            bring_back(&fake, Gadget::Storage),
            Ok(Back {
                check: Some(Check::Failed(None))
            })
        );
        assert!(fake.read_only.get());
    }

    #[test]
    fn the_ui_hears_of_a_repair_or_a_read_only_storage() {
        for (code, notice) in [
            (1, Notice::Repaired),
            (4, Notice::ReadOnly(Check::Failed(Some(4)))),
        ] {
            let fake = Fake::default();
            fake.fsck.set(code);
            let ui = Screen::default();
            let t0 = Instant::now();
            let mut driver = Driver::start(&fake, &ui);
            plug_in(&mut driver, &fake, &ui, t0);
            fake.plugged.set(false);
            driver.signal(Signal::Cable, t0, &ui);
            assert_eq!(
                ui.events()[2..],
                [Event::Drive(Drive::Kindle), Event::DriveNotice(notice)]
            );
        }
    }

    #[test]
    fn a_flapping_cable_exports_only_once_the_computer_stays() {
        let fake = Fake::default();
        let ui = Screen::default();
        let t0 = Instant::now();
        let at = |ms| t0 + Duration::from_millis(ms);
        let mut driver = Driver::start(&fake, &ui);
        fake.plugged.set(true);
        driver.signal(Signal::Cable, at(0), &ui);
        driver.signal(Signal::Online, at(100), &ui);
        fake.plugged.set(false);
        driver.signal(Signal::Cable, at(1_000), &ui);
        fake.plugged.set(true);
        driver.signal(Signal::Cable, at(1_500), &ui);
        driver.signal(Signal::Online, at(1_600), &ui);
        driver.signal(Signal::Offline, at(2_000), &ui);
        driver.signal(Signal::Online, at(2_500), &ui);
        // The first timer would have fired here; the storage stays.
        driver.poll(at(3_200), &ui);
        driver.poll(at(4_700), &ui);
        assert!(fake.at_home());
        assert!(fake.log().is_empty(), "{:?}", fake.log());
        driver.poll(at(5_500), &ui);
        assert!(matches!(driver.state(), State::Computer { .. }));
        assert_eq!(ui.releases.get(), 1);
    }

    #[test]
    fn the_settle_timer_from_a_cancelled_plug_is_stale() {
        let t0 = Instant::now();
        let (state, _) = State::Kindle { plugged: true }
            .step(Input::Online, t0)
            .unwrap();
        let (state, _) = state.step(Input::Offline, t0).unwrap();
        let later = t0 + Duration::from_millis(500);
        let (state, _) = state.step(Input::Online, later).unwrap();
        assert_eq!(state.step(Input::Settled(t0), t0 + SETTLE), None);
        assert_eq!(
            state.step(Input::Settled(later), later + SETTLE),
            Some((State::Exporting, Some(Command::Export)))
        );
    }

    #[test]
    fn a_wall_charger_never_exports() {
        let fake = Fake::default();
        let ui = Screen::default();
        let t0 = Instant::now();
        let mut driver = Driver::start(&fake, &ui);
        fake.plugged.set(true);
        driver.signal(Signal::Cable, t0, &ui);
        driver.poll(t0 + SETTLE * 10, &ui);
        assert_eq!(driver.state(), State::Kindle { plugged: true });
        assert!(fake.log().is_empty());
        assert!(ui.events().is_empty());
    }

    #[test]
    fn the_gadget_swaps_own_echoes_change_nothing_while_exported() {
        let fake = Fake::default();
        fake.gadget.set(Gadget::Ether);
        let ui = Screen::default();
        let t0 = Instant::now();
        let mut driver = Driver::start(&fake, &ui);
        plug_in(&mut driver, &fake, &ui, t0);
        let before = fake.log().len();
        for signal in [
            Signal::Offline,
            Signal::Cable,
            Signal::Online,
            Signal::Gadget,
        ] {
            driver.signal(signal, t0 + SETTLE * 2, &ui);
        }
        assert_eq!(fake.log().len(), before);
        assert!(matches!(driver.state(), State::Computer { .. }));
    }

    #[test]
    fn a_start_after_a_yank_and_reboot_checks_the_storage_first() {
        // The boot mounted the storage the computer had.
        let fake = Fake::default();
        fake.exported.set(true);
        let ui = Screen::default();
        let driver = Driver::start(&fake, &ui);
        assert_eq!(
            fake.log(),
            [
                "sync",
                "umount /mnt/us",
                "umount /mnt/base-us",
                "flush",
                "fsck",
                "mount /mnt/base-us",
                "mount /mnt/us",
                "flag off"
            ]
        );
        assert_eq!(driver.state(), State::Kindle { plugged: false });
        assert_eq!(
            ui.events(),
            [
                Event::StorageLeaving,
                Event::DriveNotice(Notice::Checked(Check::Clean))
            ]
        );
    }

    #[test]
    fn that_check_waits_for_a_program_holding_the_storage() {
        let fake = Fake::default().holding(700, "cvm", Mount::Fsp);
        fake.exported.set(true);
        let ui = Screen::default();
        let _ = Driver::start(&fake, &ui);
        assert!(fake.at_home() && fake.exported.get());
        assert!(!fake.did("fsck"));
        assert!(matches!(
            ui.events()[1],
            Event::DriveNotice(Notice::Unchecked(Refusal::Busy { pid: 700, .. }))
        ));
    }

    #[test]
    fn a_start_while_still_exported_picks_up_with_the_computer() {
        let fake = Fake::default().computer();
        export(&fake).unwrap();
        fake.log.borrow_mut().clear();
        let ui = Screen::default();
        let mut driver = Driver::start(&fake, &ui);
        assert!(fake.log().is_empty(), "nothing touched: {:?}", fake.log());
        assert_eq!(ui.events(), [Event::Drive(Drive::Computer)]);
        fake.plugged.set(false);
        driver.signal(Signal::Cable, Instant::now(), &ui);
        assert!(fake.at_home() && fake.did("fsck"));
    }

    #[test]
    fn a_start_after_a_crash_mid_export_mounts_again() {
        let fake = Fake::default();
        fake.fsp.set(false);
        fake.base.set(false);
        let ui = Screen::default();
        let _ = Driver::start(&fake, &ui);
        assert!(fake.at_home());
        assert!(!fake.did("fsck"));
    }

    #[test]
    fn the_switch_is_off_unless_it_says_kindil() {
        assert_eq!(Owner::parse("kindil\n"), Owner::Kindil);
        assert_eq!(Owner::parse("amazon"), Owner::Amazon);
        assert_eq!(Owner::parse(""), Owner::Amazon);
        assert_eq!(
            Owner::read(Path::new("/nonexistent/usb-drive")),
            Owner::Amazon
        );
        let dir = tempfile::tempdir().unwrap();
        let switch = dir.path().join("usb-drive");
        std::fs::write(&switch, "kindil\n").unwrap();
        assert_eq!(Owner::read(&switch), Owner::Kindil);
    }

    #[test]
    fn uevents_of_the_cable_and_the_gadget() {
        let event = |text: &[u8]| Uevent::parse(text).unwrap();
        assert_eq!(
            signal(&event(
                b"online@/devices/platform/fsl-usb2-udc\0ACTION=online\0"
            )),
            Some(Signal::Online)
        );
        assert_eq!(
            signal(&event(b"offline@/devices/platform/fsl-usb2-udc\0")),
            Some(Signal::Offline)
        );
        assert_eq!(
            signal(&event(b"add@/devices/platform/aplite_charger.0\0")),
            Some(Signal::Cable)
        );
        assert_eq!(
            signal(&event(b"remove@/devices/platform/aplite_charger.0\0")),
            Some(Signal::Cable)
        );
        assert_eq!(
            signal(&event(
                b"change@/devices/platform/fsl-usb2-udc/gadget/g_file_storage\0"
            )),
            Some(Signal::Gadget)
        );
        let button = event(b"online@/devices/virtual/misc/yoshibutton\0");
        assert_eq!(signal(&button), None);
        assert!(!is_usb(&button));
        assert!(is_usb(&event(b"add@/devices/platform/fsl-usb2-udc\0")));
    }

    #[test]
    fn process_names_keep_to_the_kernels_length() {
        assert_eq!(Name::new("syncthing").as_str(), "syncthing");
        assert_eq!(
            Name::new("a-very-long-process-name").as_str(),
            "a-very-long-pro"
        );
        assert_eq!(Name::new("ééééééééé").as_str(), "ééééééé");
    }

    #[test]
    fn notices_that_bring_the_library_back() {
        assert!(Notice::Refused(Refusal::Gadget).storage_back());
        assert!(Notice::Checked(Check::Clean).storage_back());
        assert!(!Notice::Repaired.storage_back());
        assert!(!Notice::Lost.storage_back());
    }

    #[test]
    fn exporting_never_writes_the_lun_over_a_mounted_storage() {
        // Something mounts base-us again between the unmount and the LUN.
        struct Remounting(Fake);
        impl Ops for Remounting {
            fn sync(&self) {}
            fn holders(&self, m: Mount) -> Vec<Holder> {
                self.0.holders(m)
            }
            fn mounted(&self, m: Mount) -> bool {
                // Seen unmounted by the check after unmounting, then back.
                let unmounts = self
                    .0
                    .log()
                    .iter()
                    .filter(|l| l.starts_with("umount"))
                    .count();
                let gadget = self.0.log().iter().any(|l| l.starts_with("load"));
                if unmounts == 2 && gadget && m == Mount::Base {
                    self.0.base.set(true);
                }
                self.0.mounted(m)
            }
            fn umount(&self, m: Mount) -> std::io::Result<()> {
                self.0.umount(m)
            }
            fn mount(&self, m: Mount, ro: bool) -> std::io::Result<()> {
                self.0.mount(m, ro)
            }
            fn gadget(&self) -> Gadget {
                self.0.gadget()
            }
            fn load(&self, g: Gadget) -> std::io::Result<()> {
                self.0.load(g)
            }
            fn unload(&self, g: Gadget) -> std::io::Result<()> {
                self.0.unload(g)
            }
            fn lun(&self) -> std::io::Result<Option<String>> {
                self.0.lun()
            }
            fn set_lun(&self, f: &str) -> std::io::Result<()> {
                self.0.set_lun(f)
            }
            fn flush(&self) {}
            fn fsck(&self) -> std::io::Result<i32> {
                self.0.fsck()
            }
            fn exported(&self) -> bool {
                self.0.exported()
            }
            fn set_exported(&self, on: bool) -> std::io::Result<()> {
                self.0.set_exported(on)
            }
            fn plugged(&self) -> bool {
                true
            }
            fn wait(&self, _: Duration) {}
        }
        let fake = Fake::default();
        fake.gadget.set(Gadget::Ether);
        let ops = Remounting(fake);
        assert_eq!(export(&ops), Err(Refusal::Unmount(Mount::Base)));
        assert!(!ops.0.did("lun /dev/mmcblk0p4"));
        assert!(ops.0.at_home());
    }
}
