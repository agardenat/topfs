#!/usr/bin/env bash
# Build an .rpm package into packaging/dist/ from the prebuilt binary.
set -euo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/../common.sh"

command -v rpmbuild >/dev/null || { echo "rpmbuild is required (rpm-build / rpmdevtools)" >&2; exit 1; }

build_release_binary

TOPDIR="$(mktemp -d)"
trap 'rm -rf "$TOPDIR"' EXIT
mkdir -p "$TOPDIR"/{BUILD,RPMS,SOURCES,SPECS,SRPMS}

cp "$BIN_PATH" "$TOPDIR/SOURCES/$PKG_NAME"
cp "$PROJECT_ROOT/packaging/rpm/$PKG_NAME.spec" "$TOPDIR/SPECS/"

rpmbuild \
    --define "_topfs_version $PKG_VERSION" \
    --define "_topdir $TOPDIR" \
    -bb "$TOPDIR/SPECS/$PKG_NAME.spec"

find "$TOPDIR/RPMS" -name '*.rpm' -exec cp -v {} "$DIST_DIR/" \;
echo ">> rpm(s) copied to $DIST_DIR"
