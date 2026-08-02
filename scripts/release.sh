#!/usr/bin/env bash
# Create a release tag with optional interactive version bump.
#
# Usage: ./scripts/release.sh [new-version]
#
# If new-version is provided, bumps both app/Cargo.toml and cli/Cargo.toml
# to that version, commits the bump, then creates an annotated git tag
# v{version} and pushes it to origin.
# If omitted, prompts interactively for the new version.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

# --- Help ---

if [[ "${1:-}" == "--help" || "${1:-}" == "-h" ]]; then
    sed -n '3,11p' "$0"
    exit 0
fi

# --- Extract current version ---

CURRENT_VERSION=$(grep -m1 '^version' app/Cargo.toml | sed 's/.*"\(.*\)".*/\1/')
if [[ -z "$CURRENT_VERSION" ]]; then
    echo "ERROR: could not extract version from app/Cargo.toml" >&2
    exit 1
fi

echo "Current version: ${CURRENT_VERSION}"

# --- Determine new version ---

NEW_VERSION="${1:-}"
if [[ -z "$NEW_VERSION" ]]; then
    read -p "New version: " NEW_VERSION
    if [[ -z "$NEW_VERSION" ]]; then
        echo "ERROR: no version provided." >&2
        exit 1
    fi
fi

# --- Bump version in Cargo.toml files ---

echo ">>> Bumping version to ${NEW_VERSION} ..."
sed -i "s/^version = \".*\"/version = \"${NEW_VERSION}\"/" app/Cargo.toml cli/Cargo.toml

# --- Refresh Cargo.lock ---

# cargo rewrites Cargo.lock to match the bumped Cargo.toml versions. Build
# (rather than `cargo metadata`) so we also get a "does this release compile"
# sanity check before committing. Falls back to `nix develop` when cargo is
# not on PATH (this repo's default dev environment).
echo ">>> Building to refresh Cargo.lock ..."
if command -v cargo >/dev/null 2>&1; then
    cargo build --workspace
else
    nix develop --command cargo build --workspace
fi

# --- Commit version bump ---

echo ">>> Committing version bump ..."
git add app/Cargo.toml cli/Cargo.toml Cargo.lock
git commit -m "Bump version to ${NEW_VERSION}"

TAG="v${NEW_VERSION}"

echo ">>> Preparing release ${TAG} ..."

# --- Check if tag already exists ---

if git rev-parse --verify "$TAG" >/dev/null 2>&1; then
    echo "ERROR: tag ${TAG} already exists." >&2
    echo "  Choose a different version." >&2
    exit 1
fi

# --- Create annotated tag ---

echo ">>> Creating annotated tag ${TAG} ..."
git tag -a "$TAG" -m "Release ${TAG}"

# --- Push commit and tag ---

echo ">>> Pushing commit to origin ..."
git push origin HEAD

echo ">>> Pushing tag ${TAG} to origin ..."
git push origin "$TAG"

# --- Success ---

echo ""
echo ">>> Tag ${TAG} created and pushed."
echo "    GitHub Actions will build the release."
