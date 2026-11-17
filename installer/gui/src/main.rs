//! `kindil-installer-gui`: the installer in a window.
// No console window behind it on Windows.
#![cfg_attr(windows, windows_subsystem = "windows")]

fn main() -> anyhow::Result<()> {
    kindil_installer_gui::run()
}
