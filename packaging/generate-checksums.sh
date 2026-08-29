#!/usr/bin/env bash
# Writes SHA256SUMS.txt over the release artifacts in dist/. When
# LSB_RELEASE_SIGNING_KEY and LSB_RELEASE_TAG are set, also writes the signed
# manifest required by install.sh.
#
# Usage: packaging/generate-checksums.sh [dist-dir]

set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd -- "$SCRIPT_DIR/.." && pwd)"
DIST_ROOT="${1:-$REPO_ROOT/dist}"
SUMS_NAME="SHA256SUMS.txt"
SIGNATURE_NAME="$SUMS_NAME.minisig"
SIGNING_KEY="${LSB_RELEASE_SIGNING_KEY:-}"
RELEASE_TAG="${LSB_RELEASE_TAG:-}"
PUBLIC_KEY="${LSB_RELEASE_PUBLIC_KEY:-$REPO_ROOT/release.pub}"

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

rm -f "$DIST_ROOT/$SIGNATURE_NAME"
if [[ -n "$SIGNING_KEY" || -n "$RELEASE_TAG" ]]; then
    [[ -n "$SIGNING_KEY" && -n "$RELEASE_TAG" ]] \
        || fail "LSB_RELEASE_SIGNING_KEY and LSB_RELEASE_TAG must be set together"
    [[ -r "$SIGNING_KEY" ]] || fail "release signing key is not readable: $SIGNING_KEY"
    [[ -r "$PUBLIC_KEY" ]] || fail "release public key is not readable: $PUBLIC_KEY"
    [[ "$RELEASE_TAG" =~ ^v[0-9]+\.[0-9]+\.[0-9]+([.+-][0-9A-Za-z.-]+)?$ ]] \
        || fail "invalid release tag: $RELEASE_TAG"
    command -v minisign >/dev/null 2>&1 || fail "minisign is required to sign the checksum manifest"

    minisign -S -s "$SIGNING_KEY" \
        -m "$DIST_ROOT/$SUMS_NAME" \
        -x "$DIST_ROOT/$SIGNATURE_NAME" \
        -t "Linux Soundboard release $RELEASE_TAG" >/dev/null \
        || fail "could not sign $SUMS_NAME"

    trusted_comment="$(minisign -V -H -Q -p "$PUBLIC_KEY" \
        -m "$DIST_ROOT/$SUMS_NAME" \
        -x "$DIST_ROOT/$SIGNATURE_NAME" 2>/dev/null)" \
        || fail "the generated signature does not match release.pub"
    [[ "$trusted_comment" == "Linux Soundboard release $RELEASE_TAG" ]] \
        || fail "the generated signature is not bound to $RELEASE_TAG"
fi

printf 'Wrote %s over %d artifact(s):\n' "$DIST_ROOT/$SUMS_NAME" "${#assets[@]}"
printf '  %s\n' "${assets[@]}"
[[ -f "$DIST_ROOT/$SIGNATURE_NAME" ]] \
    && printf 'Signed %s as %s\n' "$SUMS_NAME" "$DIST_ROOT/$SIGNATURE_NAME"
