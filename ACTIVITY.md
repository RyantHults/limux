# limux activity log

## 2026-08-01 — Startup update checks + settings controls

**Feature:** On startup, limux (AppImage builds only) checks the GitHub
`releases/latest` endpoint and prompts when a newer `vX.Y.Z` release exists.

- New `app/src/update.rs`: background-thread fetch (curl, wget fallback),
  `X.Y.Z` tuple comparison against `CARGO_PKG_VERSION`, modal prompt with
  Update (xdg-open) / Not now / Skip this version.
- Settings additions (`GeneralSettings`): `check_for_updates` (default true),
  `update_check_frequency` (startup/daily/weekly, default daily),
  `skip_update_version`, `last_update_check`. All optional — backward
  compatible with existing settings files.
- Settings UI: General page gains an "Updates" section (enable switch +
  frequency dropdown, dropdown disabled while switch off).
- Startup: check scheduled 4s after launch via `glib::timeout_add_local_once`
  so it never stacks with the first-run desktop-integration prompt.
- `releases/latest` is safe despite daemon releases on the same repo: every
  daemon release ships with a main-app version bump, so the newest release is
  always a `vX.Y.Z` limux tag (safety net: non-`vX.Y.Z` tags are ignored).

**Verification:** `cargo build` clean; MVP pytest suite 7/7 pass; version
parse/compare unit-probed; dialog visually confirmed under Xvfb with a
temporary version bump (reverted afterwards).

## 2026-08-02 — Socket server fix + pane-tab CWD inheritance regression test

**Root cause 1 (main-loop freeze):** the control socket server ran a
blocking per-client handler (`reader.lines()`) on the main thread. Any client
that stayed connected froze the GTK main loop — and with it the ghostty
tick/action cycle — for the entire connection, so runtime-created terminals
never received `SET_TITLE` actions.

- Rewrote `handle_client()` in `app/src/socket.rs` as a non-blocking
  `glib::unix_fd_add_local(..., IO::IN)` source: reads into a buffer,
  processes complete `\n`-terminated lines one at a time, `ControlFlow::Break`
  on EOF, `WouldBlock` handled, clients exceeding 1 MiB buffered data dropped.

**Root cause 2 (test):** the rewritten `test_new_pane_tab_inherits_working_directory`
failed because `/tmp/limux-cwd-inherit` was never created, so ghostty's
`openDirAbsolute`/`access` checks in `embedded.zig` / `Exec.zig` failed and the
surface silently inherited the app's cwd. The `new_pane_tab()` inherit path in
`app/src/window.rs` was correct all along.

- Test now `os.makedirs(target)` before launching its dedicated instance.
- Client parser bug surfaced during diagnosis: `tests_v2/limux.py`
  `list_surfaces()` skipped `cwd=` tokens whose value contained no colon (Unix
  paths), because the `":" not in token` guard `continue`d past them. Moved the
  `cwd=`/`url=` checks above that guard.

**Verification:** MVP suite 8/8 pass, stable across repeat runs (~1.7s);
socket freeze confirmed fixed via standalone probes before cleanup. All `[dbg]`
and `[limux-boot]` instrumentation removed; `cargo build` clean.

## 2026-08-02 — CI flake in cwd-inherit test seeding, fixed

PR #7's `Build + unit tests (Rust)` check failed at the rewritten
`test_new_pane_tab_inherits_working_directory`'s seed precondition
(`tab1 never picked up cwd=`), not at the real regression assertion. Local runs
(real display and `xvfb-run -a`) were 8/8, so the failure was CI-slowness-
specific: the seed relied on a single one-shot OSC-0 title from the
`--command` shell. Under slow software-GL boot the emission can arrive before
the surface/tab is registered and the `SET_TITLE` dispatch is dropped, so tab1
never gained a `working_directory`.

- Verified in the ghostty fork that `title_changed` fires on every OSC-0
  regardless of value change (`stream_terminal.zig`), so a re-emitting loop
  re-dispatches `SET_TITLE` and the seed self-heals.
- Command now wraps the OSC-0 title in `while :; do ...; sleep 0.2; done &`
  before `exec bash`; `PWDINIT:$PWD` ground truth unchanged.
- Ruled out the "lighter" `list_surfaces`-based rewrite: tab `working_directory`
  is only set via `SET_TITLE` (`workspace::add_tab` seeds `None`), so a
  `list_surfaces` assertion on tab2 is fed by tab2's own title and would pass
  even without the `window.rs` fix (false positive). Kept the PWDINIT
  screen-scrape as the only real signal of the inherit fix.

**Verification:** 8/8 under `xvfb-run -a` (1.4s), self-wrap fallback green;
PR #7 checks all green on head `1a5ef39`.
