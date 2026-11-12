{
  description = "kindile - a single-process Rust shell for the first-generation Amazon Kindle Paperwhite";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs =
    { self, nixpkgs }:
    let
      # The device runs a 2.6.31-era kernel with a uClibc userland, so we ship
      # a statically linked ARMv7 musl binary: no dynamic loader, no libc, and
      # no dependency on whatever Amazon happens to have installed.
      #
      # `isStatic = true` is how nixpkgs' own `pkgsStatic` package set is
      # built (see `pkgs/top-level/stage.nix`): it selects musl as the libc
      # and makes the Rust target use `crt-static`.
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

      # Systems we build a native package for. `builtins.currentSystem` is not
      # available in a pure flake evaluation, so the systems are spelled out.
      hostSystems = [
        "x86_64-linux"
        "aarch64-linux"
      ];

      kindileFor =
        pkgs:
        let
          # The Rust target triple this build produces. For a host build it is
          # the native triple; for the cross build, ARMv7 musl.
          rustcTargetSpec = pkgs.stdenv.hostPlatform.rust.rustcTargetSpec;
        in
        pkgs.rustPlatform.buildRustPackage {
          pname = "kindile";
          version = "0.1.0";

          src = ./.;

          # No dependencies, so the lock file is trivial and the build is
          # fully offline.
          cargoLock.lockFile = ./Cargo.lock;

          buildAndInstallSubcommand = "cargo";
          installPhase = ''
            runHook preInstall

            mkdir -p $out/bin
            install -m755 target/${rustcTargetSpec}/release/kindile $out/bin/kindile

            runHook postInstall
          '';

          meta = {
            description = "A single-process Rust shell for the first-generation Kindle Paperwhite";
            longDescription = ''
              kindile owns the e-ink panel (via the Freescale EPDC framebuffer
              driver) and the touch controller, and renders a small clock and
              menu shell. It is designed to run either as a KUAL extension
              alongside Amazon's own UI, or as PID 1 in a custom initramfs.
            '';
            license = pkgs.lib.licenses.mit;
            # The device build is ARMv7; the host builds are for development.
            platforms = pkgs.lib.platforms.unix ++ [ "armv7l-linux" ];
          };
        };

      # The device build: static ARMv7 musl.
      armv7Package = kindileFor armv7Pkgs;

      hostPackages = nixpkgs.lib.genAttrs hostSystems (
        system:
        let
          pkgs = nixpkgs.legacyPackages.${system};
          kindile = kindileFor pkgs;
        in
        {
          inherit kindile;
          default = kindile;
        }
      );

      # A dev shell with the Rust toolchain, per host system.
      devShells = nixpkgs.lib.genAttrs hostSystems (
        system:
        let
          pkgs = nixpkgs.legacyPackages.${system};
        in
        {
          default = pkgs.mkShell {
            packages = [
              pkgs.cargo
              pkgs.rustc
              pkgs.rustfmt
              pkgs.clippy
            ];
          };
        }
      );

      # A formatter per host system. Nix expects `formatter.<system>` to be a
      # derivation.
      formatter = nixpkgs.lib.genAttrs hostSystems (system: nixpkgs.legacyPackages.${system}.nixfmt-tree);
    in
    {
      packages = hostPackages // {
        ${armv7System} = {
          kindile = armv7Package;
          default = armv7Package;
        };
      };

      checks = {
        # Building the device binary is itself the meaningful check: it proves
        # the crate cross-compiles against the ARM32 ABI.
        ${armv7System}.kindile = armv7Package;

        # Run the unit tests on the host.
        x86_64-linux.kindile-tests = nixpkgs.legacyPackages.x86_64-linux.rustPlatform.buildRustPackage {
          pname = "kindile-tests";
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
      };

      inherit devShells;
      inherit formatter;
    };
}
