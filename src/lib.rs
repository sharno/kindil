//! kindile: a single-process shell for the first-generation Kindle Paperwhite.
//!
//! The crate is split into a library and a binary so that the hardware
//! layers can be exercised by tests and, later, by a second front end.
//!
//! # Layers
//!
//! * [`epdc`] - the Freescale EPDC ioctl ABI: waveform modes, update regions,
//!   and the panel refresh handshake.
//! * [`fb`] - mapping `/dev/fb0` and reading the panel geometry.
//! * [`input`] - touch events through evdev.
//! * [`render`] - drawing text and shapes into the framebuffer.
//! * [`time`] - the wall clock, without pulling in a date/time crate.
//! * [`shell`] - the application itself: screens, transitions and updates.

pub mod epdc;
pub mod fb;
pub mod font;
pub mod frontlight;
pub mod input;
pub mod render;
pub mod shell;
pub mod sys;
pub mod time;
