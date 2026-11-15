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
          # kindil's own wpa_supplicant: nixpkgs' pinned upstream source, built
          # with the same Zig cross compiler into one static binary. Only what
          # the PW1 needs: its wireless-extensions driver, WPA/WPA2 personal,
          # the control socket kindil talks to, and built-in crypto (no
          # OpenSSL, libnl or D-Bus).
          wpaSupplicant = pkgs.stdenv.mkDerivation {
            pname = "wpa_supplicant-device";
            inherit (pkgs.wpa_supplicant) version src;
            nativeBuildInputs = [ pkgs.zig ];
            dontConfigure = true;
            config = ''
              CONFIG_DRIVER_WEXT=y
              CONFIG_CTRL_IFACE=y
              CONFIG_BACKEND=file
              CONFIG_NO_CONFIG_WRITE=y
              CONFIG_L2_PACKET=linux
              CONFIG_ELOOP=eloop
              CONFIG_TLS=internal
              CONFIG_CRYPTO=internal
              CONFIG_INTERNAL_LIBTOMMATH=y
            '';
            # Not CONFIG_NO_WPA_PASSPHRASE: besides the wpa_passphrase tool it
            # drops PBKDF2, and with it every typed (quoted) password.
            passAsFile = [ "config" ];
            buildPhase = ''
              runHook preBuild
              export ZIG_GLOBAL_CACHE_DIR="$TMPDIR/zig-global-cache"
              export ZIG_LOCAL_CACHE_DIR="$TMPDIR/zig-local-cache"
              cd wpa_supplicant
              cp "$configPath" .config
              # From the environment: the Makefile appends its include paths,
              # which make variables on the command line would override.
              export CFLAGS="-Os -MMD -Wall" LDFLAGS="-static -s"
              make -j"$NIX_BUILD_CORES" \
                CC="${linker}/bin/armv7l-unknown-linux-gnueabihf-gcc" \
                wpa_supplicant wpa_cli
              runHook postBuild
            '';
            installPhase = ''
              mkdir -p $out/bin
              install -m755 wpa_supplicant wpa_cli $out/bin/
            '';
            dontStrip = true;
            dontFixup = true;
            meta.license = pkgs.lib.licenses.bsd3;
          };
          # kindil's own SSH server (src/ssh.rs): nixpkgs' pinned upstream
          # Dropbear, built with the same Zig cross compiler into one static
          # dropbearmulti holding dropbear, dropbearkey and scp (`cargo xtask`
          # copies with `scp -O`, which runs scp on the Kindle). Keys only: no
          # passwords, PAM, zlib, login records, sftp, X11 or agent forwarding.
          dropbear = pkgs.stdenv.mkDerivation {
            pname = "dropbear-device";
            inherit (pkgs.dropbear) version src;
            nativeBuildInputs = [ pkgs.zig ];
            # PATH for sessions; dropbear sets it for root regardless of its
            # own environment. The first directory is src/ssh.rs's BIN (its
            # tests check it is here), so scp is this one.
            localOptions = ''
              #define DEFAULT_PATH "/var/local/kindil/ssh/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin"
              #define DEFAULT_ROOT_PATH DEFAULT_PATH
              #define DROPBEAR_PIDFILE "/var/run/kindil-dropbear.pid"
              #define DROPBEAR_SVR_PASSWORD_AUTH 0
              #define DROPBEAR_SVR_AGENTFWD 0
              #define DROPBEAR_X11FWD 0
              #define DROPBEAR_SFTPSERVER 0
              #define DO_MOTD 0
              #define DROPBEAR_REEXEC 0
              #define INETD_MODE 0
              #define DROPBEAR_SVR_LOCALSTREAMFWD 0
              #define DROPBEAR_SVR_REMOTESTREAMFWD 0
              /* Ed25519 and RSA keys; Curve25519 and sntrup761 key exchange. */
              #define DROPBEAR_ECDSA 0
              #define DROPBEAR_ECDH 0
              #define DROPBEAR_SK_KEYS 0
              #define DROPBEAR_MLKEM768 0
            '';
            passAsFile = [ "localOptions" ];
            # kindil keeps authorized_keys in its own root-only directory,
            # whose parents on the Kindle (/var, /var/local, /var/local/kindil)
            # are group-writable: Dropbear walks up to / and would refuse it.
            # Check the file's own directory only. Those parents already hold
            # the kindil binary that runs as root, so this opens nothing new.
            postPatch = ''
              substituteInPlace src/svr-authpubkey.c \
                --replace-fail \
                  'if (strcmp(path, ses.authstate.pw_dir) == 0 || strcmp(path, "/") == 0) {' \
                  'if (1) {'
            '';
            configurePhase = ''
              runHook preConfigure
              export ZIG_GLOBAL_CACHE_DIR="$TMPDIR/zig-global-cache"
              export ZIG_LOCAL_CACHE_DIR="$TMPDIR/zig-local-cache"
              cp "$localOptionsPath" localoptions.h
              # The PW1's 2.6.31 kernel has no getrandom(): /dev/urandom only.
              export ac_cv_func_getrandom=no
              ./configure --host=arm-linux-musleabihf \
                CC="${linker}/bin/armv7l-unknown-linux-gnueabihf-gcc" \
                AR="${pkgs.zig}/bin/zig ar" RANLIB="${pkgs.zig}/bin/zig ranlib" \
                CFLAGS="-Os" \
                --enable-static --disable-zlib --disable-pam \
                --disable-lastlog --disable-utmp --disable-utmpx \
                --disable-wtmp --disable-wtmpx \
                --disable-pututline --disable-pututxline
              runHook postConfigure
            '';
            buildPhase = ''
              runHook preBuild
              make -j"$NIX_BUILD_CORES" MULTI=1 STATIC=1 \
                PROGRAMS="dropbear dropbearkey scp" \
                LDFLAGS="-static -s"
              runHook postBuild
            '';
            installPhase = ''
              mkdir -p $out/bin
              install -m755 dropbearmulti $out/bin/
            '';
            dontStrip = true;
            dontFixup = true;
            meta.license = pkgs.lib.licenses.mit;
          };
          common = {
            pname = "kindil";
            version = "0.4.0";
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
                # kindil carries its launcher (launcher/, src/launcher.rs).
                cargo build --locked --offline --release --target ${target} --package launcher --jobs "$NIX_BUILD_CORES"
                export KINDIL_LAUNCHER="$PWD/target/${target}/release/launcher"
                cargo build --locked --offline --release --target ${target} --features kindle --jobs "$NIX_BUILD_CORES"
                runHook postBuild
              '';
              RUSTFLAGS = "-C linker=${linker}/bin/armv7l-unknown-linux-gnueabihf-gcc -C target-feature=+crt-static -C link-self-contained=no";
              CC_armv7_unknown_linux_musleabihf = "${linker}/bin/armv7l-unknown-linux-gnueabihf-gcc";
              KINDIL_WPA_SUPPLICANT = "${wpaSupplicant}/bin/wpa_supplicant";
              KINDIL_DROPBEAR = "${dropbear}/bin/dropbearmulti";
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
            wpa-supplicant-device = wpaSupplicant;
            dropbear-device = dropbear;
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
              pkgs.pkg-config
              pkgs.shellcheck
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
