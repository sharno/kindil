//! kindil (قنديل): a Rust/Slint wall dashboard and reader that runs as an app
//! on the Kindle Paperwhite's stock OS.
pub mod battery;
pub mod config;
pub mod dashboards;
pub mod drain;
pub mod files;
pub mod frontlight;
pub mod ghosting;
pub mod host;
pub mod kindle;
pub mod lipc;
pub mod net;
pub mod prefs;
pub mod preview;
pub mod reader;
pub mod signals;
pub mod sync;
pub mod system;
pub mod uevent;
pub mod ui;
pub mod update;
pub mod view;
pub mod wifi;
