# Ghostty Fork Changes (RyantHults/ghostty)

limux uses a thin fork of Ghostty for Linux embedded-apprt support that hasn't
been upstreamed. The fork lives at `https://github.com/RyantHults/ghostty` on
the `limux` branch. The submodule in this repo tracks that branch.

## Fork update checklist

1. Make changes in `ghostty/` on the `limux` branch.
2. Commit and push to `origin limux` (the fork).
3. Update this file with the new change summary.
4. In the parent repo: `git add ghostty` and commit the updated submodule SHA.

## Syncing with upstream

```bash
cd ghostty
git fetch upstream
git rebase upstream/main   # resolve conflicts, test, then:
git push --force-with-lease origin limux
cd ..
git add ghostty && git commit -m "Update ghostty submodule"
```

## Current patches

Forked from `ghostty-org/ghostty` (upstream), Linux embedded-apprt patches on
top. Single commit: `add Linux embedded-apprt support`.

### Linux embedded-apprt support

**Files:**
- `include/ghostty.h`
- `src/apprt/embedded.zig`
- `src/renderer/OpenGL.zig`

**What it does:**

Lets libghostty be embedded in a Linux host app (GTK4 in our case) via the
embedded apprt, which previously assumed macOS/iOS only.

- **`include/ghostty.h`** — C declarations for the GL context reinit API pair
  `ghostty_surface_display_unrealized` / `ghostty_surface_display_realized`.
  Host apps call these when the underlying GL context is destroyed and
  re-created (e.g. GTK widget reparenting destroys and re-creates the
  `GdkGLContext` along with it, invalidating every shader, texture, FBO).
- **`src/apprt/embedded.zig`** — adds a `Linux` variant to the `Platform`
  union and `PlatformTag` enum so host apps can identify themselves as Linux
  without going through the macOS/iOS code paths. Exposes
  `App.must_draw_from_app_thread = true` on Linux because OpenGL contexts
  are thread-affine under GTK4. Exports the two CAPI bindings for the GL
  reinit cycle, which forward to the renderer's existing
  `displayUnrealized` / `displayRealized` methods. Also drops an unreachable
  `return` from `Surface.draw`.
- **`src/renderer/OpenGL.zig`** — on `surfaceInit` for the embedded apprt,
  load GL functions via `prepareContext(null)` the same way GTK does (the
  upstream code had a TODO stub that left libghostty unable to render under
  embedded runtimes). Also broadens the `displayRealized` switch to accept
  `apprt.embedded`, not just `apprt.gtk`.

**Conflict expectations on rebase:** low. The changes are surgical and live
in code that upstream touches infrequently. The most likely sites for
conflicts on future upstream rebases are:

- `include/ghostty.h` around `ghostty_surface_refresh` / `ghostty_surface_draw`
  (upstream sometimes re-decorates API macros here).
- `src/apprt/embedded.zig` around the Platform/PlatformTag definitions
  (upstream may add new platforms or rename the existing ones).
- `src/renderer/OpenGL.zig` around the `apprt.embedded` branches
  (upstream may revisit the stub pattern).

If upstream lands native Linux support for the embedded apprt, these patches
can likely be dropped wholesale. Until then, keep them minimal.
