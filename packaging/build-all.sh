#!/usr/bin/env bash
# Build every package whose tooling is available on this host.
set -euo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

if command -v dpkg-deb >/dev/null; then
    "$HERE/deb/build-deb.sh"
else
    echo "-- skipping deb (dpkg-deb missing)"
fi

if command -v rpmbuild >/dev/null; then
    "$HERE/rpm/build-rpm.sh"
else
    echo "-- skipping rpm (rpmbuild missing)"
fi

"$HERE/brew/update-formula.sh"
echo ">> done; artifacts in $HERE/dist"
