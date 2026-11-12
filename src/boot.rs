//! Early boot setup for `kindil --init`.
//!
//! When kindil is PID 1 in an initramfs there is no udev to create device
//! nodes, and `/dev` may not even be a devtmpfs mount. Without this the shell
//! starts, cannot open `/dev/fb0`, and exits - which panics the kernel.
//!
//! So before opening anything: mount devtmpfs on `/dev`, and if that is not
//! available (the lab126 kernel may not have it enabled), create the handful of
//! nodes the shell needs by hand.

use crate::sys;
use core::ffi::c_char;

/// Character device bit for `mknod(2)`.
const S_IFCHR: u32 = 0o020000;

/// `/dev` nodes the shell uses, with their major/minor numbers.
///
/// `fb0` is the EPDC framebuffer (29,0), `event0..event3` are the input
/// devices (13,64..), and `yoshime_fl` is the frontlight misc device (10,162).
const NODES: &[(&str, u32, u32)] = &[
    ("/dev/console", 5, 1),
    ("/dev/null", 1, 3),
    ("/dev/fb0", 29, 0),
    ("/dev/input/event0", 13, 64),
    ("/dev/input/event1", 13, 65),
    ("/dev/input/event2", 13, 66),
    ("/dev/input/event3", 13, 67),
    ("/dev/yoshime_fl", 10, 162),
];

/// Prepare `/dev` for the rest of the program.
pub fn prepare() {
    mount_devtmpfs();
    for &(path, major, minor) in NODES {
        create_node(path, major, minor);
    }
}

fn mount_devtmpfs() {
    let source = sys::to_cstring("devtmpfs").unwrap();
    let target = sys::to_cstring("/dev").unwrap();
    let fstype = sys::to_cstring("devtmpfs").unwrap();
    // SAFETY: all three strings are NUL-terminated and outlive the call. A
    // failure is expected when devtmpfs is already mounted or unsupported, so
    // the result is deliberately ignored.
    let rc = unsafe {
        sys::mount(
            source.as_ptr() as *const c_char,
            target.as_ptr() as *const c_char,
            fstype.as_ptr() as *const c_char,
            0,
            core::ptr::null(),
        )
    };
    if rc < 0 {
        let errno = sys::errno();
        // EBUSY means it is already mounted, which is fine.
        if errno != sys::EBUSY {
            eprintln!(
                "kindil: devtmpfs not mounted ({}); creating nodes by hand",
                sys::strerror(errno)
            );
        }
    }
}

fn create_node(path: &str, major: u32, minor: u32) {
    let Some(c_path) = sys::to_cstring(path) else {
        return;
    };
    // SAFETY: `c_path` is a NUL-terminated string that outlives the call.
    let rc = unsafe {
        sys::mknod(
            c_path.as_ptr() as *const c_char,
            S_IFCHR | 0o600,
            make_dev(major, minor),
        )
    };
    if rc < 0 {
        let errno = sys::errno();
        // EEXIST means devtmpfs already made it, which is the normal case.
        if errno != sys::EEXIST {
            eprintln!(
                "kindil: mknod({path}) failed: {} ({errno})",
                sys::strerror(errno)
            );
        }
    }
}

/// Linux's `dev_t` encoding for a major/minor pair.
pub const fn make_dev(major: u32, minor: u32) -> u64 {
    (((major & 0xfff) << 8) | (minor & 0xff) | ((minor & !0xff) << 12)) as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_numbers_match_the_kernel_encoding() {
        assert_eq!(make_dev(29, 0), 0x1d00);
        assert_eq!(make_dev(13, 64), 0x0d40);
        assert_eq!(make_dev(10, 162), 0x0aa2);
        assert_eq!(make_dev(5, 1), 0x0501);
    }
}
