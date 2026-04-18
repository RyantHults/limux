#!/usr/bin/env bash
# Launch limux with an isolated socket, run MVP pytest suite, tear down.
#
# Usage:
#   scripts/run-tests-linux.sh             # build + test
#   LIMUX_BIN=path/to/limux scripts/...    # skip build, use prebuilt binary
#   scripts/run-tests-linux.sh -k <expr>   # pass through to pytest

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

LIMUX_BIN="${LIMUX_BIN:-}"
if [[ -z "$LIMUX_BIN" ]]; then
  echo ">>> building limux (debug)"
  cargo build --package limux --bin limux
  LIMUX_BIN="target/debug/limux"
fi

if [[ ! -x "$LIMUX_BIN" ]]; then
  echo "error: limux binary not found or not executable at $LIMUX_BIN" >&2
  exit 1
fi

# Isolated socket path so we don't collide with a running user session.
SOCKET_DIR="$(mktemp -d -t limux-test-XXXXXX)"
SOCKET_PATH="$SOCKET_DIR/limux.sock"
export LIMUX_SOCKET="$SOCKET_PATH"

# Run headlessly if no display is set. GTK apps need a display server.
DISPLAY_CMD=()
if [[ -z "${DISPLAY:-}" && -z "${WAYLAND_DISPLAY:-}" ]]; then
  if command -v xvfb-run >/dev/null 2>&1; then
    DISPLAY_CMD=(xvfb-run -a --server-args="-screen 0 1280x800x24")
  else
    echo "warning: no DISPLAY/WAYLAND_DISPLAY and xvfb-run unavailable; GUI tests may fail" >&2
  fi
fi

cleanup() {
  if [[ -n "${LIMUX_PID:-}" ]] && kill -0 "$LIMUX_PID" 2>/dev/null; then
    kill -TERM "$LIMUX_PID" 2>/dev/null || true
    wait "$LIMUX_PID" 2>/dev/null || true
  fi
  rm -rf "$SOCKET_DIR"
}
trap cleanup EXIT

echo ">>> launching limux (socket=$SOCKET_PATH)"
"${DISPLAY_CMD[@]}" "$LIMUX_BIN" --socket "$SOCKET_PATH" &
LIMUX_PID=$!

# Wait up to 30s for the socket to show up.
for _ in $(seq 1 300); do
  [[ -S "$SOCKET_PATH" ]] && break
  sleep 0.1
done
if [[ ! -S "$SOCKET_PATH" ]]; then
  echo "error: socket never appeared at $SOCKET_PATH" >&2
  exit 1
fi

echo ">>> running MVP test suite"
python3 -m pytest tests_v2/test_linux_mvp.py -v "$@"
