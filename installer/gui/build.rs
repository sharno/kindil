fn main() {
    slint_build::compile_with_config(
        "ui/installer.slint",
        slint_build::CompilerConfiguration::new()
            .embed_resources(slint_build::EmbedResourcesKind::EmbedFiles),
    )
    .expect("compile the installer's Slint UI");
}
