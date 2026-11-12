//! Raw system-call bindings.
//!
//! The Kindle Paperwhite 1 runs a 2.6.31-era kernel with a uClibc userland, so
//! we avoid the `libc` crate (which would have to be vendored) and declare the
//! handful of functions we need directly. They are the C library's symbols, not
//! raw `syscall(2)` numbers, so the C library's own ABI decides the layouts -
//! see the type notes on each item.
//!
//! # The rule that matters
//!
//! `long`, `unsigned long`, `size_t`, `time_t` and `nfds_t` are **4 bytes on the
//! ARM32 target and 8 bytes on the x86_64 host**. Every declaration here uses
//! `core::ffi::c_long` / `c_ulong` / `usize` for those, never `i64`/`u64`.
//!
//! Getting this wrong does not fail to compile: the host build is happy, and on
//! the device the arguments silently land in the wrong registers. `poll` with a
//! `u64` count, for instance, makes the timeout read as zero and turns the main
//! loop into a busy loop.

use core::ffi::{c_char, c_int, c_long, c_ulong, c_void};

extern "C" {
    pub fn open(path: *const c_char, flags: c_int, ...) -> c_int;
    pub fn close(fd: c_int) -> c_int;
    pub fn read(fd: c_int, buf: *mut c_void, count: usize) -> isize;
    pub fn write(fd: c_int, buf: *const c_void, count: usize) -> isize;
    pub fn ioctl(fd: c_int, request: c_ulong, ...) -> c_int;
    pub fn mmap(
        addr: *mut c_void,
        length: usize,
        prot: c_int,
        flags: c_int,
        fd: c_int,
        offset: i64,
    ) -> *mut c_void;
    pub fn munmap(addr: *mut c_void, length: usize) -> c_int;
    /// `nfds_t` is `unsigned long`: 4 bytes on the device, 8 on the host.
    pub fn poll(fds: *mut PollFd, nfds: c_ulong, timeout: c_int) -> c_int;
    pub fn clock_gettime(clk_id: c_int, tp: *mut Timespec) -> c_int;
    pub fn getpid() -> c_int;
    pub fn __errno_location() -> *mut c_int;
    pub fn mount(
        source: *const c_char,
        target: *const c_char,
        fstype: *const c_char,
        flags: c_ulong,
        data: *const c_void,
    ) -> c_int;
    /// `dev_t` is 64-bit on every Linux libc, `mode_t` is 32-bit.
    pub fn mknod(path: *const c_char, mode: u32, dev: u64) -> c_int;
}

/// `CLOCK_REALTIME`
pub const CLOCK_REALTIME: c_int = 0;

/// `struct timespec`.
///
/// `tv_sec` is `time_t` and `tv_nsec` is `long`; both are 4 bytes on the 32-bit
/// ARM target and 8 on the 64-bit host, so they are spelled `c_long` rather
/// than `i64`. With `i64` the host build passes, and on the device the kernel
/// writes 4 bytes into an 8-byte field, so `tv_sec` reads the nanoseconds as its
/// high half.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct Timespec {
    pub tv_sec: c_long,
    pub tv_nsec: c_long,
}

/// `open(2)` flags.
pub const O_RDONLY: c_int = 0;
pub const O_RDWR: c_int = 2;
pub const O_CLOEXEC: c_int = 0o2000000;

/// Error numbers we compare against.
pub const ENOENT: c_int = 2;
pub const EINVAL: c_int = 22;
pub const EBUSY: c_int = 16;
pub const EEXIST: c_int = 17;

/// `mmap(2)` protection flags.
pub const PROT_READ: c_int = 1;
pub const PROT_WRITE: c_int = 2;

/// `mmap(2)` mapping flags.
pub const MAP_SHARED: c_int = 1;

/// `mount(2)` flags.
pub const MS_NOSUID: c_ulong = 2;
pub const MS_NOEXEC: c_ulong = 8;

/// `poll(2)` events.
pub const POLLIN: i16 = 0x001;

/// A single `poll(2)` file descriptor entry.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct PollFd {
    pub fd: c_int,
    pub events: i16,
    pub revents: i16,
}

/// The current value of `errno`.
pub fn errno() -> c_int {
    // SAFETY: `__errno_location` always returns a valid, thread-local pointer.
    unsafe { *__errno_location() }
}

/// `strerror(3)` for the error codes we realistically hit.
///
/// The Kindle's C library is uClibc, whose `strerror` returns short static
/// strings; we keep our own table so error output does not depend on it.
pub fn strerror(code: c_int) -> &'static str {
    match code {
        1 => "EPERM",
        2 => "ENOENT",
        4 => "EINTR",
        5 => "EIO",
        6 => "ENXIO",
        9 => "EBADF",
        11 => "EAGAIN",
        12 => "ENOMEM",
        13 => "EACCES",
        14 => "EFAULT",
        16 => "EBUSY",
        17 => "EEXIST",
        19 => "ENODEV",
        22 => "EINVAL",
        25 => "ENOTTY",
        28 => "ENOSPC",
        30 => "EROFS",
        38 => "ENOSYS",
        110 => "ETIMEDOUT",
        _ => "unknown error",
    }
}

/// `ioctl(2)` request codes, computed from the kernel's `_IOC` encoding.
///
/// The kernel encodes an ioctl number as
/// `dir << 30 | size << 16 | type << 8 | nr`, where `dir` is 1 for write-only
/// (`_IOW`), 2 for read-only (`_IOR`) and 3 for read/write (`_IOWR`), and `size`
/// is the size of the argument struct.
///
/// The size is part of the number, so the argument structs must match the
/// kernel's 32-bit ARM layout. The numbers themselves are the same on every
/// architecture and on both pointer widths.
pub mod ioc {
    use core::ffi::{c_uint, c_ulong};

    const IOC_NONE: c_uint = 0;
    const IOC_WRITE: c_uint = 1;
    const IOC_READ: c_uint = 2;
    const IOC_NRBITS: u32 = 8;
    const IOC_TYPEBITS: u32 = 8;
    const IOC_SIZEBITS: u32 = 14;
    const IOC_NRSHIFT: u32 = 0;
    const IOC_TYPESHIFT: u32 = IOC_NRSHIFT + IOC_NRBITS;
    const IOC_SIZESHIFT: u32 = IOC_TYPESHIFT + IOC_TYPEBITS;
    const IOC_DIRSHIFT: u32 = IOC_SIZESHIFT + IOC_SIZEBITS;

    const fn encode(dir: c_uint, ty: c_uint, nr: c_uint, size: u32) -> c_ulong {
        ((dir << IOC_DIRSHIFT)
            | ((size & ((1 << IOC_SIZEBITS) - 1)) << IOC_SIZESHIFT)
            | (ty << IOC_TYPESHIFT)
            | (nr << IOC_NRSHIFT)) as c_ulong
    }

    /// `_IO(type, nr)`: no argument.
    pub const fn io(ty: c_uint, nr: c_uint) -> c_ulong {
        encode(IOC_NONE, ty, nr, 0)
    }

    /// `_IOR(type, nr, size)`: read-only argument.
    pub const fn ior(ty: c_uint, nr: c_uint, size: u32) -> c_ulong {
        encode(IOC_READ, ty, nr, size)
    }

    /// `_IOW(type, nr, size)`: write-only argument.
    pub const fn iow(ty: c_uint, nr: c_uint, size: u32) -> c_ulong {
        encode(IOC_WRITE, ty, nr, size)
    }

    /// `_IOWR(type, nr, size)`: read/write argument.
    pub const fn iowr(ty: c_uint, nr: c_uint, size: u32) -> c_ulong {
        encode(IOC_READ | IOC_WRITE, ty, nr, size)
    }
}

/// `c_uint` value for the `'F'` ioctl type used by the EPDC driver.
pub const IOC_TYPE_F: core::ffi::c_uint = b'F' as core::ffi::c_uint;

/// Read a file into `buf`, returning the number of bytes read (0 on error).
pub fn read_file(path: &str, buf: &mut [u8]) -> usize {
    let c_path = match to_cstring(path) {
        Some(p) => p,
        None => return 0,
    };
    // SAFETY: `c_path` is a NUL-terminated C string that outlives the call.
    let fd = unsafe { open(c_path.as_ptr() as *const c_char, O_RDONLY | O_CLOEXEC) };
    if fd < 0 {
        return 0;
    }
    // SAFETY: `buf` is a live buffer of the length we pass.
    let n = unsafe { read(fd, buf.as_mut_ptr() as *mut c_void, buf.len()) };
    // SAFETY: `fd` was opened above and is closed exactly once.
    unsafe {
        close(fd);
    }
    if n < 0 {
        0
    } else {
        n as usize
    }
}

/// Encode a path as a NUL-terminated C string.
///
/// Returns `None` if the path contains an interior NUL, which cannot be
/// represented and would silently truncate the path.
pub fn to_cstring(path: &str) -> Option<Vec<u8>> {
    if path.as_bytes().contains(&0) {
        return None;
    }
    let mut bytes = Vec::with_capacity(path.len() + 1);
    bytes.extend_from_slice(path.as_bytes());
    bytes.push(0);
    Some(bytes)
}

/// The type of an ioctl request number.
///
/// On the 32-bit ARM target `unsigned long` is 4 bytes, so an ioctl request is
/// passed in a 32-bit register. The request *numbers* are the same on every
/// architecture, but the type they are passed in is not.
pub type IoctlRequest = c_ulong;

#[cfg(test)]
mod tests {
    use super::*;
    use core::mem::size_of;

    #[test]
    fn timespec_matches_the_c_long_abi() {
        // 8 bytes on ARM32, 16 on x86_64 - but always two `long`s.
        assert_eq!(size_of::<Timespec>(), 2 * size_of::<c_long>());
    }

    #[test]
    fn ioctl_request_is_pointer_sized() {
        // The request goes into a register, so it must be `unsigned long`.
        assert_eq!(size_of::<IoctlRequest>(), size_of::<usize>());
    }

    #[test]
    fn ioc_encoding_matches_the_kernel() {
        // `_IOW('F', 0x2E, struct mxcfb_update_data)` with a 72-byte struct.
        assert_eq!(ioc::iow(IOC_TYPE_F, 0x2E, 72), 0x4048_462e);
        // `_IOR('E', 0x02, struct input_id)` with an 8-byte struct.
        assert_eq!(ioc::ior(b'E' as core::ffi::c_uint, 0x02, 8), 0x8008_4502);
    }

    #[test]
    fn cstring_rejects_interior_nul() {
        assert!(to_cstring("/dev/fb0").is_some());
        assert!(to_cstring("/dev/fb\0").is_none());
        assert_eq!(to_cstring("/a").unwrap(), b"/a\0");
    }
}
