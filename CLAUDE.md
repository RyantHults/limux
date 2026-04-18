# limux agent notes

This is a Rust + GTK4 terminal app for Linux. Roughly 15K lines of Rust across
`app/` (the GUI binary) and `cli/` (the `limux-cli` CLI). libghostty provides
the terminal emulation + OpenGL rendering, WebKitGTK provides browser panels.

## Build & run

```bash
# First-time setup
cd ghostty
zig build -Doptimize=ReleaseFast
cd ..

# Normal dev loop
cargo build           # debug
cargo run             # launch debug build
cargo build --release # release
```

**Always rebuild after changes** — `cargo check` is faster but misses link
errors from the libghostty FFI and cc-compiled GLAD.

libghostty gets linked from `ghostty/zig-out/lib` by default. Override with
`GHOSTTY_LIB=/path/to/lib cargo build` if you have a prebuilt copy. See
`app/build.rs` for the logic.

## Ghostty submodule

The `ghostty/` submodule is a thin fork at
`github.com/RyantHults/ghostty`, `limux` branch, forked from
`ghostty-org/ghostty`. It carries one commit: Linux embedded-apprt support
(CAPI exports for GL context (un)realize, `displayRealized` unblock for the
embedded apprt, Linux variant on the `Platform` union). `docs/ghostty-fork.md`
describes the patches.

To sync with upstream:

```bash
cd ghostty
git fetch upstream
git rebase upstream/main         # resolve any conflicts, rebuild, test
git push --force-with-lease origin limux
cd ..
git add ghostty
git commit -m "Update ghostty submodule"
```

Always push the submodule commit to `origin limux` *before* committing the
parent-repo pointer — otherwise the parent references a commit that only
exists locally.

## Control socket

The app listens on a Unix socket at `/tmp/limux-<pid>.sock` by default (or
whatever `--socket <path>` specifies). The path is exported to the env as
`LIMUX_SOCKET` so child shells and the CLI can connect. See
`app/src/socket.rs` for the command list and `tests_v2/limux.py` for a
reference client.

Protocol is line-based:

```
request:   "<cmd> <args>\n"
response:  "OK [payload]\n"                    (single-line)
           "ERROR <message>\n"                 (error)
           "OK+<byte_count>\n<raw bytes>"      (length-prefixed, no trailing newline)
```

## Tests

```bash
scripts/run-tests-linux.sh
```

Spawns limux with an isolated socket under `/tmp`, waits for it, runs the
MVP pytest suite in `tests_v2/test_linux_mvp.py`, tears everything down.
Uses `xvfb-run` if no display is available.

The MVP set covers workspace CRUD, panes/splits, terminal send/read, and the
length-prefixed protocol. Broader coverage ported from the predecessor's v2
JSON-RPC test suite is follow-up.

## Remote SSH

The app can host "remote workspaces" that run shells on a remote host over
SSH. The Go daemon in `daemon/remote/cmd/limuxd-remote/` is shipped to the
remote host at bootstrap time; the app fetches a signed manifest +
per-`(GOOS, GOARCH)` binaries from this repo's GitHub Releases (tag shape
`limuxd-remote-vX.Y.Z`). See `app/src/remote/bootstrap.rs` for the URL
template (look for the `releases/download/limuxd-remote-v{}` format string).

To cut a daemon release:

```bash
git tag limuxd-remote-v0.1.0
git push origin limuxd-remote-v0.1.0
```

The `release-daemon.yml` workflow builds + publishes.

## Layout

```
app/                    # Rust + GTK4 GUI (~14K LoC)
  src/
    app.rs              # GhosttyApp init / libghostty config
    surface.rs          # GLArea wrapper around a ghostty surface
    window.rs           # top-level window, workspace/pane/tab management
    workspace.rs        # data model (Workspace -> Pane -> Tab)
    split.rs            # split tree (binary tree of Leaf/Split nodes)
    sidebar.rs          # vertical workspace sidebar
    browser.rs          # WebKitGTK browser panels
    remote/             # SSH bootstrap, JSON-RPC, proxy broker, CLI relay
    socket.rs           # control socket server
    dbus.rs             # D-Bus scripting interface
    settings.rs         # persisted user settings
    ...
cli/                    # Rust CLI (limux-cli binary, ~600 LoC)
  src/
    commands/           # workspace, browser, terminal, metadata
ghostty/                # submodule (thin Linux fork)
daemon/remote/          # Go limuxd-remote source
web/                    # Next.js marketing/docs site
tests_v2/               # Python socket integration tests
docs/                   # architecture + fork notes
scripts/                # Linux build + test runner + daemon release helper
```

## Pitfalls

- **Typing latency** — keystroke handlers in `app/src/surface.rs` and
  `app/src/window.rs` run on every key event. Avoid allocations, file I/O,
  or formatting in those paths.
- **GL context loss on reparenting** — GTK destroys/re-creates the
  `GdkGLContext` when a widget moves. The ghostty fork exposes
  `ghostty_surface_display_unrealized` / `_realized` to let us deinit + reinit
  GPU resources while preserving terminal state. See `app/src/surface.rs`
  for the hookup.
- **User-facing strings** should eventually be localized, but we don't have
  the infrastructure yet. For now: plain English string literals are fine.
- **Main thread only for GL** — OpenGL contexts are thread-affine under GTK.
  The renderer draws from the app's main thread via the render callback.

## Workflow

- Keep `ACTIVITY.md` updated after each substantial change (decisions,
  issues, stats). Prepend new sections at the end.
- Prefer splitting one logical change per commit. Don't squash unrelated
  work.
- After making code changes, always run `cargo build` (not just
  `cargo check`) before claiming success — cc-compiled GLAD and the
  libghostty FFI only surface errors at link time.
