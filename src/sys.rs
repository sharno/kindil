//! Raw system-call bindings.
//!
//! The Kindle Paperwhite 1 runs a 2.6.31-era kernel with a uClibc userland, so
//! we avoid `libc` as a crate dependency and declare the handful of syscalls we
//! need directly. Everything here is `unsafe` by construction: the caller is
//! responsible for upholding the invariants.

use core::ffi::{c_int, c_uint, c_ulong, c_void};

extern "C" {
    /// `open(2)`. The path argument is `*const i8` to match the C library's
    /// `const char *`; we cast at the call site.
    pub fn open(path: *const core::ffi::c_char, flags: c_int, ...) -> c_int;
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
    pub fn poll(fds: *mut PollFd, nfds: u64, timeout: c_int) -> c_int;
    pub fn clock_gettime(clk_id: c_int, tp: *mut Timespec) -> c_int;
    pub fn __errno_location() -> *mut c_int;
}

/// `CLOCK_REALTIME`
pub const CLOCK_REALTIME: c_int = 0;

/// `struct timespec`.
///
/// `time_t` is 32-bit on the ARM32 target and 64-bit on the host, so we
/// spell the fields out explicitly rather than relying on `libc`'s layout.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct Timespec {
    pub tv_sec: i64,
    pub tv_nsec: i64,
}

/// `open(2)` flags.
pub const O_RDONLY: c_int = 0;
pub const O_RDWR: c_int = 2;
pub const O_CLOEXEC: c_int = 0o2000000;

/// `mmap(2)` protection flags.
pub const PROT_READ: c_int = 1;
pub const PROT_WRITE: c_int = 2;

/// `mmap(2)` mapping flags.
pub const MAP_SHARED: c_int = 1;

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

/// A human-readable description of the last error.
pub fn last_error() -> String {
    let e = errno();
    format!("{e} ({})", strerror(e))
}

/// `strerror(3)` for the error codes we realistically hit.
///
/// The Kindle's C library is uClibc, whose `strerror` returns short static
/// strings; we keep our own table so error output does not depend on it.
pub fn strerror(code: c_int) -> &'static str {
    match code {
        1 => "EPERM",
        2 => "ENOENT",
        5 => "EIO",
        9 => "EBADF",
        11 => "EAGAIN",
        12 => "ENOMEM",
        13 => "EACCES",
        14 => "EFAULT",
        16 => "EBUSY",
        19 => "ENODEV",
        22 => "EINVAL",
        25 => "ENOTTY",
        28 => "ENOSPC",
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
/// The argument size is part of the number, so these must be computed against
/// the *target's* struct layout, not the host's. We therefore always derive
/// them from [`crate::epdc`]'s `#[repr(C)]` structs, which mirror the 32-bit
/// ARM layout of the Kindle kernel exactly.
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
    let mut p = path.as_bytes().to_vec();
    p.push(0);
    // SAFETY: `p` is a NUL-terminated C string that outlives the call.
    let fd = unsafe { open(p.as_ptr() as *const core::ffi::c_char, O_RDONLY | O_CLOEXEC) };
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

/// `c_uint` is used for the `mode_t`/`unsigned int` arguments of `open`.
pub type CUInt = c_uint;

/// The type of an ioctl request number.
///
/// On the 32-bit ARM target `unsigned long` is 4 bytes, so an ioctl request
/// is passed in a 32-bit register. The request *numbers* are the same on
/// every architecture, but the type they are passed in is not - so we always
/// use `c_ulong` and never widen a request to `u64` by hand.
pub type IoctlRequest = c_ulong;
