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
