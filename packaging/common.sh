#!/usr/bin/env bash
# Shared metadata sourced by the packaging scripts.
set -euo pipefail

PROJECT_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

PKG_NAME="topfs"
PKG_VERSION="$(grep -m1 '^version' "$PROJECT_ROOT/Cargo.toml" | sed -E 's/.*"(.*)".*/\1/')"
PKG_DESC="Live top-N biggest filesystem entries with tree display"
PKG_MAINTAINER="Antoine Gardenat <agardenat@leisambro.net>"
PKG_HOMEPAGE="https://github.com/agardenat/topfs"
PKG_LICENSE="Apache-2.0"

MUSL_TARGET="x86_64-unknown-linux-musl"
BIN_PATH="$PROJECT_ROOT/target/$MUSL_TARGET/release/$PKG_NAME"
DIST_DIR="$PROJECT_ROOT/packaging/dist"

# Static, fully self-contained binary (no dynamic glibc dependency) for the
# deb/rpm payloads. Portable across distros regardless of host glibc.
build_release_binary() {
    echo ">> rustup target add $MUSL_TARGET"
    rustup target add "$MUSL_TARGET" >/dev/null
    echo ">> cargo build --release --target $MUSL_TARGET"
    ( cd "$PROJECT_ROOT" && cargo build --release --target "$MUSL_TARGET" )
    test -x "$BIN_PATH" || { echo "binary not found: $BIN_PATH" >&2; exit 1; }
    if ldd "$BIN_PATH" 2>&1 | grep -qv 'statically linked\|not a dynamic'; then
        echo "WARNING: binary is not statically linked" >&2
    fi
}

mkdir -p "$DIST_DIR"
