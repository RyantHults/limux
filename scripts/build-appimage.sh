#!/usr/bin/env bash
# Build a self-contained AppImage for limux.
#
# Output: dist/limux-x86_64.AppImage
#
# What this does:
#   1. Verifies the prebuilt artifacts (target/release/limux, etc.)
#   2. Uses nix to get the runtime closure (gtk4, webkitgtk-6.0, glib, ...)
#   3. Stages an AppDir matching the AppImage spec
#   4. Downloads the official appimagetool from AppImageKit
#   5. Runs appimagetool to produce a real Type 2 AppImage
#
# The result is a single .AppImage file that runs on any modern Linux
# distro (Ubuntu, Fedora, Arch, etc.) with no nix install required.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

# --- Sanity checks ---

if [[ ! -x target/release/limux ]] || [[ ! -x target/release/limux-cli ]]; then
    echo "ERROR: prebuilt binaries missing."
    echo "  Run: cargo build --release"
    exit 1
fi

if [[ ! -f ghostty/zig-out/lib/ghostty-internal.so ]]; then
    echo "ERROR: libghostty missing."
    echo "  Run: cd ghostty && zig build -Dapp-runtime=none -Demit-lib-vt=false -Doptimize=ReleaseFast && cd .."
    exit 1
fi

# --- Step 1: build the AppDir ---

echo ">>> Staging AppDir..."
DIST_DIR="$REPO_ROOT/dist"
APPDIR="$(mktemp -d -t limux-appdir-XXXXXX)"
APPIMAGETOOL_DIR="$(mktemp -d -t appimagetool-XXXXXX)"

# The AppDir contains files copied from the nix store with their
# original (read-only) permissions. Standard rm can't remove them.
# Make them writable before cleanup. We swallow errors with || true
# so a partial failure doesn't mask a successful build.
cleanup() {
    if [[ -n "$APPDIR" && -d "$APPDIR" ]]; then
        chmod -R u+w "$APPDIR" 2>/dev/null || true
        rm -rf "$APPDIR" 2>/dev/null || true
    fi
    if [[ -n "$APPIMAGETOOL_DIR" && -d "$APPIMAGETOOL_DIR" ]]; then
        rm -rf "$APPIMAGETOOL_DIR" 2>/dev/null || true
    fi
}
trap cleanup EXIT

mkdir -p "$APPDIR/usr/bin"
mkdir -p "$APPDIR/usr/lib"
mkdir -p "$APPDIR/usr/share/applications"
mkdir -p "$APPDIR/usr/share/icons/hicolor/scalable/apps"

# Binaries
cp target/release/limux     "$APPDIR/usr/bin/limux"
cp target/release/limux-cli "$APPDIR/usr/bin/limux-cli"

# libghostty (zig emits ghostty-internal.{so,a}; the binary's SONAME
# is libghostty.so, so we add the symlink for that too)
cp -P ghostty/zig-out/lib/ghostty-internal.so "$APPDIR/usr/lib/"
cp -P ghostty/zig-out/lib/ghostty-internal.a  "$APPDIR/usr/lib/"
ln -sf ghostty-internal.so "$APPDIR/usr/lib/libghostty-internal.so"
ln -sf ghostty-internal.so "$APPDIR/usr/lib/libghostty.so"

# Desktop entry + icon. appimagetool looks for limux.{png,svg,xpm}
# in the AppDir root, so we stage limux.svg there and in the hicolor
# theme location. We then convert the SVG to a real 256x256 PNG for
# limux.png and .DirIcon so file managers and the freedesktop
# thumbnailer can decode them (previously we renamed SVG→PNG, which
# caused generic fallback icons in many file managers).
cp packaging/AppImage/limux.desktop "$APPDIR/limux.desktop"
cp packaging/AppImage/limux.svg     "$APPDIR/limux.svg"
cp packaging/AppImage/limux.svg     "$APPDIR/usr/share/icons/hicolor/scalable/apps/limux.svg"

# Convert the SVG to a real 256x256 PNG so file managers and the
# freedesktop thumbnailer can decode it. The build script previously
# copied the SVG to limux.png as a workaround, but file managers try
# to decode it as PNG and fall back to a generic icon.
if command -v rsvg-convert >/dev/null 2>&1; then
    rsvg-convert -w 256 -h 256 "$APPDIR/limux.svg" > "$APPDIR/limux.png"
    rsvg-convert -w 256 -h 256 "$APPDIR/limux.svg" > "$APPDIR/.DirIcon"
elif command -v inkscape >/dev/null 2>&1; then
    inkscape "$APPDIR/limux.svg" --export-type=png --export-filename="$APPDIR/limux.png" -w 256 -h 256
    cp "$APPDIR/limux.png" "$APPDIR/.DirIcon"
else
    # Fallback to the SVG-as-PNG trick (will show generic icon in some
    # file managers, but the rest of the integration still works)
    cp "$APPDIR/limux.svg" "$APPDIR/limux.png"
    cp "$APPDIR/limux.svg" "$APPDIR/.DirIcon"
    echo "WARNING: neither rsvg-convert nor inkscape found; limux.png and .DirIcon are SVG renamed to PNG"
fi

# Copy to XDG_DATA_DIRS-discoverable location for the app menu
cp packaging/AppImage/limux.desktop "$APPDIR/usr/share/applications/limux.desktop"
# Also copy under the application ID so Wayland's xdg-shell app_id matching works
cp packaging/AppImage/limux.desktop "$APPDIR/usr/share/applications/com.limux.terminal.desktop"

# --- Step 2: bundle the nix runtime closure ---

echo ">>> Computing runtime closure via nix..."
RUNTIME_LIB_DIR=$(nix-build --no-out-link -E '
  let
    pkgs = import <nixpkgs> {};
  in
  pkgs.symlinkJoin {
    name = "limux-runtime-libs";
    # Use .out for multi-output packages (e.g. glib, pango, openssl) —
    # the bare attribute is the .bin wrapper, which doesn'\''t contain
    # the .so files. Single-output packages (gtk4, cairo, graphene, ...)
    # can be referenced directly.
    paths = [
      # GTK4 stack
      pkgs.gtk4
      pkgs.glib.out
      pkgs.gsettings-desktop-schemas
      pkgs.gtk-layer-shell
      pkgs.gobject-introspection
      pkgs.gdk-pixbuf
      pkgs.hicolor-icon-theme
      pkgs.cairo
      pkgs.pango.out
      # WebKitGTK 6
      pkgs.webkitgtk_6_0
      pkgs.libsoup_3
      pkgs.libsecret
      pkgs.glib-networking
      # Graphics / GL
      pkgs.libglvnd
      pkgs.mesa
      pkgs.libgbm
      pkgs.libepoxy
      pkgs.graphene
      # X11 / Wayland
      pkgs.libxkbcommon
      pkgs.libX11
      pkgs.libXi
      pkgs.libxcursor
      pkgs.libxinerama
      pkgs.libxrandr
      pkgs.wayland
      # Text
      pkgs.freetype
      pkgs.harfbuzz
      pkgs.fontconfig.out
      pkgs.libxml2.out
      # Misc
      pkgs.openssl.out
      pkgs.shared-mime-info
      pkgs.desktop-file-utils
    ];
  }
')

echo ">>> Copying .so files from $RUNTIME_LIB_DIR..."
# Preserve symlinks (libfoo.so.1 -> libfoo.so.1.2.3 etc.) so the
# dynamic linker can find versioned SONAMEs.
cp -aP "$RUNTIME_LIB_DIR"/lib/*.so* "$APPDIR/usr/lib/" 2>/dev/null || true
cp -aP "$RUNTIME_LIB_DIR"/lib/*.so "$APPDIR/usr/lib/" 2>/dev/null || true

# GIO modules (for gobject-introspection, gvfs, etc.)
if [[ -d "$RUNTIME_LIB_DIR/lib/gio" ]]; then
    cp -a "$RUNTIME_LIB_DIR/lib/gio" "$APPDIR/usr/lib/"
fi

# GDK pixbuf loaders
if [[ -d "$RUNTIME_LIB_DIR/lib/gdk-pixbuf-2.0" ]]; then
    cp -a "$RUNTIME_LIB_DIR/lib/gdk-pixbuf-2.0" "$APPDIR/usr/lib/"
fi

# GTK-4.0 modules (input methods, etc.)
if [[ -d "$RUNTIME_LIB_DIR/lib/gtk-4.0" ]]; then
    cp -a "$RUNTIME_LIB_DIR/lib/gtk-4.0" "$APPDIR/usr/lib/"
fi

# MIME data (XDG_DATA_DIRS) — optional but helps
if [[ -d "$RUNTIME_LIB_DIR/share" ]]; then
    cp -a "$RUNTIME_LIB_DIR/share" "$APPDIR/usr/"
fi

# GSettings discovers schemas at $XDG_DATA_DIRS/glib-2.0/schemas/.
# Nix stores them in versioned subdirs (gsettings-schemas/<pkg>-<ver>/...)
# that GSettings cannot find via the standard XDG_DATA_DIRS lookup.
# Flatten them to a discoverable path and compile the binary cache.
#
# Note: nix store files are read-only (0444), and cp -a preserves
# permissions. Make the share tree writable so we can mkdir and copy.
chmod -R u+w "$APPDIR/usr/share" 2>/dev/null || true
if [[ -d "$APPDIR/usr/share/gsettings-schemas" ]]; then
    mkdir -p "$APPDIR/usr/share/glib-2.0/schemas"
    find "$APPDIR/usr/share/gsettings-schemas" -name '*.gschema.xml' \
        -exec cp -aL {} "$APPDIR/usr/share/glib-2.0/schemas/" \;
    if command -v glib-compile-schemas >/dev/null 2>&1; then
        glib-compile-schemas "$APPDIR/usr/share/glib-2.0/schemas/"
    fi
fi

# --- Stage GTK4 Adwaita theme files on disk ---
# GTK4's Adwaita theme is compiled into libgtk-4.so as GResources, not
# shipped as files. Without on-disk theme directories, GTK may discover
# system themes that lack the dark variant, causing a light fallback
# when gtk-application-prefer-dark-theme is set. Extract the wrapper
# CSS from the GResource so the AppImage ships its own Adwaita themes.
if [[ -x "$(command -v gresource)" ]]; then
    # -type f misses symlinks; also match symlinks so gresource can
    # extract the wrapper CSS from the library's GResources.
    GTK4_LIB=$(find "$APPDIR/usr/lib" -name 'libgtk-4.so*' \( -type f -o -type l \) 2>/dev/null | head -1)
    if [[ -n "$GTK4_LIB" ]]; then
        THEME_BASE="$APPDIR/usr/share/themes"
        mkdir -p "$THEME_BASE/Adwaita/gtk-4.0"
        mkdir -p "$THEME_BASE/Adwaita-dark/gtk-4.0"

        # Extract wrapper CSS files from the GResource. These are thin
        # files (599 B) that @import the main CSS from GResources at
        # runtime, so GTK can use them to discover the theme on disk
        # while still loading the actual styles from the library.
        gresource extract "$GTK4_LIB" \
            /org/gtk/libgtk/theme/Default/gtk.css \
            > "$THEME_BASE/Adwaita/gtk-4.0/gtk.css" 2>/dev/null || true
        gresource extract "$GTK4_LIB" \
            /org/gtk/libgtk/theme/Default/gtk-dark.css \
            > "$THEME_BASE/Adwaita-dark/gtk-4.0/gtk-dark.css" 2>/dev/null || true
        # Adwaita-dark's light-mode CSS wrapper (for completeness)
        gresource extract "$GTK4_LIB" \
            /org/gtk/libgtk/theme/Default/gtk-dark.css \
            > "$THEME_BASE/Adwaita-dark/gtk-4.0/gtk.css" 2>/dev/null || true

        # index.theme files for theme discovery
        cat > "$THEME_BASE/Adwaita/index.theme" << 'THEME_EOF'
[Desktop Entry]
Type=Application
Name=Adwaita

[GTK Theme]
Name=Adwaita
Type=GTK4
THEME_EOF

        cat > "$THEME_BASE/Adwaita-dark/index.theme" << 'THEME_EOF'
[Desktop Entry]
Type=Application
Name=Adwaita Dark

[GTK Theme]
Name=Adwaita-dark
Type=GTK4
THEME_EOF
    fi
fi

# --- Step 3: rpath + interpreter fixup on the binaries ---

# The prebuilt binaries were linked in the nix dev shell, so both their
# rpath AND their ELF interpreter (the ld-linux path) point into
# /nix/store/.../glibc-2.42-67/... — paths that don't exist on a
# non-nix system. We need to reset BOTH:
#   - interpreter → /lib64/ld-linux-x86-64.so.2 (the standard FHS path
#     that exists on every Linux distro, including the AppImage's
#     squashed /lib64 at runtime)
#   - rpath → $ORIGIN/../lib so the linker finds our bundled libs
#     relative to the AppDir (resolves to AppDir/usr/lib)
echo ">>> Patching binary rpaths and interpreters..."
if command -v patchelf >/dev/null 2>&1; then
    for bin in "$APPDIR/usr/bin/limux" "$APPDIR/usr/bin/limux-cli"; do
        patchelf --set-interpreter '/lib64/ld-linux-x86-64.so.2' "$bin"
        patchelf --set-rpath '$ORIGIN/../lib' "$bin"
    done
else
    echo "ERROR: patchelf not found. Install it (or run this script"
    echo "  inside 'nix develop') to fix the binaries' rpath and"
    echo "  interpreter. The AppImage will not work on non-nix systems"
    echo "  without this fix."
    exit 1
fi

# --- Step 4: AppRun script ---

# AppRun is invoked by the AppImage runtime. It sets up the env and
# execs the main binary. The runtime sets APPDIR to the AppDir path,
# but the actual binaries live in usr/bin/ per the FHS layout.
cat > "$APPDIR/AppRun" <<'EOF'
#!/usr/bin/env bash
# AppRun — entry point for the AppImage runtime.
set -e

APPDIR="$(cd "$(dirname "$0")" && pwd)"

# Self-install: copy the icon and desktop file to the user's XDG
# locations so the host desktop environment (GNOME Shell, KDE, etc.)
# can see the app in the application menu and Alt+Tab switcher. The
# AppImage's bundled resources live inside the mount and are invisible
# to the host desktop without this step. Use this on first run, or
# whenever the icon/desktop file changes.
if [[ "${1:-}" == "--install" ]]; then
    set +e  # best-effort install
    echo ">>> Installing limux to ~/.local/share/..."

    ICON_SRC="$APPDIR/usr/share/icons/hicolor/scalable/apps/limux.svg"
    HICOLOR_INDEX_SRC="$APPDIR/usr/share/icons/hicolor/index.theme"
    DESKTOP_SRC="$APPDIR/usr/share/applications/limux.desktop"
    WAYLAND_DESKTOP_SRC="$APPDIR/usr/share/applications/com.limux.terminal.desktop"

    HICOLOR_DST_DIR="$HOME/.local/share/icons/hicolor"
    ICON_DST_DIR="$HICOLOR_DST_DIR/scalable/apps"
    DESKTOP_DST_DIR="$HOME/.local/share/applications"

    mkdir -p "$ICON_DST_DIR" "$DESKTOP_DST_DIR"

    # Copy the icon SVG
    if [[ -f "$ICON_SRC" ]]; then
        cp "$ICON_SRC" "$ICON_DST_DIR/limux.svg" && echo "  icon: $ICON_DST_DIR/limux.svg"
    else
        echo "  WARNING: icon source not found at $ICON_SRC"
    fi

    # Copy the hicolor index.theme — without it GTK4's IconTheme engine
    # treats the user-local hicolor directory as invalid and skips it.
    if [[ -f "$HICOLOR_INDEX_SRC" ]]; then
        cp "$HICOLOR_INDEX_SRC" "$HICOLOR_DST_DIR/index.theme" && echo "  hicolor index: $HICOLOR_DST_DIR/index.theme"
    else
        echo "  WARNING: hicolor index.theme not found at $HICOLOR_INDEX_SRC"
    fi

    # Determine the actual AppImage path. $APPIMAGE is set by the AppImage
    # runtime; for plain FUSE mounts (e.g. via `appimage-run`), find it via
    # /proc/self/mountinfo by matching on this AppRun's path.
    APPIMAGE_PATH="${APPIMAGE:-}"
    if [[ -z "$APPIMAGE_PATH" && -r /proc/self/mountinfo ]]; then
        # Look for a mount whose target contains the AppRun's directory
        while IFS= read -r line; do
            # mountinfo format: parent_major:minor mount_parent ... mount_point -
            mount_point=$(echo "$line" | awk '{print $5}')
            if [[ "$mount_point" == "$APPDIR" ]]; then
                # Found the mount — the source is in the next field after '-'
                # Format: ... mount_point - fs_type source super_options
                source=$(echo "$line" | awk '{for(i=1;i<=NF;i++) if($i=="-"){print $(i+2); exit}}')
                if [[ -n "$source" && -f "$source" ]]; then
                    APPIMAGE_PATH="$source"
                    break
                fi
            fi
        done < /proc/self/mountinfo
    fi

    # Helper: rewrite Exec= in a desktop file to use the AppImage path
    rewrite_exec() {
        local file="$1"
        if [[ -n "$APPIMAGE_PATH" && -f "$file" ]]; then
            # Use a delimiter that's unlikely to appear in paths
            sed -i "s|^Exec=limux|Exec=\"$APPIMAGE_PATH\"|" "$file"
            echo "  rewrote Exec= in $file"
        fi
    }

    # Copy desktop files (after rewrites)
    if [[ -f "$DESKTOP_SRC" ]]; then
        cp "$DESKTOP_SRC" "$DESKTOP_DST_DIR/limux.desktop" && \
            rewrite_exec "$DESKTOP_DST_DIR/limux.desktop" && \
            echo "  desktop: $DESKTOP_DST_DIR/limux.desktop"
    fi
    if [[ -f "$WAYLAND_DESKTOP_SRC" ]]; then
        cp "$WAYLAND_DESKTOP_SRC" "$DESKTOP_DST_DIR/com.limux.terminal.desktop" && \
            rewrite_exec "$DESKTOP_DST_DIR/com.limux.terminal.desktop" && \
            echo "  desktop (wayland): $DESKTOP_DST_DIR/com.limux.terminal.desktop"
    fi

    if [[ -z "$APPIMAGE_PATH" ]]; then
        echo "  WARNING: could not determine AppImage path; menu entries will use bare 'limux' command"
    fi

    # Refresh host-side caches
    if command -v update-desktop-database >/dev/null 2>&1; then
        update-desktop-database "$DESKTOP_DST_DIR" && echo "  refreshed desktop database"
    fi
    if command -v gtk-update-icon-cache >/dev/null 2>&1; then
        # Remove stale cache first so it gets rebuilt fresh
        rm -f "$HICOLOR_DST_DIR/icon-theme.cache"
        gtk-update-icon-cache -f -t "$HICOLOR_DST_DIR" 2>/dev/null && echo "  refreshed icon cache"
    fi

    echo ">>> Done. limux should now appear in your app menu and Alt+Tab."
    echo ">>> You can launch it normally with: $APPDIR/AppRun"
    exit 0
fi

# Make all the bundled libraries findable. The prebuilt binaries have
# rpath=$ORIGIN/../lib which resolves to $APPDIR/usr/lib, but we also
# add $APPDIR/lib and LD_LIBRARY_PATH for any subprocesses that
# ignore rpath (e.g., dlopen calls by WebKit's loader).
export LD_LIBRARY_PATH="$APPDIR/usr/lib:$APPDIR/lib:${LD_LIBRARY_PATH:-}"

# GTK/GIO look for data files (schemas, pixbuf loaders, mime info)
# in $XDG_DATA_DIRS. Ship the relevant ones in usr/share and prepend.
export XDG_DATA_DIRS="$APPDIR/usr/share:${XDG_DATA_DIRS:-/usr/local/share:/usr/share}"

# GIO modules (gobject-introspection, etc.) — also helps the runtime
# find libgimme-what-it-needs in non-standard locations.
export GIO_EXTRA_MODULES="$APPDIR/usr/lib/gio/modules:${GIO_EXTRA_MODULES:-}"

# Make the control socket predictable if the user runs the CLI from
# the same shell. The AppImage runtime sets $XDG_RUNTIME_DIR.
if [[ -z "${LIMUX_SOCKET:-}" && -n "${XDG_RUNTIME_DIR:-}" ]]; then
    export LIMUX_SOCKET="$XDG_RUNTIME_DIR/limux.sock"
fi

exec "$APPDIR/usr/bin/limux" "$@"
EOF
chmod +x "$APPDIR/AppRun"

# --- Step 5: appimagetool ---

APPIMAGETOOL_VERSION="13"

if [[ ! -x "$APPIMAGETOOL_DIR/appimagetool.AppImage" ]]; then
    echo ">>> Downloading appimagetool..."
    # The current official binary is published as "obsolete-appimagetool-..."
    # (the project renamed but kept the prefix for stable URLs).
    curl -fsSL \
        "https://github.com/AppImage/AppImageKit/releases/download/${APPIMAGETOOL_VERSION}/obsolete-appimagetool-x86_64.AppImage" \
        -o "$APPIMAGETOOL_DIR/appimagetool.AppImage"
    chmod +x "$APPIMAGETOOL_DIR/appimagetool.AppImage"

    # Extract appimagetool using unsquashfs. The newer appimagetool
    # runtime doesn't honor --appimage-extract, so we just unpack the
    # squashfs directly. We need the offset of the squashfs, which
    # the standard appimage-exec.sh script computes; for simplicity we
    # let appimagetool run itself (it'll extract to its own cache
    # and we invoke the cached binary).
    echo ">>> Extracting appimagetool..."
    mkdir -p "$APPIMAGETOOL_DIR/extracted"
    (cd "$APPIMAGETOOL_DIR/extracted" && \
        OFFSET=$(awk 'NR==13{e_shoff=$5} NR==18{e_shentsize=$5} NR==19{e_shnum=$5} END{print e_shoff+e_shentsize*e_shnum}' <(LC_ALL=C readelf -h "$APPIMAGETOOL_DIR/appimagetool.AppImage")) && \
        unsquashfs -q -o "$OFFSET" -d "$APPIMAGETOOL_DIR/squashfs-root" \
            "$APPIMAGETOOL_DIR/appimagetool.AppImage")
fi

APPIMAGETOOL_EXTRACTED="$APPIMAGETOOL_DIR/squashfs-root/usr/bin/appimagetool"
if [[ ! -x "$APPIMAGETOOL_EXTRACTED" ]]; then
    echo "ERROR: failed to extract appimagetool"
    exit 1
fi

# Patch the appimagetool binary so it runs on NixOS. The official
# appimagetool is built against generic Linux (glibc at /lib64/...,
# dynamic linker at /lib64/ld-linux-x86-64.so.2). NixOS doesn't have
# those paths — its dynamic linker lives in the nix store. Use patchelf
# to point the appimagetool binary at the nix dynamic linker and adjust
# the rpath so it finds the bundled libs (and nix's libstdc++, etc.).
# We rely on the nix dev shell having `patchelf` available.
if command -v patchelf >/dev/null 2>&1; then
    echo ">>> Patching appimagetool for NixOS..."
    PATCHED_INTERP=$(nix-instantiate --eval --raw -E 'with import <nixpkgs> {}; toString pkgs.stdenv.cc.bintools.dynamicLinker')
    PATCHED_LIBPATH=$(nix-build --no-out-link -E '
      let
        pkgs = import <nixpkgs> {};
      in
      pkgs.symlinkJoin {
        name = "appimagetool-libs";
        paths = [
          pkgs.stdenv.cc.cc.lib
          pkgs.glib.out
          pkgs.zlib.out
          pkgs.openssl.out
          pkgs.libarchive
        ];
      }
    ')
    # Patchelf the main appimagetool binary.
    patchelf \
        --set-interpreter "$PATCHED_INTERP" \
        --set-rpath "$PATCHED_LIBPATH/lib" \
        "$APPIMAGETOOL_EXTRACTED" 2>/dev/null || \
        echo "WARNING: patchelf failed on appimagetool"

    # Patchelf the bundled mksquashfs (and any other ELF binaries
    # in usr/lib/appimagekit/) so they can also run on NixOS. The
    # bundled mksquashfs is dynamically linked and would otherwise
    # fail with the "stub-ld" NixOS message.
    if [[ -d "$APPIMAGETOOL_DIR/squashfs-root/usr/lib/appimagekit" ]]; then
        for elf in "$APPIMAGETOOL_DIR/squashfs-root/usr/lib/appimagekit/"*; do
            [[ -f "$elf" ]] || continue
            if file "$elf" 2>/dev/null | grep -q 'ELF.*executable'; then
                patchelf \
                    --set-interpreter "$PATCHED_INTERP" \
                    --set-rpath "$PATCHED_LIBPATH/lib" \
                    "$elf" 2>/dev/null || true
            fi
        done
    fi
else
    echo "WARNING: patchelf not found; appimagetool may not run on NixOS"
fi

# --- Step 6: build the AppImage ---

mkdir -p "$DIST_DIR"
OUTPUT="$DIST_DIR/limux-x86_64.AppImage"

echo ">>> Running appimagetool..."
# Nix sets SOURCE_DATE_EPOCH for reproducible builds, but mksquashfs
# refuses to run when both SOURCE_DATE_EPOCH and timestamp CLI flags
# are set. Unset it for the appimagetool invocation.
unset SOURCE_DATE_EPOCH
"$APPIMAGETOOL_EXTRACTED" \
    --no-appstream \
    "$APPDIR" \
    "$OUTPUT"

# Ensure the AppImage is executable in case the user's umask or
# filesystem mount options stripped the executable bit during copy.
chmod +x "$OUTPUT"

echo ""
echo ">>> Done: $OUTPUT"
ls -lh "$OUTPUT"
