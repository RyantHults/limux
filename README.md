<h1 align="center">limux</h1>
<p align="center">A Ghostty-based terminal for Linux with workspaces, splits, browser panels, and a control socket for AI coding agents.</p>

---

## Status

limux is a Rust + GTK4 rewrite of [cmux](https://github.com/manaflow-ai/cmux) (macOS-only) for Linux. Phases 1–4 of the port are complete; Phase 5 (remote SSH, settings UI, D-Bus scripting) is in place with packaging and auto-update still pending. See [`ACTIVITY.md`](ACTIVITY.md) for the running progress log and [`PORT.md`](PORT.md) for the original architectural plan.

## Features

- **Workspaces** in a vertical sidebar — each workspace owns a split tree of panes, each pane owns a stack of tabs
- **Splits** — directional navigation (Alt+arrows), equalize, drag-to-reorder
- **Browser panels** — WebKitGTK 6.0, find-in-page, JS evaluation via the socket
- **Control socket** — a text protocol on `LIMUX_SOCKET` for scripting the app (see `app/src/socket.rs` for the command list)
- **D-Bus interface** — scriptable from any language that can talk D-Bus
- **Remote SSH sessions** — workspace-level `ssh` transport with a Go daemon on the remote host, browser proxy tunneling, file drop upload

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
zig build -Doptimize=ReleaseFast
cd ..

# Build the app + CLI
cargo build --release

# Run
./target/release/limux
```

## Run tests

```bash
scripts/run-tests-linux.sh
```

This launches limux with an isolated socket under `/tmp`, runs the MVP pytest suite in `tests_v2/test_linux_mvp.py`, and tears down.

## Layout

```
app/               # Rust + GTK4 GUI binary
cli/               # Rust CLI binary (limux-cli)
ghostty/           # submodule — thin fork for Linux embedded-apprt support
daemon/remote/     # Go cmuxd-remote daemon (runs on remote hosts for SSH sessions)
web/               # Next.js portal (marketing site)
tests_v2/          # Python socket integration tests (MVP set adapted for Linux)
docs/              # design docs, ghostty fork notes
```

## License

GPL-3.0-or-later. See [`LICENSE`](LICENSE) and [`THIRD_PARTY_LICENSES.md`](THIRD_PARTY_LICENSES.md).
