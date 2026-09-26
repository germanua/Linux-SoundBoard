#!/usr/bin/env bash





set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd -- "$SCRIPT_DIR/.." && pwd)"
DIST_ROOT="${LSB_DIST_ROOT:-$REPO_ROOT/dist}"
SUMS_NAME="SHA256SUMS.txt"
SIGNATURE_NAME="$SUMS_NAME.minisig"
SIGNING_KEY="${LSB_RELEASE_SIGNING_KEY:-}"
RELEASE_TAG="${LSB_RELEASE_TAG:-}"
PUBLIC_KEY="${LSB_RELEASE_PUBLIC_KEY:-$REPO_ROOT/release.pub}"

fail() { printf 'generate-checksums: %s\n' "$1" >&2; exit 1; }

ONLY=()
while [[ $# -gt 0 ]]; do
    case "$1" in
        --only)
            shift
            [[ $# -gt 0 ]] || fail "--only needs a file name"
            ONLY+=("$1")
            ;;
        -*)
            fail "unknown option: $1"
            ;;
        *)
            DIST_ROOT="$1"
            ;;
    esac
    shift
done

[[ -d "$DIST_ROOT" ]] || fail "no such directory: $DIST_ROOT"


if command -v sha256sum >/dev/null 2>&1; then
    hash_files() { sha256sum "$@"; }
elif command -v shasum >/dev/null 2>&1; then
    hash_files() { shasum -a 256 "$@"; }
else
    fail "sha256sum or shasum is required, and neither is installed."
fi

if ((${#ONLY[@]} > 0)); then
    assets=()
    for name in "${ONLY[@]}"; do
        [[ -f "$DIST_ROOT/$name" ]] || fail "missing asset: $name"
        assets+=("$name")
    done
    mapfile -t assets < <(printf '%s\n' "${assets[@]}" | sort)
else
    mapfile -t assets < <(
        find "$DIST_ROOT" -maxdepth 1 -type f \
            \( -name '*.tar.gz' -o -name '*.deb' -o -name '*.rpm' -o -name '*.AppImage' -o -name 'install.sh' -o -name 'update.json' \) \
            -printf '%f\n' | sort
    )
fi

((${#assets[@]} > 0)) || fail "no release artifacts found in $DIST_ROOT"

(
    cd "$DIST_ROOT"

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
if [[ -f "$DIST_ROOT/$SIGNATURE_NAME" ]]; then
    printf 'Signed %s as %s\n' "$SUMS_NAME" "$DIST_ROOT/$SIGNATURE_NAME"
fi
