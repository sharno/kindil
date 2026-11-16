//! Reusable Miqat dashboard engine. The CLI and embedded consumers use the
//! same configuration, prayer calculations, layout, fonts and contrast path.
pub mod config;
pub mod hijri;
pub mod image;
pub mod layout;
pub mod prayer;
pub mod radar;
pub mod render;
pub mod sources;
pub mod weather;
