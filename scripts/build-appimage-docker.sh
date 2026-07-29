#!/usr/bin/env bash
# Build the limux AppImage inside the project's Docker image.
#
# One-shot convenience wrapper: builds the image if needed, then runs
# the build pipeline. The output lands in ./dist/ on the host.
#
# Usage:
#   scripts/build-appimage-docker.sh
#
# Requires Docker. Does not require the host to have Rust, Zig, or any
# GTK/WebKit dev headers installed — the Docker image bundles them.

set -euo pipefail
cd "$(dirname "$0")/.."

docker build \
    -f packaging/Dockerfile.appimage-build \
    -t limux-builder \
    .

exec docker run --rm -v "$(pwd):/src" limux-builder
