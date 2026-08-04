#!/usr/bin/env bash
# Build a self-contained AppImage for limux.
#
# Output: dist/limux-<version>-x86_64.AppImage
#
# What this does (default / system path):
#   1. Verifies the prebuilt artifacts (target/release/limux, etc.)
#   2. Stages an AppDir matching the AppImage spec
#   3. Downloads linuxdeploy + linuxdeploy-plugin-gtk
#   4. Runs linuxdeploy to bundle GTK/GLib/Pango/Harfbuzz/Adwaita deps
#      and copy the WebKitGTK 6.0 .so files
#   5. Copies the WebKitGTK 6.0 helper processes (WebKit*Process)
#   6. Byte-patches the hardcoded helper paths inside libwebkitgtk-6.0.so
#   7. Removes any libwayland-* that snuck in (Mesa version conflict)
#   8. Patches the binary rpath + interpreter (FHS-reset)
#   9. Writes AppRun with --install self-install + env wiring
#  10. Stages Adwaita theme files from libgtk-4.so GResources
#  11. linuxdeploy produces dist/limux-<version>-x86_64.AppImage
#
# The result is a single .AppImage file that runs on any modern Linux
# distro (Ubuntu 22.04+, Fedora 38+, Arch) with no nix install required.
#
# The closure source is the SYSTEM package manager (via linuxdeploy's
# ldd-driven discovery + apt-installed libs at build time), not nix.
# For NixOS users, pass --use-nix to fall back to the legacy nix-build
# closure path. The --use-nix path uses the original appimagetool
# pipeline (preserved verbatim from the prior script).

set -euo pipefail

# Extract a type-2 AppImage by locating its embedded squashfs.
#
# Type-2 AppImages are static-pie ELFs with a squashfs filesystem appended
# at a non-zero offset. Plain `unsquashfs -d DEST FILE` only checks offset
# 0, which fails. This helper:
#   1. Searches for the squashfs superblock magic ('hsqs' = 0x68 0x73 0x71 0x73)
#   2. Validates each candidate with `unsquashfs -s` (header-only check)
#   3. Extracts with the correct offset
#
# Required in rootless docker where FUSE + binfmt_misc are unavailable
# and the AppImage's own runtime stub can't run.
#
# Usage: extract_appimage <AppImage> <dest-dir>
extract_appimage() {
    local appimage="$1"
    local dest="$2"

    if [[ ! -f "$appimage" ]]; then
        echo "ERROR: $appimage not found" >&2
        return 1
    fi

    local offset=""
    while IFS=: read -r candidate _; do
        if unsquashfs -s -o "$candidate" "$appimage" >/dev/null 2>&1; then
            offset="$candidate"
            break
        fi
    done < <(grep -boa $'hsqs' "$appimage" 2>/dev/null)

    if [[ -z "$offset" ]]; then
        echo "ERROR: no valid squashfs superblock found in $appimage" >&2
        return 1
    fi

    rm -rf "$dest"
    if ! unsquashfs -q -d "$dest" -o "$offset" "$appimage"; then
        echo "ERROR: unsquashfs failed at offset $offset" >&2
        return 1
    fi
}

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

# Extract version from Cargo.toml for versioned AppImage naming.
VERSION=$(grep -m1 '^version' app/Cargo.toml | sed 's/.*"\(.*\)".*/\1/')
if [[ -z "$VERSION" ]]; then
    echo "ERROR: could not extract version from app/Cargo.toml" >&2
    exit 1
fi
echo ">>> Building limux v${VERSION} AppImage..."

# --- Option parsing ---

USE_NIX=0
for arg in "$@"; do
    case "$arg" in
        --use-nix) USE_NIX=1 ;;
        -h|--help)
            sed -n '2,32p' "$0"
            exit 0
            ;;
        *) echo "ERROR: unknown argument: $arg" >&2; exit 2 ;;
    esac
done

# --- Sanity checks ---

if [[ ! -x target/release/limux ]] || [[ ! -x target/release/limux-cli ]]; then
    echo "ERROR: prebuilt binaries missing." >&2
    echo "  Run: cargo build --release" >&2
    exit 1
fi

if [[ ! -f ghostty/zig-out/lib/ghostty-internal.so ]]; then
    echo "ERROR: libghostty missing." >&2
    echo "  Run: cd ghostty && zig build -Dapp-runtime=none -Demit-lib-vt=false -Doptimize=ReleaseFast -Dcpu=x86_64_v3 && cd .." >&2
    exit 1
fi

if ! command -v patchelf >/dev/null 2>&1; then
    echo "ERROR: patchelf is required." >&2
    echo "  Debian/Ubuntu:  sudo apt install patchelf" >&2
    echo "  Fedora:         sudo dnf install patchelf" >&2
    echo "  Arch:           sudo pacman -S patchelf" >&2
    echo "  NixOS:          run with --use-nix (handled by nix develop)" >&2
    exit 1
fi

# --- Temp dirs / cleanup ---

DIST_DIR="$REPO_ROOT/dist"
APPDIR="$(mktemp -d -t limux-appdir-XXXXXX)"
TOOLS_DIR="$(mktemp -d -t limux-tools-XXXXXX)"

cleanup() {
    if [[ -n "$APPDIR" && -d "$APPDIR" ]]; then
        chmod -R u+w "$APPDIR" 2>/dev/null || true
        rm -rf "$APPDIR" 2>/dev/null || true
    fi
    if [[ -n "$TOOLS_DIR" && -d "$TOOLS_DIR" ]]; then
        rm -rf "$TOOLS_DIR" 2>/dev/null || true
    fi
}
trap cleanup EXIT

mkdir -p "$DIST_DIR"
mkdir -p "$APPDIR/usr/bin"
mkdir -p "$APPDIR/usr/lib"
mkdir -p "$APPDIR/usr/share/applications"
mkdir -p "$APPDIR/usr/share/icons/hicolor/scalable/apps"

# --- Step 1: stage binaries, libghostty, desktop, icon ---

echo ">>> Staging AppDir..."
cp target/release/limux     "$APPDIR/usr/bin/limux"
cp target/release/limux-cli "$APPDIR/usr/bin/limux-cli"

# libghostty (zig emits ghostty-internal.{so,a}; the binary's SONAME
# is libghostty.so, so we add the symlink for that too).
cp -P ghostty/zig-out/lib/ghostty-internal.so "$APPDIR/usr/lib/"
cp -P ghostty/zig-out/lib/ghostty-internal.a  "$APPDIR/usr/lib/"
ln -sf ghostty-internal.so "$APPDIR/usr/lib/libghostty-internal.so"
ln -sf ghostty-internal.so "$APPDIR/usr/lib/libghostty.so"

# Desktop entry + icon. appimagetool looks for limux.{png,svg,xpm}
# in the AppDir root, so we stage limux.svg there and in the hicolor
# theme location. We then convert the SVG to a real 256x256 PNG for
# limux.png and .DirIcon so file managers and the freedesktop
# thumbnailer can decode them.
cp packaging/AppImage/limux.desktop "$APPDIR/limux.desktop"
cp packaging/AppImage/limux.svg     "$APPDIR/limux.svg"
cp packaging/AppImage/limux.svg     "$APPDIR/usr/share/icons/hicolor/scalable/apps/limux.svg"

if command -v rsvg-convert >/dev/null 2>&1; then
    rsvg-convert -w 256 -h 256 "$APPDIR/limux.svg" > "$APPDIR/limux.png"
    rsvg-convert -w 256 -h 256 "$APPDIR/limux.svg" > "$APPDIR/.DirIcon"
elif command -v inkscape >/dev/null 2>&1; then
    inkscape "$APPDIR/limux.svg" --export-type=png --export-filename="$APPDIR/limux.png" -w 256 -h 256
    cp "$APPDIR/limux.png" "$APPDIR/.DirIcon"
else
    cp "$APPDIR/limux.svg" "$APPDIR/limux.png"
    cp "$APPDIR/limux.svg" "$APPDIR/.DirIcon"
    echo "WARNING: neither rsvg-convert nor inkscape found; limux.png and .DirIcon are SVG renamed to PNG" >&2
fi

# XDG_DATA_DIRS-discoverable location for the app menu.
cp packaging/AppImage/limux.desktop "$APPDIR/usr/share/applications/limux.desktop"
# Also under the application ID so Wayland's xdg-shell app_id matching works.
cp packaging/AppImage/limux.desktop "$APPDIR/usr/share/applications/com.limux.terminal.desktop"

# Stamp the real version into the desktop files.
for df in "$APPDIR/limux.desktop" \
          "$APPDIR/usr/share/applications/limux.desktop" \
          "$APPDIR/usr/share/applications/com.limux.terminal.desktop"; do
    sed -i "s|^X-AppImage-Version=.*|X-AppImage-Version=${VERSION}|" "$df"
done

# --- Step 2: runtime closure ---

if (( USE_NIX )); then
    # ---- Legacy Nix path (preserved from the original script) ----
    #
    # On NixOS the nix store is the only reliable source of the GTK4
    # + WebKitGTK 6.0 runtime closure. We don't run linuxdeploy here
    # because its system-package discovery is meaningless inside the
    # nix store. We use appimagetool directly at the end.
    echo ">>> Computing runtime closure via nix (--use-nix)..."
    if ! command -v nix-build >/dev/null 2>&1; then
        echo "ERROR: --use-nix requires nix-build on PATH (run inside 'nix develop')." >&2
        exit 1
    fi
    RUNTIME_LIB_DIR=$(nix-build --no-out-link -E '
      let
        pkgs = import <nixpkgs> {};
      in
      pkgs.symlinkJoin {
        name = "limux-runtime-libs";
        # .out suffix is required for multi-output packages (glib, pango,
        # openssl) so we get the .so files, not the .bin wrapper.
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
    # Preserve symlinks (libfoo.so.1 -> libfoo.so.1.2.3) so the dynamic
    # linker can resolve versioned SONAMEs.
    cp -aP "$RUNTIME_LIB_DIR"/lib/*.so* "$APPDIR/usr/lib/" 2>/dev/null || true
    cp -aP "$RUNTIME_LIB_DIR"/lib/*.so "$APPDIR/usr/lib/" 2>/dev/null || true
    if [[ -d "$RUNTIME_LIB_DIR/lib/gio" ]]; then
        cp -a "$RUNTIME_LIB_DIR/lib/gio" "$APPDIR/usr/lib/"
    fi
    if [[ -d "$RUNTIME_LIB_DIR/lib/gdk-pixbuf-2.0" ]]; then
        cp -a "$RUNTIME_LIB_DIR/lib/gdk-pixbuf-2.0" "$APPDIR/usr/lib/"
    fi
    if [[ -d "$RUNTIME_LIB_DIR/lib/gtk-4.0" ]]; then
        cp -a "$RUNTIME_LIB_DIR/lib/gtk-4.0" "$APPDIR/usr/lib/"
    fi
    if [[ -d "$RUNTIME_LIB_DIR/share" ]]; then
        cp -a "$RUNTIME_LIB_DIR/share" "$APPDIR/usr/"
    fi
    # GSettings schemas: nix stores them in versioned subdirs that
    # GSettings can't find via XDG_DATA_DIRS. Flatten + recompile.
    chmod -R u+w "$APPDIR/usr/share" 2>/dev/null || true
    if [[ -d "$APPDIR/usr/share/gsettings-schemas" ]]; then
        mkdir -p "$APPDIR/usr/share/glib-2.0/schemas"
        find "$APPDIR/usr/share/gsettings-schemas" -name '*.gschema.xml' \
            -exec cp -aL {} "$APPDIR/usr/share/glib-2.0/schemas/" \;
        if command -v glib-compile-schemas >/dev/null 2>&1; then
            glib-compile-schemas "$APPDIR/usr/share/glib-2.0/schemas/"
        fi
    fi
else
    # ---- System path (default) ----
    #
    # linuxdeploy + linuxdeploy-plugin-gtk discovers and bundles
    # the GTK4 + WebKitGTK 6.0 closure from the system package
    # manager. The plugin auto-handles gdk-pixbuf loaders and
    # glib-compile-schemas; we don't need to call those manually.
    echo ">>> Bundling GTK/WebKit/GLib via linuxdeploy..."
    if [[ ! -x "$TOOLS_DIR/linuxdeploy-x86_64.AppImage" ]]; then
        echo "    Downloading linuxdeploy (continuous build)..."
        curl -fsSL \
            "https://github.com/linuxdeploy/linuxdeploy/releases/download/continuous/linuxdeploy-x86_64.AppImage" \
            -o "$TOOLS_DIR/linuxdeploy-x86_64.AppImage"
        chmod +x "$TOOLS_DIR/linuxdeploy-x86_64.AppImage"
    fi
    if [[ ! -x "$TOOLS_DIR/linuxdeploy-plugin-gtk.sh" ]]; then
        echo "    Downloading linuxdeploy-plugin-gtk..."
        curl -fsSL \
            "https://raw.githubusercontent.com/linuxdeploy/linuxdeploy-plugin-gtk/master/linuxdeploy-plugin-gtk.sh" \
            -o "$TOOLS_DIR/linuxdeploy-plugin-gtk.sh"
        chmod +x "$TOOLS_DIR/linuxdeploy-plugin-gtk.sh"
    fi
    # linuxdeploy is shipped as a type-2 AppImage whose runtime depends
    # on a kernel-side binfmt_misc handler (or appimaged) to self-extract
    # at exec time. That handler is missing in rootless docker and some
    # minimal CI images, so direct execution returns ENOENT. We extract
    # the AppImage ourselves with unsquashfs and run the inner AppRun
    # directly. This is the same pattern the --use-nix path uses for
    # appimagetool (see below at the appimagetool extraction block).
    LINUXDEPLOY_EXTRACTED="$TOOLS_DIR/linuxdeploy-extracted"
    LINUXDEPLOY_BIN="$LINUXDEPLOY_EXTRACTED/AppRun"
    if [[ ! -x "$LINUXDEPLOY_BIN" ]]; then
        echo "    Extracting linuxdeploy AppImage (avoids binfmt_misc dependency)..."
        if ! extract_appimage "$TOOLS_DIR/linuxdeploy-x86_64.AppImage" \
                "$LINUXDEPLOY_EXTRACTED"; then
            echo "ERROR: failed to extract linuxdeploy AppImage" >&2
            echo "  Is squashfs-tools installed? (apt install squashfs-tools)" >&2
            exit 1
        fi
        chmod +x "$LINUXDEPLOY_BIN"
    fi
    # linuxdeploy discovers plugins by looking on PATH for entries
    # matching `linuxdeploy-plugin-<name>.*`. Keep $TOOLS_DIR on PATH so
    # the .sh plugin we downloaded above is findable by the inner
    # linuxdeploy binary.
    export PATH="$TOOLS_DIR:${PATH}"

    # REQUIRED for linuxdeploy to run in CI / container environments
    # where FUSE is not available — it self-extracts and runs from a
    # temp dir instead.
    export APPIMAGE_EXTRACT_AND_RUN=1
    # Avoids the .relr.dyn strip regression in modern binutils
    # (linuxdeploy's bundled strip fails on Ubuntu 24.04 / Arch).
    export NO_STRIP=1
    # The plugin hardcodes Adwaita theme selection which breaks
    # libadwaita styling. We unset GTK_THEME in our AppRun so
    # libadwaita's own detection takes over.
    export DEPLOY_GTK_VERSION=4

    # WebKit's libjavascriptcoregtk and libwebkitgtk are dlopened
    # lazily and not always visible to ldd — declare them explicitly
    # so linuxdeploy pulls their .so files into the AppDir.
    WEBKIT_LIB=""
    for candidate in \
        "$APPDIR/usr/lib/x86_64-linux-gnu/libwebkitgtk-6.0.so.4" \
        "/usr/lib/x86_64-linux-gnu/libwebkitgtk-6.0.so.4"; do
        if [[ -e "$candidate" ]]; then
            WEBKIT_LIB="$candidate"
            break
        fi
    done
    LD_ARGS=()
    if [[ -n "$WEBKIT_LIB" ]]; then
        LD_ARGS=(--library "$WEBKIT_LIB")
    fi

    # linuxdeploy produces the AppImage as a side effect of
    # --output appimage. We don't pass an output path so it lands
    # next to the AppDir by default; we'll move it to dist/ at the
    # end (after the WebKit patch, which has to happen against the
    # AppDir's .so files BEFORE the AppImage is sealed).
    # Note: $LINUXDEPLOY_BIN is the extracted AppRun, not the .AppImage
    # file directly — see the extraction block above.
    "$LINUXDEPLOY_BIN" \
        --appdir "$APPDIR" \
        --executable "$APPDIR/usr/bin/limux" \
        --executable "$APPDIR/usr/bin/limux-cli" \
        --desktop-file "$APPDIR/limux.desktop" \
        --icon-file "$APPDIR/limux.png" \
        "${LD_ARGS[@]}" \
        --plugin gtk

    # linuxdeploy emitted its AppImage next to the AppDir; remove it
    # so we can re-seal after our WebKit patch below.
    STAGING_APPIMAGE="$(dirname "$APPDIR")/limux-x86_64.AppImage"
    if [[ -f "$STAGING_APPIMAGE" ]]; then
        rm -f "$STAGING_APPIMAGE"
    fi

    # --- Step 2b: WebKitGTK 6.0 helper-process patching ---
    # WebKit's libwebkitgtk-6.0.so has compile-time absolute paths to
    # the WebKit*Process helper binaries baked in. linuxdeploy copies
    # the helpers but doesn't rewrite those paths. We do a precise
    # byte-level patch: replace the long path with a shorter sentinel,
    # then AppRun symlinks the sentinel to the real helper dir at
    # launch. Same approach Lantern uses.
    WEBKIT_HELPER_DIR_SYSTEM="$(pkg-config --variable=libdir webkitgtk-6.0 2>/dev/null || true)/webkitgtk-6.0"
    WEBKIT_HELPER_DIR_APP="$APPDIR/usr/lib/webkitgtk-6.0"
    if [[ -d "$WEBKIT_HELPER_DIR_SYSTEM" ]]; then
        echo ">>> Copying WebKit helper processes..."
        mkdir -p "$WEBKIT_HELPER_DIR_APP/injected-bundle"
        for helper in WebKitNetworkProcess WebKitWebProcess WebKitGPUProcess; do
            if [[ -e "$WEBKIT_HELPER_DIR_SYSTEM/$helper" ]]; then
                cp -L "$WEBKIT_HELPER_DIR_SYSTEM/$helper" "$WEBKIT_HELPER_DIR_APP/"
            fi
        done
        if [[ -d "$WEBKIT_HELPER_DIR_SYSTEM/injected-bundle" ]]; then
            cp -L "$WEBKIT_HELPER_DIR_SYSTEM/injected-bundle/"*.so \
                "$WEBKIT_HELPER_DIR_APP/injected-bundle/" 2>/dev/null || true
        fi
    fi

    # Byte-patch the hardcoded helper path inside libwebkitgtk-6.0.so
    # and libjavascriptcoregtk-6.0.so. The original path is the full
    # multiarch prefix; we replace it with a short sentinel which
    # AppRun symlinks to the real helper dir at launch.
    SENTINEL_PATH="/tmp/limux-webkit-helpers"
    if [[ -d "$WEBKIT_HELPER_DIR_APP" ]]; then
        echo ">>> Patching WebKit helper paths..."
        python3 - "$APPDIR" "$SENTINEL_PATH" <<'PY'
import re, sys, glob
appdir, sentinel = sys.argv[1], sys.argv[2]
sentinel_bytes = sentinel.encode() + b"\x00"
pattern = re.compile(rb"/usr/lib/[a-z0-9_]+-linux-gnu/webkitgtk-6\.0\x00")
patched = 0
skipped = 0
for path in glob.glob(f"{appdir}/usr/lib*/libwebkitgtk-6.0.so*") + \
            glob.glob(f"{appdir}/usr/lib*/libjavascriptcoregtk-6.0.so*"):
    data = bytearray(open(path, "rb").read())
    for m in pattern.finditer(data):
        s, e = m.start(), m.end()
        if len(sentinel_bytes) > (e - s):
            # Sentinel too long; this binary has a different string
            # table layout. Skip rather than corrupt the ELF.
            skipped += 1
            continue
        # Pad the sentinel with trailing NULs to preserve byte length.
        replacement = sentinel_bytes + b"\x00" * ((e - s) - len(sentinel_bytes))
        data[s:e] = replacement
        patched += 1
    open(path, "wb").write(data)
print(f"    patched {patched} path(s); skipped {skipped} oversized")
PY
    fi

    # Remove libwayland-* if linuxdeploy pulled them in. They
    # version-conflict with system Mesa on end-user systems. The
    # excludelist should already exclude them, but be defensive in
    # case the binary was built before the fix.
    find "$APPDIR/usr/lib" -name 'libwayland-client.so*' -delete 2>/dev/null || true
    find "$APPDIR/usr/lib" -name 'libwayland-cursor.so*' -delete 2>/dev/null || true
    find "$APPDIR/usr/lib" -name 'libwayland-egl.so*' -delete 2>/dev/null || true
    find "$APPDIR/usr/lib" -name 'libwayland-server.so*' -delete 2>/dev/null || true
fi

# --- Step 3: rpath + interpreter fixup on the binaries ---
#
# The prebuilt binaries were linked in a nix or system dev shell, so
# their rpath AND their ELF interpreter may point into non-FHS paths.
# We reset both to the standard FHS layout that exists on every
# supported distro and inside the AppImage's squashed /lib64.
# linuxdeploy also does some patchelf work, but it's tied to the libs
# it bundled — resetting the interpreter to /lib64/ld-linux is
# something only we can do correctly.

echo ">>> Patching binary rpaths and interpreters..."
for bin in "$APPDIR/usr/bin/limux" "$APPDIR/usr/bin/limux-cli"; do
    patchelf --set-interpreter '/lib64/ld-linux-x86-64.so.2' "$bin"
    patchelf --set-rpath '$ORIGIN/../lib' "$bin"
done

# --- Step 4: AppRun script ---

# AppRun is invoked by the AppImage runtime. It sets up the env and
# execs the main binary. The runtime sets APPDIR to the AppDir path.
# Also handles --install for self-installing icons/desktop files into
# the host's XDG locations.
cat > "$APPDIR/AppRun" <<'APPRUN_EOF'
#!/usr/bin/env bash
# AppRun — entry point for the AppImage runtime.
set -e
set -x

APPDIR="$(cd "$(dirname "$0")" && pwd)"

LIMUX_APPIMAGE_VERSION=%%VERSION%%

# Symlink the WebKit helper path sentinel to the real helper dir.
# libwebkitgtk-6.0.so was byte-patched at build time to point at
# /tmp/limux-webkit-helpers for the WebKit*Process executables.
# Create the symlink lazily on first run so the AppImage stays
# relocatable. Use ln -sfn to atomically replace any stale link.
if [[ -d "$APPDIR/usr/lib/webkitgtk-6.0" ]]; then
    echo "[AppRun] Creating webkit helpers symlink..."
    ln -sfn "$APPDIR/usr/lib/webkitgtk-6.0" /tmp/limux-webkit-helpers
    echo "[AppRun] Webkit helpers symlink created"
fi

# Self-install: copy the icon and desktop file to the user's XDG
# locations so the host desktop environment (GNOME Shell, KDE, etc.)
# can see the app in the application menu and Alt+Tab switcher.
if [[ "${1:-}" == "--install" ]]; then
    set +e
    echo ">>> Installing limux v${LIMUX_APPIMAGE_VERSION:-unknown} to ~/.local/share/..."

    ICON_SRC="$APPDIR/usr/share/icons/hicolor/scalable/apps/limux.svg"
    HICOLOR_INDEX_SRC="$APPDIR/usr/share/icons/hicolor/index.theme"
    DESKTOP_SRC="$APPDIR/usr/share/applications/limux.desktop"
    WAYLAND_DESKTOP_SRC="$APPDIR/usr/share/applications/com.limux.terminal.desktop"

    HICOLOR_DST_DIR="$HOME/.local/share/icons/hicolor"
    ICON_DST_DIR="$HICOLOR_DST_DIR/scalable/apps"
    DESKTOP_DST_DIR="$HOME/.local/share/applications"

    mkdir -p "$ICON_DST_DIR" "$DESKTOP_DST_DIR"

    if [[ -f "$ICON_SRC" ]]; then
        cp "$ICON_SRC" "$ICON_DST_DIR/limux.svg" && echo "  icon: $ICON_DST_DIR/limux.svg"
    else
        echo "  WARNING: icon source not found at $ICON_SRC"
    fi

    if [[ -f "$HICOLOR_INDEX_SRC" ]]; then
        cp "$HICOLOR_INDEX_SRC" "$HICOLOR_DST_DIR/index.theme" && echo "  hicolor index: $HICOLOR_DST_DIR/index.theme"
    else
        echo "  WARNING: hicolor index.theme not found at $HICOLOR_INDEX_SRC"
    fi

    APPIMAGE_PATH="${APPIMAGE:-}"
    if [[ -z "$APPIMAGE_PATH" && -r /proc/self/mountinfo ]]; then
        while IFS= read -r line; do
            mount_point=$(echo "$line" | awk '{print $5}')
            if [[ "$mount_point" == "$APPDIR" ]]; then
                source=$(echo "$line" | awk '{for(i=1;i<=NF;i++) if($i=="-"){print $(i+2); exit}}')
                if [[ -n "$source" && -f "$source" ]]; then
                    APPIMAGE_PATH="$source"
                    break
                fi
            fi
        done < /proc/self/mountinfo
    fi

    # Read the version from the desktop file we're about to install.
    NEW_VERSION=""
    if [[ -f "$DESKTOP_SRC" ]]; then
        NEW_VERSION=$(grep -m1 '^X-AppImage-Version=' "$DESKTOP_SRC" | cut -d= -f2)
    fi

    # Check existing desktop files for a previous version.
    OLD_VERSION=""
    for existing in "$DESKTOP_DST_DIR/limux.desktop" "$DESKTOP_DST_DIR/com.limux.terminal.desktop"; do
        if [[ -f "$existing" ]]; then
            OLD_VERSION=$(grep -m1 '^X-AppImage-Version=' "$existing" | cut -d= -f2)
            [[ -n "$OLD_VERSION" ]] && break
        fi
    done

    # Determine the old AppImage path from the existing desktop file's Exec= line.
    OLD_APPIMAGE=""
    if [[ -f "$DESKTOP_DST_DIR/limux.desktop" ]]; then
        OLD_EXEC=$(grep -m1 '^Exec=' "$DESKTOP_DST_DIR/limux.desktop" | cut -d= -f2-)
        # Strip quotes and any arguments (%U, etc.)
        OLD_APPIMAGE=$(echo "$OLD_EXEC" | sed 's/^"//;s/"$//;s/ %U$//;s/ .*//')
        # Only keep it if it looks like an AppImage path
        if [[ "$OLD_APPIMAGE" != *.AppImage ]]; then
            OLD_APPIMAGE=""
        fi
    fi

    rewrite_exec() {
        local file="$1"
        if [[ -n "$APPIMAGE_PATH" && -f "$file" ]]; then
            sed -i "s|^Exec=.*|Exec=\"$APPIMAGE_PATH\" %U|" "$file"
            echo "  rewrote Exec= in $file"
        fi
    }

    if [[ -n "$NEW_VERSION" && -n "$OLD_VERSION" && "$NEW_VERSION" == "$OLD_VERSION" ]]; then
        echo "  desktop entry already at v${OLD_VERSION}, updating Exec path only"
        # Just update the Exec path in case the AppImage moved.
        rewrite_exec "$DESKTOP_DST_DIR/limux.desktop"
        rewrite_exec "$DESKTOP_DST_DIR/com.limux.terminal.desktop"
    else
        if [[ -n "$OLD_VERSION" ]]; then
            echo "  upgrading desktop entry from v${OLD_VERSION} to v${NEW_VERSION}"
        fi
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
    fi

    if [[ -z "$APPIMAGE_PATH" ]]; then
        echo "  WARNING: could not determine AppImage path; menu entries will use bare 'limux' command"
    fi

    if command -v update-desktop-database >/dev/null 2>&1; then
        update-desktop-database "$DESKTOP_DST_DIR" && echo "  refreshed desktop database"
    fi
    if command -v gtk-update-icon-cache >/dev/null 2>&1; then
        rm -f "$HICOLOR_DST_DIR/icon-theme.cache"
        gtk-update-icon-cache -f -t "$HICOLOR_DST_DIR" 2>/dev/null && echo "  refreshed icon cache"
    fi

    # Offer to delete old AppImage versions.
    if [[ -n "$APPIMAGE_PATH" && -f "$APPIMAGE_PATH" ]]; then
        APPIMAGE_DIR="$(dirname "$APPIMAGE_PATH")"
        OLD_APPIMAGES=()
        for candidate in "$APPIMAGE_DIR"/limux-*-x86_64.AppImage; do
            [[ -f "$candidate" ]] || continue
            # Skip the current AppImage
            real_current="$(realpath "$APPIMAGE_PATH" 2>/dev/null || echo "$APPIMAGE_PATH")"
            real_candidate="$(realpath "$candidate" 2>/dev/null || echo "$candidate")"
            if [[ "$real_candidate" != "$real_current" ]]; then
                OLD_APPIMAGES+=("$candidate")
            fi
        done
        if [[ ${#OLD_APPIMAGES[@]} -gt 0 ]]; then
            echo ""
            echo "  Found ${#OLD_APPIMAGES[@]} old AppImage(s):"
            for old in "${OLD_APPIMAGES[@]}"; do
                echo "    - $old ($(du -h "$old" | cut -f1))"
            done
            read -r -p "  Delete old version(s)? [y/N] " answer
            if [[ "$answer" =~ ^[Yy]$ ]]; then
                for old in "${OLD_APPIMAGES[@]}"; do
                    rm -f "$old" && echo "  deleted: $old"
                done
            else
                echo "  keeping old version(s)"
            fi
        fi
    fi

    echo ""
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

# Point gdk-pixbuf at the loaders we bundle, NOT the host's. Without
# this, gdk-pixbuf picks up the system loader cache (e.g. a librsvg
# 2.6x pixbuf loader from /usr/share) which then dlopens against the
# bundled librsvg via LD_LIBRARY_PATH — version mismatch → undefined
# symbol (rsvg_handle_get_pixbuf_and_error) → every SVG symbolic icon
# fails to load and renders as the "missing icon" placeholder. linuxdeploy's
# autogenerated AppRun set this via its gtk plugin hook; our custom
# AppRun must set it explicitly.
export GDK_PIXBUF_MODULE_FILE="$APPDIR/usr/lib/gdk-pixbuf-2.0/2.10.0/loaders.cache"

# WebKit's bubblewrap sandbox can't see the FUSE mount, so we have
# to disable it. The flag name is canonical (still current in 2025+).
export WEBKIT_DISABLE_SANDBOX_THIS_IS_DANGEROUS=1
# Some VMs / older GPUs don't support the dmabuf renderer path.
export WEBKIT_DISABLE_DMABUF_RENDERER=1
# The injected-bundle path in libwebkitgtk-6.0.so still points at the
# host system (/usr/lib/<multiarch>/webkitgtk-6.0/injected-bundle/).
# Point it at the bundle we ship so the WebKitWebProcess can load it.
export WEBKIT_INJECTED_BUNDLE_PATH="$APPDIR/usr/lib/webkitgtk-6.0/injected-bundle/libwebkitgtkinjectedbundle.so"

# Make the control socket predictable if the user runs the CLI from
# the same shell. The AppImage runtime sets $XDG_RUNTIME_DIR.
if [[ -z "${LIMUX_SOCKET:-}" && -n "${XDG_RUNTIME_DIR:-}" ]]; then
    export LIMUX_SOCKET="$XDG_RUNTIME_DIR/limux.sock"
fi

echo "[AppRun] DISPLAY=$DISPLAY WAYLAND_DISPLAY=$WAYLAND_DISPLAY GDK_BACKEND=$GDK_BACKEND"
echo "[AppRun] Executing limux binary..."
exec "$APPDIR/usr/bin/limux" "$@"
APPRUN_EOF
chmod +x "$APPDIR/AppRun"

# Stamp the version into AppRun so --install can report it.
sed -i "s|LIMUX_APPIMAGE_VERSION=%%VERSION%%|LIMUX_APPIMAGE_VERSION=${VERSION}|" "$APPDIR/AppRun"

# --- Step 5: GTK4 Adwaita theme on disk ---
#
# GTK4's Adwaita theme is compiled into libgtk-4.so as GResources, not
# shipped as files. Without on-disk theme directories, GTK may discover
# system themes that lack the dark variant, causing a light fallback
# when gtk-application-prefer-dark-theme is set. Extract the wrapper
# CSS from the GResource so the AppImage ships its own Adwaita themes.
#
# This step is independent of the closure path; it operates on the
# libgtk-4.so that ends up in $APPDIR/usr/lib regardless of source.
if [[ -x "$(command -v gresource)" ]]; then
    echo ">>> Staging Adwaita theme files from libgtk-4 GResources..."
    GTK4_LIB=$(find "$APPDIR/usr/lib" -name 'libgtk-4.so*' \( -type f -o -type l \) 2>/dev/null | head -1)
    if [[ -n "$GTK4_LIB" ]]; then
        THEME_BASE="$APPDIR/usr/share/themes"
        mkdir -p "$THEME_BASE/Adwaita/gtk-4.0"
        mkdir -p "$THEME_BASE/Adwaita-dark/gtk-4.0"

        gresource extract "$GTK4_LIB" \
            /org/gtk/libgtk/theme/Default/gtk.css \
            > "$THEME_BASE/Adwaita/gtk-4.0/gtk.css" 2>/dev/null || true
        gresource extract "$GTK4_LIB" \
            /org/gtk/libgtk/theme/Default/gtk-dark.css \
            > "$THEME_BASE/Adwaita-dark/gtk-4.0/gtk-dark.css" 2>/dev/null || true
        gresource extract "$GTK4_LIB" \
            /org/gtk/libgtk/theme/Default/gtk-dark.css \
            > "$THEME_BASE/Adwaita-dark/gtk-4.0/gtk.css" 2>/dev/null || true

        cat > "$THEME_BASE/Adwaita/index.theme" <<'THEME_EOF'
[Desktop Entry]
Type=Application
Name=Adwaita

[GTK Theme]
Name=Adwaita
Type=GTK4
THEME_EOF

        cat > "$THEME_BASE/Adwaita-dark/index.theme" <<'THEME_EOF'
[Desktop Entry]
Type=Application
Name=Adwaita Dark

[GTK Theme]
Name=Adwaita-dark
Type=GTK4
THEME_EOF
    fi
fi

# --- Step 6: build the AppImage ---

OUTPUT="$DIST_DIR/limux-${VERSION}-x86_64.AppImage"

# Download and extract appimagetool. It is invoked directly on the
# AppDir (no FUSE needed). We seal with appimagetool — NOT
# `linuxdeploy --output appimage` — because linuxdeploy regenerates the
# autogenerated AppRun on every invocation, silently discarding the
# custom AppRun we wrote in Step 4 (which sets up the WebKit helper
# symlink, LD_LIBRARY_PATH, WEBKIT_* env, LIMUX_SOCKET and --install).
# appimagetool just squashes the AppDir as-is, so the custom AppRun
# survives into the sealed image.
if [[ ! -x "$TOOLS_DIR/appimagetool.AppImage" ]]; then
    echo "    Downloading appimagetool..."
    APPIMAGETOOL_VERSION=13
    curl -fsSL \
        "https://github.com/AppImage/AppImageKit/releases/download/${APPIMAGETOOL_VERSION}/obsolete-appimagetool-x86_64.AppImage" \
        -o "$TOOLS_DIR/appimagetool.AppImage"
    chmod +x "$TOOLS_DIR/appimagetool.AppImage"

    echo "    Extracting appimagetool..."
    mkdir -p "$TOOLS_DIR/extracted"
    (cd "$TOOLS_DIR/extracted" && \
        OFFSET=$(awk 'NR==13{e_shoff=$5} NR==18{e_shentsize=$5} NR==19{e_shnum=$5} END{print e_shoff+e_shentsize*e_shnum}' <(LC_ALL=C readelf -h "$TOOLS_DIR/appimagetool.AppImage")) && \
        unsquashfs -q -o "$OFFSET" -d "$TOOLS_DIR/squashfs-root" \
            "$TOOLS_DIR/appimagetool.AppImage")
fi

APPIMAGETOOL_EXTRACTED="$TOOLS_DIR/squashfs-root/usr/bin/appimagetool"
if [[ ! -x "$APPIMAGETOOL_EXTRACTED" ]]; then
    echo "ERROR: failed to extract appimagetool" >&2
    exit 1
fi

if (( USE_NIX )); then
    # On NixOS, patchelf appimagetool + its bundled mksquashfs so they
    # run against the nix store's glibc.
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
        patchelf \
            --set-interpreter "$PATCHED_INTERP" \
            --set-rpath "$PATCHED_LIBPATH/lib" \
            "$APPIMAGETOOL_EXTRACTED" 2>/dev/null || \
            echo "WARNING: patchelf failed on appimagetool"

        if [[ -d "$TOOLS_DIR/squashfs-root/usr/lib/appimagekit" ]]; then
            for elf in "$TOOLS_DIR/squashfs-root/usr/lib/appimagekit/"*; do
                [[ -f "$elf" ]] || continue
                if file "$elf" 2>/dev/null | grep -q 'ELF.*executable'; then
                    patchelf \
                        --set-interpreter "$PATCHED_INTERP" \
                        --set-rpath "$PATCHED_LIBPATH/lib" \
                        "$elf" 2>/dev/null || true
                fi
            done
        fi
    fi
fi

# Nix sets SOURCE_DATE_EPOCH for reproducible builds, but mksquashfs
# refuses to run when both SOURCE_DATE_EPOCH and timestamp CLI flags
# are set. Unset it for the appimagetool invocation.
unset SOURCE_DATE_EPOCH

echo ">>> Sealing AppImage with appimagetool..."
"$APPIMAGETOOL_EXTRACTED" \
    --no-appstream \
    "$APPDIR" \
    "$OUTPUT"

# Validate the output is actually a sealed AppImage. We can't rely on
# `file(1)` alone: type-2 AppImages report as `static-pie linked,
# stripped` (the loader's ELF metadata), and type-1 AppImages report as
# `dynamically linked` with the loader's interpreter. The one invariant
# every valid AppImage has is an embedded squashfs filesystem,
# identified by the `hsqs` magic bytes.
if ! grep -qa $'hsqs' "$OUTPUT" 2>/dev/null; then
    echo "ERROR: $OUTPUT has no squashfs magic (not an AppImage)" >&2
    file "$OUTPUT" >&2
    rm -f "$OUTPUT"
    exit 1
fi

chmod +x "$OUTPUT"

echo ""
echo ">>> Done: $OUTPUT"
ls -lh "$OUTPUT"
