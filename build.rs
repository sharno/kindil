fn main() {
    slint_build::compile_with_config(
        "ui/app.slint",
        slint_build::CompilerConfiguration::new()
            .embed_resources(slint_build::EmbedResourcesKind::EmbedFiles),
    )
    .expect("compile Slint UI");
    // kindil's own wpa_supplicant (the flake's wpa-supplicant-device).
    embed(
        "KINDIL_WPA_SUPPLICANT",
        "wpa_supplicant",
        "nix build .#wpa-supplicant-device",
    );
    // Its launcher (launcher/, built for the Kindle first).
    embed(
        "KINDIL_LAUNCHER",
        "launcher",
        "cargo build -p launcher --release --target ...",
    );
}

/// kindil carries binaries it writes out on the Kindle: the environment
/// variable `var` names one, embedded as OUT_DIR/`name`. A device build
/// without it fails (`how` builds it); other builds embed nothing.
fn embed(var: &str, name: &str, how: &str) {
    println!("cargo:rerun-if-env-changed={var}");
    let out = std::path::PathBuf::from(std::env::var_os("OUT_DIR").expect("OUT_DIR"));
    let embedded = out.join(name);
    let device = std::env::var("CARGO_CFG_TARGET_ARCH").is_ok_and(|arch| arch == "arm")
        && std::env::var_os("CARGO_FEATURE_KINDLE").is_some();
    match std::env::var_os(var) {
        Some(path) => {
            println!("cargo:rerun-if-changed={}", path.to_string_lossy());
            std::fs::copy(&path, &embedded).unwrap_or_else(|e| panic!("copy {var}: {e}"));
        }
        None if device => panic!("a Kindle build needs {var}: {how}"),
        None => std::fs::write(&embedded, b"").expect("write an empty placeholder"),
    }
}
