#!/usr/bin/env bash
# Build a .deb package into packaging/dist/ using dpkg-deb.
set -euo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/../common.sh"

command -v dpkg-deb >/dev/null || { echo "dpkg-deb is required" >&2; exit 1; }

build_release_binary

DEB_ARCH="$(dpkg --print-architecture)"
STAGE="$(mktemp -d)"
trap 'rm -rf "$STAGE"' EXIT
chmod 755 "$STAGE"

install -Dm755 "$BIN_PATH" "$STAGE/usr/bin/$PKG_NAME"

mkdir -p "$STAGE/DEBIAN"
cat > "$STAGE/DEBIAN/control" <<EOF
Package: $PKG_NAME
Version: $PKG_VERSION
Section: utils
Priority: optional
Architecture: $DEB_ARCH
Maintainer: $PKG_MAINTAINER
Homepage: $PKG_HOMEPAGE
Description: $PKG_DESC
 Interactive terminal tool that continuously reports the largest
 filesystem entries as a live-updating tree.
EOF

OUT="$DIST_DIR/${PKG_NAME}_${PKG_VERSION}_${DEB_ARCH}.deb"
dpkg-deb --root-owner-group --build "$STAGE" "$OUT"
echo ">> built $OUT"
