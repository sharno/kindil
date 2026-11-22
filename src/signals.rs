//! SIGTERM and SIGINT end the session cleanly: the event loop quits, so
//! Syncthing is stopped and the reading position saved. SIGUSR1 does the
//! same, then asks the launcher to run kindil again ([`restart_asked`]):
//! an install (src/install.rs) restarts kindil into a new binary without
//! the launcher bringing Amazon's UI back and tearing it down again.
//!
//! The KUAL launcher ignores TERM while Amazon's UI shuts down, and ignored
//! signals are inherited, so without a handler `killall kindil` does nothing.
//! A handler (unlike blocking the signal) is reset on exec, so children such
//! as Dropbear and Syncthing still get default signal handling.
use std::io;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};

static WAKE: AtomicI32 = AtomicI32::new(-1);
/// SIGUSR1 came: the launcher is to run kindil again.
static RESTART: AtomicBool = AtomicBool::new(false);

/// Whether SIGUSR1 ended the session: exit with the launcher's restart
/// code.
pub fn restart_asked() -> bool {
    RESTART.load(Ordering::Relaxed)
}

extern "C" fn on_signal(signal: libc::c_int) {
    if signal == libc::SIGUSR1 {
        // An atomic store is async-signal-safe.
        RESTART.store(true, Ordering::Relaxed);
    }
    let fd = WAKE.load(Ordering::Relaxed);
    // SAFETY: write(2) is async-signal-safe; one byte to our own pipe.
    unsafe { libc::write(fd, [1u8].as_ptr().cast(), 1) };
}

fn check(rc: libc::c_int) -> io::Result<()> {
    match rc {
        -1 => Err(io::Error::last_os_error()),
        _ => Ok(()),
    }
}

pub fn quit_on_termination() -> io::Result<()> {
    let mut fds = [0; 2];
    // SAFETY: pipe(2) fills two descriptors; FD_CLOEXEC keeps them out of children.
    unsafe {
        check(libc::pipe(fds.as_mut_ptr()))?;
        check(libc::fcntl(fds[0], libc::F_SETFD, libc::FD_CLOEXEC))?;
        check(libc::fcntl(fds[1], libc::F_SETFD, libc::FD_CLOEXEC))?;
    }
    WAKE.store(fds[1], Ordering::Relaxed);
    [libc::SIGTERM, libc::SIGINT, libc::SIGUSR1]
        .into_iter()
        .try_for_each(|signal| {
            // SAFETY: a zeroed sigaction with only a handler and SA_RESTART set.
            unsafe {
                let mut action: libc::sigaction = std::mem::zeroed();
                action.sa_sigaction = on_signal as extern "C" fn(libc::c_int) as usize;
                action.sa_flags = libc::SA_RESTART;
                check(libc::sigemptyset(&mut action.sa_mask))?;
                check(libc::sigaction(signal, &action, std::ptr::null_mut()))
            }
        })?;
    let read = fds[0];
    std::thread::spawn(move || {
        let mut byte = [0u8; 1];
        loop {
            // SAFETY: reads into a one-byte buffer from our own pipe.
            match unsafe { libc::read(read, byte.as_mut_ptr().cast(), 1) } {
                1 => break,
                -1 if io::Error::last_os_error().kind() == io::ErrorKind::Interrupted => {}
                // The pipe broke: no signal can reach us any more.
                _ => return,
            }
        }
        let _ = slint::invoke_from_event_loop(|| {
            let _ = slint::quit_event_loop();
        });
    });
    Ok(())
}
