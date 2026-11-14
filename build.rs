fn main() {
    slint_build::compile_with_config(
        "ui/app.slint",
        slint_build::CompilerConfiguration::new()
            .embed_resources(slint_build::EmbedResourcesKind::EmbedFiles),
    )
    .expect("compile Slint UI");
    wpa_supplicant();
}

/// kindil carries its own wpa_supplicant (the flake's wpa-supplicant-device)
/// and writes it out on the Kindle: KINDIL_WPA_SUPPLICANT names the binary.
/// A device build without it fails; other builds embed nothing.
fn wpa_supplicant() {
    println!("cargo:rerun-if-env-changed=KINDIL_WPA_SUPPLICANT");
    let out = std::path::PathBuf::from(std::env::var_os("OUT_DIR").expect("OUT_DIR"));
    let embedded = out.join("wpa_supplicant");
    let device = std::env::var("CARGO_CFG_TARGET_ARCH").is_ok_and(|arch| arch == "arm")
        && std::env::var_os("CARGO_FEATURE_KINDLE").is_some();
    match std::env::var_os("KINDIL_WPA_SUPPLICANT") {
        Some(path) => {
            println!("cargo:rerun-if-changed={}", path.to_string_lossy());
            std::fs::copy(&path, &embedded).expect("copy KINDIL_WPA_SUPPLICANT");
        }
        None if device => {
            panic!("a Kindle build needs KINDIL_WPA_SUPPLICANT: nix build .#wpa-supplicant-device")
        }
        None => std::fs::write(&embedded, b"").expect("write an empty placeholder"),
    }
}
