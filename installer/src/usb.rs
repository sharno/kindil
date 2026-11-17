//! Kindles on the USB bus, by vendor: Lab126 (Amazon's device maker),
//! 0x1949. A Kindle reports its own serial number as the USB device's
//! serial (`iSerial`), e.g. `B024...` for a Paperwhite 1. Reading it needs
//! no driver and no access to the device: the OS lists it (sysfs on Linux,
//! IOKit on macOS, SetupAPI on Windows).

/// Lab126's USB vendor id.
pub const LAB126: u16 = 0x1949;

/// The serial numbers of the Lab126 devices plugged in; empty if none, or
/// if the bus can't be listed (the volume is then all there is to go by).
#[cfg(feature = "usb")]
pub fn kindle_serials() -> Vec<String> {
    use nusb::MaybeFuture;
    let Ok(devices) = nusb::list_devices().wait() else {
        return Vec::new();
    };
    devices
        .filter(|d| d.vendor_id() == LAB126)
        .filter_map(|d| d.serial_number().map(|s| s.trim().to_owned()))
        .filter(|s| !s.is_empty())
        .collect()
}

#[cfg(not(feature = "usb"))]
pub fn kindle_serials() -> Vec<String> {
    Vec::new()
}
