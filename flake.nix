{
  description = "kindil (قنديل) - a single-process Rust shell for the first-generation Amazon Kindle Paperwhite";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs =
    {
      self,
      nixpkgs,
    }:
    let
      # The device runs a 2.6.31-era kernel with a uClibc userland, so we ship a
      # statically linked ARMv7 musl binary: no dynamic loader, no dependency on
      # whatever Amazon happens to have installed.
      #
      # `isStatic = true` is how nixpkgs' own `pkgsStatic` set is built (see
      # `pkgs/top-level/stage.nix`). It is the one configuration that wires
      # `crt-static` and a static musl up for the Rust target, at the cost of
      # building a dedicated cross GCC the first time.
      armv7System = "armv7l-linux";
      armv7Pkgs = import nixpkgs {
        system = "x86_64-linux";
        crossSystem = {
          isStatic = true;
          config = "armv7l-unknown-linux-musleabihf";
          host = "armv7l-unknown-linux-musleabihf";
          build = "x86_64-linux";
        };
      };

      # A cross GCC for the *host* toolchain, used only as a link driver by the
      # fast development loop. Rust's `armv7-unknown-linux-musleabihf` target is
      # self-contained: rustc passes its own crt objects and musl `libc.a` from
      # the rustup toolchain, so the driver's own libc is never linked. That
      # makes this cached glibc cross compiler sufficient, and a device build
      # takes seconds instead of building a second GCC from source.
      armv7Linker = nixpkgs.legacyPackages.x86_64-linux.pkgsCross.armv7l-hf-multiplatform.stdenv.cc;

      # Systems we build a native package for. `builtins.currentSystem` is not
      # available in a pure flake evaluation, so the systems are spelled out.
      hostSystems = [
        "x86_64-linux"
        "aarch64-linux"
      ];

      kindilFor =
        pkgs:
        let
          # The Rust target triple this build produces: the native triple for a
          # host build, ARMv7 musl for the cross build.
          rustcTargetSpec = pkgs.stdenv.hostPlatform.rust.rustcTargetSpec;
        in
        pkgs.rustPlatform.buildRustPackage {
          pname = "kindil";
          version = "0.1.0";

          src = ./.;

          # No dependencies, so the lock file is trivial and the build is fully
          # offline.
          cargoLock.lockFile = ./Cargo.lock;

          cargoBuildFlags = [ "--target" rustcTargetSpec ];

          installPhase = ''
            runHook preInstall

            mkdir -p $out/bin
            install -m755 target/${rustcTargetSpec}/release/kindil $out/bin/kindil

            runHook postInstall
          '';

          meta = {
            description = "A single-process Rust shell for the first-generation Kindle Paperwhite";
            longDescription = ''
              kindil owns the e-ink panel (via the Freescale EPDC framebuffer
              driver) and the touch controller, and renders a clock and a small
              menu. There is no init, no window manager and no compositor. It
              runs as a KUAL extension, as PID 1 in an initramfs, or on a
              workstation with `--sim`.
            '';
            license = pkgs.lib.licenses.mit;
            platforms = pkgs.lib.platforms.unix ++ [ "armv7l-linux" ];
          };
        };

      armv7Package = kindilFor armv7Pkgs;

      hostPackages = nixpkgs.lib.genAttrs hostSystems (
        system:
        let
          pkgs = nixpkgs.legacyPackages.${system};
          kindil = kindilFor pkgs;
        in
        {
          inherit kindil;
          default = kindil;
        }
      );

      devShells = nixpkgs.lib.genAttrs hostSystems (
        system:
        let
          pkgs = nixpkgs.legacyPackages.${system};
          # The fast device loop needs a link driver for the ARM target. Only
          # x86_64 hosts have the cached cross compiler wired up.
          extra =
            if system == "x86_64-linux" then
              [ nixpkgs.legacyPackages.${system}.pkgsCross.armv7l-hf-multiplatform.stdenv.cc ]
            else
              [ ];
        in
        {
          default = pkgs.mkShell {
            packages = [
              pkgs.cargo
              pkgs.rustc
              pkgs.rustfmt
              pkgs.clippy
            ] ++ extra;
          };
        }
      );

      formatter = nixpkgs.lib.genAttrs hostSystems (
        system: nixpkgs.legacyPackages.${system}.nixfmt-tree
      );
    in
    {
      packages = hostPackages // {
        x86_64-linux = hostPackages.x86_64-linux // {
          # `kindil --sim`, the cross link driver, and the device build.
          armv7-linker = armv7Linker;
        };
        ${armv7System} = {
          kindil = armv7Package;
          default = armv7Package;
        };
      };

      checks = {
        # Building the device binary is itself the meaningful check: it proves
        # the crate cross-compiles against the ARM32 ABI.
        ${armv7System}.kindil = armv7Package;

        # Run the unit tests on the host. The integration tests drive the real
        # renderer and the in-memory panel, so no device is needed.
        x86_64-linux.kindil-tests =
          nixpkgs.legacyPackages.x86_64-linux.rustPlatform.buildRustPackage {
            pname = "kindil-tests";
            version = "0.1.0";
            src = ./.;
            cargoLock.lockFile = ./Cargo.lock;
            buildAndInstallSubcommand = "cargo";
            buildPhase = ''
              runHook preBuild
              cargo test --offline
              runHook postBuild
            '';
            installPhase = ''
              runHook preInstall
              touch $out
              runHook postInstall
            '';
          };

        # The device binary must be a static ARM executable: the Kindle has no
        # dynamic loader for musl, so a dynamic binary would simply not start.
        x86_64-linux.kindil-device-static =
          let
            pkgs = nixpkgs.legacyPackages.x86_64-linux;
          in
          pkgs.runCommand "check-kindil-device-static"
            {
              nativeBuildInputs = [
                pkgs.file
                pkgs.binutils
              ];
            }
            ''
              binary=${armv7Package}/bin/kindil
              file "$binary"
              file "$binary" | grep -q 'ELF 32-bit LSB executable, ARM' || {
                echo "error: not an ARM32 executable" >&2
                exit 1
              }
              if readelf -l "$binary" | grep -q INTERP; then
                echo "error: the binary is dynamically linked" >&2
                exit 1
              fi
              touch $out
            '';
      };

      inherit devShells;
      inherit formatter;
    };
}
