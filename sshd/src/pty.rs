//! Pseudo-terminals for interactive sessions: one pair from openpty(3), the
//! shell made a session leader with the terminal as its controlling one, the
//! window's size, and non-blocking reads and writes of the master side.
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use tokio::io::unix::AsyncFd;

/// A terminal's size, in characters and pixels, as the client gives it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Size {
    pub cols: u32,
    pub rows: u32,
    pub width: u32,
    pub height: u32,
}

impl Size {
    fn winsize(self) -> libc::winsize {
        let clamp = |n: u32| n.min(u16::MAX.into()) as u16;
        libc::winsize {
            ws_col: clamp(self.cols),
            ws_row: clamp(self.rows),
            ws_xpixel: clamp(self.width),
            ws_ypixel: clamp(self.height),
        }
    }
}

fn check(result: libc::c_int) -> io::Result<libc::c_int> {
    if result < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(result)
    }
}

/// A new pair: the master side, ready for tokio, and the side the shell gets.
pub fn open(size: Size) -> io::Result<(Master, OwnedFd)> {
    let (mut master, mut slave) = (-1, -1);
    let mut winsize = size.winsize();
    // SAFETY: openpty fills in two descriptors it opened; no name is asked for.
    check(unsafe {
        libc::openpty(
            &mut master,
            &mut slave,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            // A raw pointer: glibc and musl take `*const winsize`, macOS
            // `*mut`; a `*mut` serves both, `&mut` trips clippy on Linux.
            &raw mut winsize,
        )
    })?;
    // SAFETY: both were just opened by openpty and are owned by nobody else.
    let (master, slave) = unsafe { (OwnedFd::from_raw_fd(master), OwnedFd::from_raw_fd(slave)) };
    for fd in [master.as_raw_fd(), slave.as_raw_fd()] {
        // SAFETY: plain fcntl on descriptors owned here.
        check(unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) })?;
    }
    let fd = master.as_raw_fd();
    // SAFETY: as above.
    let flags = check(unsafe { libc::fcntl(fd, libc::F_GETFL) })?;
    check(unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) })?;
    // SAFETY: the descriptor stays open and owned by the AsyncFd until it
    // is dropped with it.
    Ok((Master(unsafe { AsyncFd::register(master)? }), slave))
}

/// Gives `command` the terminal `slave` as its stdin, stdout and stderr and as
/// the controlling terminal of a new session it leads.
pub fn attach(command: &mut Command, slave: OwnedFd) -> io::Result<()> {
    command
        .stdin(Stdio::from(slave.try_clone()?))
        .stdout(Stdio::from(slave.try_clone()?))
        .stderr(Stdio::from(slave));
    // SAFETY: only async-signal-safe calls between fork and exec.
    unsafe {
        command.pre_exec(|| {
            check(libc::setsid())?;
            check(libc::ioctl(0, libc::TIOCSCTTY as _, 0))?;
            Ok(())
        });
    }
    Ok(())
}

/// The master side of a terminal.
pub struct Master(AsyncFd<OwnedFd>);

impl Master {
    fn fd(&self) -> RawFd {
        self.0.get_ref().as_raw_fd()
    }

    /// Tells the terminal (and so the shell, by SIGWINCH) its new size.
    pub fn resize(&self, size: Size) -> io::Result<()> {
        let winsize = size.winsize();
        // SAFETY: TIOCSWINSZ reads one winsize.
        check(unsafe { libc::ioctl(self.fd(), libc::TIOCSWINSZ as _, &winsize) }).map(drop)
    }

    /// What the shell wrote; 0 once every slave side is closed (Linux says
    /// EIO).
    pub async fn read(&self, buf: &mut [u8]) -> io::Result<usize> {
        loop {
            let mut ready = self.0.readable().await?;
            match ready.try_io(|fd| {
                // SAFETY: reads into `buf`, within its length.
                let n = unsafe {
                    libc::read(fd.get_ref().as_raw_fd(), buf.as_mut_ptr().cast(), buf.len())
                };
                if n < 0 {
                    Err(io::Error::last_os_error())
                } else {
                    Ok(n as usize)
                }
            }) {
                Ok(Err(error)) if error.raw_os_error() == Some(libc::EIO) => return Ok(0),
                Ok(result) => return result,
                Err(_would_block) => continue,
            }
        }
    }

    /// Types `data` into the terminal.
    pub async fn write_all(&self, mut data: &[u8]) -> io::Result<()> {
        while !data.is_empty() {
            let mut ready = self.0.writable().await?;
            match ready.try_io(|fd| {
                // SAFETY: writes from `data`, within its length.
                let n = unsafe {
                    libc::write(fd.get_ref().as_raw_fd(), data.as_ptr().cast(), data.len())
                };
                if n < 0 {
                    Err(io::Error::last_os_error())
                } else {
                    Ok(n as usize)
                }
            }) {
                Ok(Ok(n)) => data = &data[n..],
                Ok(Err(error)) => return Err(error),
                Err(_would_block) => continue,
            }
        }
        Ok(())
    }
}
