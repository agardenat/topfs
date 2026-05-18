#!/usr/bin/env bash
# Shared metadata sourced by the packaging scripts.
set -euo pipefail

PROJECT_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

PKG_NAME="topfs"
PKG_VERSION="$(grep -m1 '^version' "$PROJECT_ROOT/Cargo.toml" | sed -E 's/.*"(.*)".*/\1/')"
PKG_DESC="Live top-N biggest filesystem entries with tree display"
PKG_MAINTAINER="Antoine Gardenat <agardenat@leisambro.net>"
PKG_HOMEPAGE="https://github.com/agardenat/topfs"
PKG_LICENSE="MIT"

BIN_PATH="$PROJECT_ROOT/target/release/$PKG_NAME"
DIST_DIR="$PROJECT_ROOT/packaging/dist"

build_release_binary() {
    echo ">> cargo build --release"
    ( cd "$PROJECT_ROOT" && cargo build --release )
    test -x "$BIN_PATH" || { echo "binary not found: $BIN_PATH" >&2; exit 1; }
}

mkdir -p "$DIST_DIR"
