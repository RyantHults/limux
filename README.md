<p align="center">
  <img alt="limux" width="160" src="docs/assets/limux-logo.png" />
</p>

<h1 align="center">limux</h1>
<p align="center">A Ghostty-based terminal for Linux with workspaces, splits, browser panels, and a control socket for AI coding agents.</p>

---

## Status

limux is a Rust + GTK4 rewrite of [cmux](https://github.com/manaflow-ai/cmux) (macOS-only) for Linux. It ships as a self-contained AppImage with workspaces/splits, browser panels, remote SSH, a control socket, settings UI, D-Bus scripting, desktop integration, and automatic update checks. See [`ACTIVITY.md`](ACTIVITY.md) for the running progress log and [`PORT.md`](PORT.md) for the original architectural plan.

## Features

- **Workspaces** in a vertical sidebar — each workspace owns a split tree of panes, each pane owns a stack of tabs
- **Splits** — directional navigation (Alt+arrows), equalize, drag-to-reorder
- **Browser panels** — WebKitGTK 6.0, find-in-page, JS evaluation via the socket
- **Control socket** — a text protocol on `LIMUX_SOCKET` for scripting the app (see `app/src/socket.rs` for the command list)
- **D-Bus interface** — scriptable from any language that can talk D-Bus
- **Remote SSH sessions** — workspace-level `ssh` transport with a Go daemon on the remote host, browser proxy tunneling, file drop upload
- **Self-contained AppImage** — a versioned `limux-<version>-x86_64.AppImage` bundle (GTK4 + WebKitGTK 6.0 + libghostty included) that runs on any modern x86_64 Linux distro
- **Desktop integration** — first-run prompt to install the icon + menu entry, with automatic upgrades when a new version is installed and an option to clean up old AppImage versions
- **Automatic update checks** — on startup (configurable to daily/weekly), checks GitHub for newer `vX.Y.Z` releases and prompts to update
- **Settings UI** — General (auto-start, update preferences), Appearance (theme detection, including dark theme via dconf on NixOS), and terminal settings
- **Release automation** — `scripts/release.sh` bumps the version in Cargo.toml, commits, tags `vX.Y.Z`, and pushes; the app and CLI report the version via the socket, D-Bus, and About page using `CARGO_PKG_VERSION`

## Build

System dependencies (Ubuntu / Debian):

```bash
sudo apt install build-essential pkg-config libgtk-4-dev libwebkitgtk-6.0-dev \
  libgdk-pixbuf-2.0-dev libglib2.0-dev libcairo2-dev libpango1.0-dev \
  libxkbcommon-dev libx11-dev
```

Plus [Zig 0.15.1](https://ziglang.org/download/) and a recent stable Rust toolchain.

```bash
git clone --recursive git@github.com:RyantHults/limux.git
cd limux

# Build libghostty (ReleaseFast takes ~2 minutes on a modern laptop)
cd ghostty
zig build -Dapp-runtime=none -Demit-lib-vt=false -Doptimize=ReleaseFast
cd ..

# Build the app + CLI
cargo build --release

# Run
./target/release/limux
```

## Build a portable AppImage

`scripts/build-appimage.sh` packages limux + libghostty + the GTK/WebKit
runtime into a single self-contained executable that runs on any modern
x86_64 Linux distro without host packages beyond a base glibc.

```bash
# Finish the regular Build step first
scripts/build-appimage.sh
```

Output: `dist/limux-<version>-x86_64.AppImage` (e.g. `limux-0.2.3-x86_64.AppImage`,
~35 MB). On NixOS, run the script with `--use-nix` from inside `nix develop`.
The AppImage self-installs its icon and menu entry on first run and prompts to
remove outdated versions when upgrading.

## Run tests

```bash
scripts/run-tests-linux.sh
```

This launches limux with an isolated socket under `/tmp`, runs the MVP pytest suite in `tests_v2/test_linux_mvp.py`, and tears down.

## Releases

Releases are tagged `vX.Y.Z` and built automatically by GitHub Actions
(see `.github/workflows/release-appimage.yml`). Each release attaches a
versioned AppImage. To cut a release:

```bash
./scripts/release.sh [new-version]   # bumps Cargo.toml, commits, tags, pushes
```

The tag push triggers the AppImage build + GitHub Release publish. The
`limux-cli version` command, D-Bus `version()` method, and the About page all
report the same `CARGO_PKG_VERSION` so they stay in sync with the tag.

## Layout

```
app/               # Rust + GTK4 GUI binary
cli/               # Rust CLI binary (limux-cli)
ghostty/           # submodule — thin fork for Linux embedded-apprt support
daemon/remote/     # Go limuxd-remote daemon (runs on remote hosts for SSH sessions)
web/               # Next.js portal (marketing site)
tests_v2/          # Python socket integration tests (MVP set adapted for Linux)
docs/              # design docs, ghostty fork notes
```

## License

GPL-3.0-or-later. See [`LICENSE`](LICENSE) and [`THIRD_PARTY_LICENSES.md`](THIRD_PARTY_LICENSES.md).
