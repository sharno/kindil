//! kindil: a single-process shell for the first-generation Kindle Paperwhite.
//!
//! The crate is split into a library and a binary so the hardware layers can be
//! exercised by tests and by the simulator.
//!
//! # Layers
//!
//! * [`canvas`] - the shadow canvas and frame differ.
//! * [`render`] - drawing text and shapes into the canvas.
//! * [`shell`] - the screens, the transitions and the frame loop.
//! * [`panel`] - the [`panel::Panel`] trait: the device panel or the simulator.
//! * [`epdc`] - the Freescale EPDC ioctl ABI: update regions and waveforms.
//! * [`fb`] - mapping `/dev/fb0`, reading the panel geometry, and blitting.
//! * [`input`] - touch events through evdev.
//! * [`frontlight`] - the `/dev/yoshime_fl` ioctls.
//! * [`civil`], [`time`], [`tz`] - the wall clock and POSIX timezones.
//! * [`boot`] - `/dev` setup when running as PID 1.
//! * [`sim`] - a memory panel and a scripted render tour.
//! * [`sys`] - the raw C library bindings.

pub mod boot;
pub mod canvas;
pub mod civil;
pub mod epdc;
pub mod fb;
pub mod font;
pub mod frontlight;
pub mod input;
pub mod panel;
pub mod render;
pub mod shell;
pub mod sim;
pub mod sys;
pub mod time;
pub mod tz;
