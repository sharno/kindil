{
  description = "kindil (قنديل): Slint dashboard and reader for Kindle PW1";
  inputs = {
    self.submodules = true;
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };
  outputs =
    {
      self,
      nixpkgs,
      rust-overlay,
    }:
    let
      systems = [
        "x86_64-linux"
        "aarch64-linux"
      ];
      forSystems = nixpkgs.lib.genAttrs systems;
      perSystem =
        system:
        let
          pkgs = import nixpkgs {
            inherit system;
            overlays = [ rust-overlay.overlays.default ];
          };
          # Official prebuilt Rust + ARM musl std: no source-built cross rustc/GCC.
          rust = pkgs.rust-bin.stable."1.99.1".default.override {
            extensions = [
              "clippy"
              "rustfmt"
            ];
            targets = [ "armv7-unknown-linux-musleabihf" ];
          };
          platform = pkgs.makeRustPlatform {
            cargo = rust;
            rustc = rust;
          };
          linker = pkgs.runCommand "kindil-arm-musl-linker" { } ''
            mkdir -p $out/bin
            cat > $out/bin/armv7l-unknown-linux-gnueabihf-gcc <<'EOF'
            #!${pkgs.runtimeShell}
            # cc-rs passes a Rust/LLVM triple which Zig spells differently.
            # The final explicit target wins, including the old kernel baseline.
            exec ${pkgs.zig}/bin/zig cc "$@" -target arm-linux.2.6.31-musleabihf -mcpu=cortex_a8
            EOF
            chmod +x $out/bin/armv7l-unknown-linux-gnueabihf-gcc
          '';
          target = "armv7-unknown-linux-musleabihf";
          common = {
            pname = "kindil";
            version = "0.3.0";
            src = pkgs.lib.cleanSource ./.;
            cargoLock.lockFile = ./Cargo.lock;
            meta.license = pkgs.lib.licenses.mit;
          };
          host = platform.buildRustPackage common;
          device = platform.buildRustPackage (
            common
            // {
              pname = "kindil-device";
              # The native buildRustPackage hook always adds its host --target.
              # Adding another target builds BOTH and applies the ARM linker to
              # the native executable too. Select only the prebuilt ARM target.
              buildPhase = ''
                runHook preBuild
                cargo build --locked --offline --release --target ${target} --features kindle --jobs "$NIX_BUILD_CORES"
                runHook postBuild
              '';
              RUSTFLAGS = "-C linker=${linker}/bin/armv7l-unknown-linux-gnueabihf-gcc -C target-feature=+crt-static -C link-self-contained=no";
              CC_armv7_unknown_linux_musleabihf = "${linker}/bin/armv7l-unknown-linux-gnueabihf-gcc";
              preBuild = ''
                # Nix's sandbox HOME is intentionally unwritable. Zig needs a
                # writable cache for its musl/CRT artifacts as well as ring C.
                export ZIG_GLOBAL_CACHE_DIR="$TMPDIR/zig-global-cache"
                export ZIG_LOCAL_CACHE_DIR="$TMPDIR/zig-local-cache"
              '';
              doCheck = false;
              # Cargo's release profile strips the ARM binary; do not run a
              # host-only strip tool on it during native stdenv fixup.
              dontStrip = true;
              installPhase = ''
                mkdir -p $out/bin
                install -m755 target/${target}/release/kindil $out/bin/kindil
              '';
            }
          );
        in
        {
          packages = {
            default = host;
            kindil = host;
            kindil-device = device;
            armv7-linker = linker;
          };
          checks = {
            host = host;
          };
          devShells.default = pkgs.mkShell {
            packages = [
              rust
              linker
              pkgs.qemu
              pkgs.binutils
              pkgs.cpio
              pkgs.gzip
              pkgs.pkg-config
            ];
            CARGO_TARGET_ARMV7_UNKNOWN_LINUX_MUSLEABIHF_LINKER = "${linker}/bin/armv7l-unknown-linux-gnueabihf-gcc";
            CARGO_TARGET_ARMV7_UNKNOWN_LINUX_MUSLEABIHF_RUNNER = "${pkgs.qemu}/bin/qemu-arm";
          };
          formatter = pkgs.nixfmt-tree;
        };
    in
    {
      packages = forSystems (s: (perSystem s).packages);
      checks = forSystems (s: (perSystem s).checks);
      devShells = forSystems (s: (perSystem s).devShells);
      formatter = forSystems (s: (perSystem s).formatter);
    };
}
