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

        # Only the files that actually affect the build. Doc/CI/deploy-config
        # edits (README, CLAUDE.md, fly.toml, .github, …) are excluded, so they
        # don't change the derivation — and therefore the image's store hash /
        # tag stays identical, letting CI skip a redundant push + deploy.
        src = pkgs.lib.fileset.toSource {
          root = ./.;
          fileset = pkgs.lib.fileset.unions [
            ./Cargo.toml
            ./Cargo.lock
            ./src
          ];
        };

        mkQasa = p: p.rustPlatform.buildRustPackage {
          pname = "qasa-tg-notifier";
          version = "0.1.0";
          inherit src;
          cargoLock.lockFile = ./Cargo.lock;

          # reqwest's rustls stack pulls aws-lc-rs, whose -sys crate builds C
          # (cmake) and, on x86_64, assembles with nasm (perl drives some
          # codegen). These aren't in the pure build sandbox by default.
          nativeBuildInputs = with p; [ cmake perl nasm ];
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
            # rustls verifies TLS against the system trust store, which a
            # scratch image lacks; ship CA certs and point rustls at them so
            # HTTPS to api.qasa.com and api.telegram.org works.
            contents = [ linuxPkgs.cacert ];
            config = {
              Entrypoint = [ "${mkQasa linuxPkgs}/bin/qasa-tg-notifier" ];
              Env = [
                "SSL_CERT_FILE=${linuxPkgs.cacert}/etc/ssl/certs/ca-bundle.crt"
                "QASA_AREA=se/stockholm"
                "HOME_TYPES=apartment"
                "POLL_INTERVAL_HOURS=3"
              ];
            };
          };
        };

        devShells.default = pkgs.mkShell {
          packages = with pkgs; [
            rustc
            cargo
            rustfmt
            clippy
            rust-analyzer
            # Build deps for aws-lc-rs (pulled by reqwest's rustls), so a
            # clean `nix develop` builds without relying on host tools.
            cmake
            perl
            nasm
          ];

          env.RUST_SRC_PATH = "${pkgs.rustPlatform.rustLibSrc}";
        };
      });
}
