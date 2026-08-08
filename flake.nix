{
  description = "qasa-tg-notifier";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-26.05";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs = { self, nixpkgs, flake-utils }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        pkgs = nixpkgs.legacyPackages.${system};

        # The image must contain Linux binaries: on darwin, target the
        # matching Linux arch (built by the nix-darwin linux-builder VM).
        linuxPkgs = nixpkgs.legacyPackages.${
          builtins.replaceStrings [ "darwin" ] [ "linux" ] system
        };

        mkQasa = p: p.rustPlatform.buildRustPackage {
          pname = "qasa-tg-notifier";
          version = "0.1.0";
          src = ./.;
          cargoLock.lockFile = ./Cargo.lock;
        };
      in
      {
        packages = {
          default = mkQasa pkgs;

          # OCI image tarball (buildLayeredImage, not stream*: the stream
          # script is a Linux executable, useless on the darwin host — the
          # tarball loads anywhere). No fixed tag: the manifest tag is the
          # store hash; `make image TAG=...` applies the real tag on load.
          docker = linuxPkgs.dockerTools.buildLayeredImage {
            name = "qasa-tg-notifier";
            config.Entrypoint = [ "${mkQasa linuxPkgs}/bin/qasa-tg-notifier" ];
          };
        };

        devShells.default = pkgs.mkShell {
          packages = with pkgs; [
            rustc
            cargo
            rustfmt
            clippy
            rust-analyzer
          ];

          env.RUST_SRC_PATH = "${pkgs.rustPlatform.rustLibSrc}";
        };
      });
}
