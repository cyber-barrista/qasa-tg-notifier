# qasa-tg-notifier

Rust binary (currently a hello-world loop) destined to run as a container on
Fly.io. Everything — toolchain, image, CI — comes from `flake.nix`; nothing is
installed on the host imperatively.

## Build & run

- `nix develop` / direnv: dev shell with rustc, cargo, clippy, rustfmt,
  rust-analyzer. `.envrc` (`use flake`) auto-loads it — `direnv allow` once.
- `nix build` / `nix run`: native (darwin) binary via `packages.default`.
- `make image TAG=<tag>` — builds the OCI image and loads it into docker as
  `qasa-tg-notifier:<tag>`. `IMAGE=` overrides the repo name (CI passes
  `registry.fly.io/...`).
- `make local` — alias producing `qasa-tg-notifier:local`.
- `make run` — `make local` + `docker compose up`.

## How the image is built

`packages.docker` is `dockerTools.buildLayeredImage` evaluated with
`linuxPkgs` — nixpkgs for the host arch with `darwin` swapped for `linux`
(no-op on Linux/CI). Non-obvious constraints, learned the hard way:

- The image must be built with **Linux** packages; with plain `pkgs` on a Mac
  it silently produces a Mach-O binary in the image → `exec format error`.
- `buildLayeredImage`, not `streamLayeredImage`: the stream variant outputs a
  Linux-only executable script, useless on a darwin host. The tarball output
  `docker load`s anywhere.
- No `tag` is set in the flake (flake outputs can't take arguments); the
  manifest tag is the store hash and the Makefile retags on load.
- On this Mac the aarch64-linux compile is dispatched to the nix-darwin
  `linux-builder` VM (a background QEMU NixOS machine registered in
  `/etc/nix/machines`). No docker needed for *building*, only for loading.

## Host environment (nix-darwin managed)

- The Mac is managed by nix-darwin + home-manager, config at
  `/etc/nix-darwin` (dendritic flake-parts layout: one feature per file under
  `modules/`). Relevant modules: `docker.nix` (colima), `direnv.nix`.
- Docker daemon is **colima** (Lima VM, started by a launchd user agent) —
  no Docker Desktop. Clients need
  `DOCKER_HOST=unix://$HOME/.colima/default/docker.sock`; home-manager sets it
  as a session variable. Long-running GUI apps (VS Code) snapshot their env at
  launch and home-manager's session vars are guarded source-once
  (`__HM_SESS_VARS_SOURCED`), so after config changes stale `DOCKER_HOST` in
  integrated terminals persists until the app fully restarts.
- colima reports image sizes via the containerd store: ~2× the layer sum
  (compressed blobs + snapshots). This image's layers total ~63 MB.

## Deploy (Fly.io)

- `.github/workflows/fly.yml`: on push to `main`, `ubuntu-latest` (x86_64 —
  same arch as Fly machines, so the nix build is native there) builds the
  image via `make image`, pushes to `registry.fly.io`, deploys with
  `flyctl deploy --image` tagged by commit SHA. Actions and flyctl are pinned
  to exact versions; bump them deliberately.
- `fly.toml` intentionally has **no `[build]` section** — the image always
  comes from CI, never from a Dockerfile (there is none in this repo).
- One-time prerequisites (not yet done): push repo to GitHub,
  `fly apps create qasa-tg-notifier`, set `FLY_API_TOKEN` repo secret from
  `fly tokens create deploy`.

## Conventions

- `flake.lock` pins everything; `Cargo.lock` must be committed (the nix build
  vendors deps from it). After changing `Cargo.toml`, build once inside the
  dev shell and commit the updated lock.
- Git identity in this repo is overridden to a personal address
  (cyber.barrista@gmail.com) in `.git/config`; the global config uses the
  work email.
