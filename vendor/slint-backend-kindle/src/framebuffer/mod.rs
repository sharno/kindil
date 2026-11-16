mod ffi;

use std::cell::Cell;
use std::ops::Range;
use std::os::fd::{AsRawFd, RawFd};
use std::sync::atomic::{AtomicU32, AtomicU8, Ordering};

/// The marker the next update carries. Each update gets its own, so a wait
/// is for that update and not for whichever earlier one shared a fixed
/// marker; never 0, which the driver takes as "no marker". kindil patch.
static NEXT_MARKER: AtomicU32 = AtomicU32::new(1);
/// The marker of the last update sent to the panel.
static LAST_MARKER: AtomicU32 = AtomicU32::new(0);

fn next_marker() -> u32 {
    loop {
        let marker = NEXT_MARKER.fetch_add(1, Ordering::Relaxed);
        if marker != 0 {
            LAST_MARKER.store(marker, Ordering::Release);
            return marker;
        }
    }
}

/// Which wait ioctl answered last: 0 not known yet, 1 the marker alone
/// (PW1), 2 the marker struct (PW2 and later).
static WAIT_FORM: AtomicU8 = AtomicU8::new(0);

use ffi::{
    FBIOGET_FSCREENINFO, FBIOGET_VSCREENINFO, FbFixScreeninfo, FbVarScreeninfo, MXCFB_SEND_UPDATE,
    MXCFB_SEND_UPDATE_MTK, MXCFB_SEND_UPDATE_REX, MXCFB_SEND_UPDATE_ZELDA,
    MXCFB_WAIT_FOR_UPDATE_COMPLETE, MXCFB_WAIT_FOR_UPDATE_COMPLETE_U32, TEMP_USE_AMBIENT, UPDATE_MODE_FULL, UPDATE_MODE_PARTIAL,
    UpdateMarkerData, UpdateRect, UpdateRequest, UpdateRequestMtk, UpdateRequestRex,
    UpdateRequestZelda, WAVEFORM_MODE_A2, WAVEFORM_MODE_AUTO, WAVEFORM_MODE_GC16,
};

/// How the panel moves pixels for a partial update. kindil patch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Pass {
    /// Changed pixels only; the driver picks the waveform.
    Auto,
    /// Changed pixels only, black and white, fastest: content in motion.
    Fast,
    /// Every pixel of the area, black and white: clears what fast passes
    /// left behind without stopping the motion for long.
    FastClean,
    /// Every pixel of the area in 16-level grayscale, with a flash: a hard
    /// clean once motion ends.
    Clean,
}

impl Pass {
    /// The waveform and update mode.
    fn update(self) -> (u32, u32) {
        match self {
            Pass::Auto => (WAVEFORM_MODE_AUTO, UPDATE_MODE_PARTIAL),
            Pass::Fast => (WAVEFORM_MODE_A2, UPDATE_MODE_PARTIAL),
            Pass::FastClean => (WAVEFORM_MODE_A2, UPDATE_MODE_FULL),
            Pass::Clean => (WAVEFORM_MODE_GC16, UPDATE_MODE_FULL),
        }
    }
}

/// Which MXCFB update ioctl this kernel accepts, varies with the Kindle model
///
//We probe on first refresh and remember the winner instead of retrying the failing ioctl every frame.
#[derive(Clone, Copy)]
enum UpdateVariant {
    /// `MXCFB_SEND_UPDATE` — 72-byte struct, older devices.
    Legacy,
    /// `MXCFB_SEND_UPDATE_REX` — 80-byte struct, Paperwhite 10th gen and newer.
    Rex,
    /// `MXCFB_SEND_UPDATE_ZELDA` — 88-byte struct, KOA2/KOA3 (Oasis 2/3).
    Zelda,
    /// `MXCFB_SEND_UPDATE_MTK` — 96-byte struct, MediaTek devices (Basic 2022,
    /// Paperwhite 5, Scribe).
    Mtk,
    /// No variant was accepted. Cached so a hopeless kernel costs one probe
    /// and one error log total, not three failing ioctls per frame.
    Unsupported,
}

impl UpdateVariant {
    /// Probed oldest ABI first.
    const PROBE_ORDER: [UpdateVariant; 4] = [Self::Legacy, Self::Rex, Self::Zelda, Self::Mtk];
}

/// Memory-mapped handle to the Kindle's e-ink framebuffer.
///
/// Pixel format is 8-bit grayscale (one byte per pixel). The `stride` may be
/// wider than `width` due to hardware alignment requirements.
pub(crate) struct Framebuffer {
    file: std::fs::File,
    map: *mut u8,
    len: usize,
    pub(crate) width: u32,
    pub(crate) height: u32,
    stride: usize,
    /// The update ioctl variant this kernel accepts, cached after the first
    /// successful refresh. `None` until then.
    update_variant: Cell<Option<UpdateVariant>>,
}

// SAFETY: The mmap is process-wide and we only access it from the event loop thread.
unsafe impl Send for Framebuffer {}

impl Framebuffer {
    /// Open the framebuffer device and query its geometry from the kernel.
    ///
    /// This works on any Kindle model - the resolution and stride are read at
    /// runtime rather than being hardcoded.
    pub(crate) fn open() -> std::io::Result<Self> {
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open("/dev/fb0")?;

        let fd = file.as_raw_fd();

        let mut vinfo = FbVarScreeninfo::default();
        if unsafe {
            libc::ioctl(
                fd,
                FBIOGET_VSCREENINFO as _,
                &mut vinfo as *mut _ as *mut libc::c_void,
            )
        } == -1
        {
            return Err(std::io::Error::last_os_error());
        }

        let mut finfo = FbFixScreeninfo::default();
        if unsafe {
            libc::ioctl(
                fd,
                FBIOGET_FSCREENINFO as _,
                &mut finfo as *mut _ as *mut libc::c_void,
            )
        } == -1
        {
            return Err(std::io::Error::last_os_error());
        }

        let width = vinfo.xres;
        let height = vinfo.yres;
        let stride = finfo.line_length as usize;

        // The whole render path treats the mmap as one byte per pixel. A
        // different depth would silently produce garbled output, so reject it
        // with a clear error instead.
        if vinfo.bits_per_pixel != 8 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!(
                    "unsupported framebuffer depth: {} bpp (expected 8-bit grayscale)",
                    vinfo.bits_per_pixel
                ),
            ));
        }

        if width == 0 || height == 0 || stride < width as usize {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("invalid framebuffer geometry: {width}x{height}, stride={stride}"),
            ));
        }

        let len = stride * height as usize;

        let map = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                len,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED,
                fd,
                0,
            )
        };

        if map == libc::MAP_FAILED {
            return Err(std::io::Error::last_os_error());
        }

        Ok(Self {
            file,
            map: map as *mut u8,
            len,
            width,
            height,
            stride,
            update_variant: Cell::new(None),
        })
    }

    /// Write a horizontal span of grayscale pixels into the mmap at row `y`.
    pub(crate) fn write_line(&mut self, y: usize, x_range: Range<usize>, pixels: &[u8]) {
        let row_span = unsafe {
            std::slice::from_raw_parts_mut(
                self.map.add(y * self.stride + x_range.start),
                pixels.len(),
            )
        };
        row_span.copy_from_slice(pixels);
    }

    /// Fill the entire visible area with a single grayscale value (0x00 = black, 0xff = white).
    pub(crate) fn fill(&mut self, value: u8) {
        for y in 0..self.height as usize {
            let row = unsafe {
                std::slice::from_raw_parts_mut(self.map.add(y * self.stride), self.width as usize)
            };
            row.fill(value);
        }
    }

    /// Ask the EPDC to refresh a region of the e-ink panel.
    ///
    /// On the first call we probe which update ioctl the kernel accepts and
    /// cache it, so later frames issue exactly one ioctl instead of retrying a
    /// known-failing one every refresh.
    fn send_update(&self, region: UpdateRect, waveform: u32, mode: u32) {
        let marker = next_marker();
        match self.update_variant.get() {
            Some(variant) => {
                self.send_update_variant(variant, region, waveform, mode, marker);
            }
            None => {
                let accepted = UpdateVariant::PROBE_ORDER.into_iter().find(|&variant| {
                    self.send_update_variant(variant, region, waveform, mode, marker)
                });
                if accepted.is_none() {
                    log::error!(
                        "EPDC refresh failed: no known MXCFB_SEND_UPDATE variant \
                         was accepted; the screen will not update"
                    );
                }
                self.update_variant
                    .set(Some(accepted.unwrap_or(UpdateVariant::Unsupported)));
            }
        }
    }

    /// Issue one specific update ioctl variant. Returns whether it succeeded.
    fn send_update_variant(
        &self,
        variant: UpdateVariant,
        region: UpdateRect,
        waveform: u32,
        mode: u32,
        marker: u32,
    ) -> bool {
        match variant {
            UpdateVariant::Legacy => self.send_update_legacy(region, waveform, mode, marker),
            UpdateVariant::Rex => self.send_update_rex(region, waveform, mode, marker),
            UpdateVariant::Zelda => self.send_update_zelda(region, waveform, mode, marker),
            UpdateVariant::Mtk => self.send_update_mtk(region, waveform, mode, marker),
            UpdateVariant::Unsupported => false,
        }
    }

    /// Hand one of the `UpdateRequest*` structs to the kernel. Returns whether
    /// the ioctl succeeded.
    fn send_update_ioctl<T>(&self, request: libc::c_ulong, update: &T) -> bool {
        // SAFETY: `update` outlives the ioctl and matches the kernel's struct.
        unsafe { libc::ioctl(self.file.as_raw_fd(), request as _, update as *const _) != -1 }
    }

    /// Issue the legacy `MXCFB_SEND_UPDATE` (72-byte struct). Returns whether the
    /// ioctl succeeded.
    fn send_update_legacy(
        &self,
        region: UpdateRect,
        waveform: u32,
        mode: u32,
        marker: u32,
    ) -> bool {
        let update = UpdateRequest {
            update_region: region,
            waveform_mode: waveform,
            update_mode: mode,
            update_marker: marker,
            temperature: TEMP_USE_AMBIENT,
            ..Default::default()
        };
        self.send_update_ioctl(MXCFB_SEND_UPDATE, &update)
    }

    /// Issue the modern `MXCFB_SEND_UPDATE_REX` (80-byte struct). Returns whether
    /// the ioctl succeeded.
    fn send_update_rex(
        &self,
        region: UpdateRect,
        waveform: u32,
        mode: u32,
        marker: u32,
    ) -> bool {
        let update = UpdateRequestRex {
            update_region: region,
            waveform_mode: waveform,
            update_mode: mode,
            update_marker: marker,
            temperature: TEMP_USE_AMBIENT,
            ..Default::default()
        };
        self.send_update_ioctl(MXCFB_SEND_UPDATE_REX, &update)
    }

    /// Issue `MXCFB_SEND_UPDATE_MTK` (96-byte struct, MediaTek devices).
    /// Returns whether the ioctl succeeded.
    fn send_update_mtk(
        &self,
        region: UpdateRect,
        waveform: u32,
        mode: u32,
        marker: u32,
    ) -> bool {
        let update = UpdateRequestMtk {
            update_region: region,
            waveform_mode: waveform,
            update_mode: mode,
            update_marker: marker,
            temperature: TEMP_USE_AMBIENT,
            ..Default::default()
        };
        self.send_update_ioctl(MXCFB_SEND_UPDATE_MTK, &update)
    }

    /// Issue `MXCFB_SEND_UPDATE_ZELDA` (88-byte struct, KOA2/KOA3 — Oasis 2/3).
    /// Returns whether the ioctl succeeded.
    fn send_update_zelda(
        &self,
        region: UpdateRect,
        waveform: u32,
        mode: u32,
        marker: u32,
    ) -> bool {
        let update = UpdateRequestZelda {
            update_region: region,
            waveform_mode: waveform,
            update_mode: mode,
            update_marker: marker,
            temperature: TEMP_USE_AMBIENT,
            ..Default::default()
        };
        self.send_update_ioctl(MXCFB_SEND_UPDATE_ZELDA, &update)
    }

    /// Full-screen GC16 refresh
    pub(crate) fn refresh_full(&self) {
        self.send_update(
            UpdateRect {
                top: 0,
                left: 0,
                width: self.width,
                height: self.height,
            },
            WAVEFORM_MODE_GC16,
            UPDATE_MODE_FULL,
        );
    }

    /// Block until the EPDC has applied the last update.
    ///
    /// Used before writing pixels the panel may still be driving.
    /// Best-effort: a failing ioctl is ignored, since this is purely defensive.
    pub(crate) fn wait_for_update_complete(&self) {
        let _ = wait_marker(self.file.as_raw_fd(), LAST_MARKER.load(Ordering::Acquire));
    }

    /// Refresh a rectangle of the panel.
    pub(crate) fn refresh_region(
        &self,
        origin: slint::PhysicalPosition,
        size: slint::PhysicalSize,
        pass: Pass,
    ) {
        let (waveform, mode) = pass.update();
        if size.width == 0 || size.height == 0 {
            return;
        }
        self.send_update(
            UpdateRect {
                top: origin.y as u32,
                left: origin.x as u32,
                width: size.width,
                height: size.height,
            },
            waveform,
            mode,
        );
    }
}

/// Waits for the update with `marker`, in whichever form the driver takes:
/// the marker alone (PW1's 2.6.31 kernel) or the marker struct (later ones).
/// The form that answered is tried first from then on. The error is the
/// errno of the form tried last.
fn wait_marker(fd: RawFd, marker: u32) -> std::io::Result<()> {
    if marker == 0 {
        // Nothing sent yet.
        return Ok(());
    }
    let alone = || {
        let mut value = marker;
        // SAFETY: a valid fd and a u32 for the ioctl's duration.
        let done = unsafe {
            libc::ioctl(
                fd,
                MXCFB_WAIT_FOR_UPDATE_COMPLETE_U32 as _,
                &mut value as *mut u32,
            )
        };
        if done >= 0 {
            Ok(())
        } else {
            Err(std::io::Error::last_os_error())
        }
    };
    let with_struct = || {
        let mut data = UpdateMarkerData {
            update_marker: marker,
            collision_test: 0,
        };
        // SAFETY: a valid fd and marker struct for the ioctl's duration.
        let done = unsafe {
            libc::ioctl(
                fd,
                MXCFB_WAIT_FOR_UPDATE_COMPLETE as _,
                &mut data as *mut UpdateMarkerData,
            )
        };
        if done >= 0 {
            Ok(())
        } else {
            Err(std::io::Error::last_os_error())
        }
    };
    let (first, second, forms): (&dyn Fn() -> std::io::Result<()>, &dyn Fn() -> _, _) =
        match WAIT_FORM.load(Ordering::Relaxed) {
            2 => (&with_struct, &alone, (2, 1)),
            _ => (&alone, &with_struct, (1, 2)),
        };
    let first = match first() {
        Ok(()) => {
            WAIT_FORM.store(forms.0, Ordering::Relaxed);
            return Ok(());
        }
        Err(error) => error,
    };
    match second() {
        Ok(()) => {
            WAIT_FORM.store(forms.1, Ordering::Relaxed);
            Ok(())
        }
        Err(second) => {
            let (alone, with_struct) = match forms {
                (1, _) => (first, second),
                _ => (second, first),
            };
            Err(std::io::Error::other(format!(
                "waiting for update {marker}: marker alone: {alone}; marker struct: {with_struct}"
            )))
        }
    }
}

/// [`Framebuffer::wait_for_update_complete`] on a descriptor of its own,
/// for threads other than the event loop's, for the last update sent.
pub(crate) fn wait_for_panel() -> std::io::Result<()> {
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/fb0")?;
    wait_marker(file.as_raw_fd(), LAST_MARKER.load(Ordering::Acquire))
}

impl Drop for Framebuffer {
    fn drop(&mut self) {
        unsafe { libc::munmap(self.map as *mut libc::c_void, self.len) };
    }
}
