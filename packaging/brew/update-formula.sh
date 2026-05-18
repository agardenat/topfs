#!/usr/bin/env bash
# Sync packaging/brew/topfs.rb url + sha256 with the current version.
# Builds a source tarball into packaging/dist/ and patches the formula.
set -euo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/../common.sh"

TAG="v$PKG_VERSION"
TARBALL="$DIST_DIR/${PKG_NAME}-${PKG_VERSION}.tar.gz"
FORMULA="$PROJECT_ROOT/packaging/brew/$PKG_NAME.rb"

if git -C "$PROJECT_ROOT" rev-parse HEAD >/dev/null 2>&1; then
    ( cd "$PROJECT_ROOT" && git archive --format=tar.gz \
        --prefix="${PKG_NAME}-${PKG_VERSION}/" -o "$TARBALL" HEAD )
else
    echo ">> no git commit yet; tarball from working tree"
    ( cd "$PROJECT_ROOT" && tar czf "$TARBALL" \
        --transform "s,^\.,${PKG_NAME}-${PKG_VERSION}," \
        --exclude=./.git --exclude=./target \
        --exclude=./packaging/dist . )
fi

if command -v sha256sum >/dev/null; then
    SHA="$(sha256sum "$TARBALL" | cut -d' ' -f1)"
else
    SHA="$(shasum -a 256 "$TARBALL" | cut -d' ' -f1)"
fi

sed -i.bak -E \
    -e "s#archive/refs/tags/v[0-9.]+\.tar\.gz#archive/refs/tags/${TAG}.tar.gz#" \
    -e "s/sha256 \"[^\"]*\"/sha256 \"${SHA}\"/" \
    "$FORMULA"
rm -f "$FORMULA.bak"

echo ">> $FORMULA updated"
echo "   url tag : $TAG"
echo "   sha256  : $SHA"
echo "   (sha is for the git-archive tarball at $TARBALL;"
echo "    GitHub's release tarball sha will differ — regenerate after tagging if needed)"
