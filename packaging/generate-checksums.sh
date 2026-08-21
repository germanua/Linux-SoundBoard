#!/usr/bin/env bash
# Writes SHA256SUMS.txt over the release artifacts in dist/. install.sh checks
# every download it makes against that list, so it has to be uploaded with the
# release, after the last artifact is built.
#
# Usage: packaging/generate-checksums.sh [dist-dir]

set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd -- "$SCRIPT_DIR/.." && pwd)"
DIST_ROOT="${1:-$REPO_ROOT/dist}"
SUMS_NAME="SHA256SUMS.txt"

fail() { printf 'generate-checksums: %s\n' "$1" >&2; exit 1; }

[[ -d "$DIST_ROOT" ]] || fail "no such directory: $DIST_ROOT"

# Both write "<hash>  <name>", which is what install.sh parses.
if command -v sha256sum >/dev/null 2>&1; then
    hash_files() { sha256sum "$@"; }
elif command -v shasum >/dev/null 2>&1; then
    hash_files() { shasum -a 256 "$@"; }
else
    fail "sha256sum or shasum is required, and neither is installed."
fi

# Release assets only: the AppDir, the desktop and metainfo files, and the
# downloaded build tools also live in dist/ and are never published.
mapfile -t assets < <(
    find "$DIST_ROOT" -maxdepth 1 -type f \
        \( -name '*.tar.gz' -o -name '*.deb' -o -name '*.rpm' -o -name '*.AppImage' \) \
        -printf '%f\n' | sort
)

((${#assets[@]} > 0)) || fail "no release artifacts found in $DIST_ROOT"

(
    cd "$DIST_ROOT"
    # Names stay bare so the list matches whatever the asset is downloaded as.
    hash_files "${assets[@]}" > "$SUMS_NAME"
)

printf 'Wrote %s over %d artifact(s):\n' "$DIST_ROOT/$SUMS_NAME" "${#assets[@]}"
printf '  %s\n' "${assets[@]}"
