#!/usr/bin/env bash
set -uo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
SUT="$REPO_ROOT/packaging/linux/package-appimage.sh"

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
PASS=0
FAIL=0

ok() { PASS=$((PASS + 1)); printf 'ok   - %s\n' "$1"; }
bad() { FAIL=$((FAIL + 1)); printf 'FAIL - %s\n' "$1"; }

{
    sed -n '/^sha256_file()/,/^}/p' "$REPO_ROOT/packaging/common.sh"
    sed -n '/^download_if_missing()/,/^}/p' "$SUT"
    sed -n '/^download_verified()/,/^}/p' "$SUT"
} >"$WORK/functions.sh"
source "$WORK/functions.sh"

if grep -qF 'download_verified "$GTK_PLUGIN_URL" "$GTK_PLUGIN_SOURCE" "$GTK_PLUGIN_SHA256"' "$SUT" \
    && ! grep -qF 'download_if_missing "$GTK_PLUGIN_URL"' "$SUT"; then
    ok "the GTK plugin is authenticated before it is copied or executed"
else
    bad "the GTK plugin must be authenticated before it is copied or executed"
fi

if grep -qF 'GTK_PLUGIN_SHA256="' "$SUT"; then
    ok "the GTK plugin digest is pinned in the script"
else
    bad "the GTK plugin digest must be pinned in the script"
fi

if grep -qF 'if [[ -x "$LINUXDEPLOY_BIN" ]]; then' "$SUT"; then
    bad "a pre-existing extracted linuxdeploy must never be reused"
else
    ok "the extracted linuxdeploy root is rebuilt from the verified AppImage"
fi

good="$WORK/cached-good"
printf 'payload\n' >"$good"
good_sha="$(sha256_file "$good")"
if (set -euo pipefail; download_verified "http://127.0.0.1:1/unused" "$good" "$good_sha") >/dev/null 2>&1; then
    ok "a cached file matching its pinned digest is accepted without refetching"
else
    bad "a cached file matching its pinned digest must be accepted"
fi

poisoned="$WORK/cached-poisoned"
printf 'poison\n' >"$poisoned"
if (set -euo pipefail; download_verified "http://127.0.0.1:1/unused" "$poisoned" "$good_sha") >/dev/null 2>&1; then
    bad "a poisoned cached file must be refused"
else
    ok "a poisoned cached file is refused before it can be used"
fi
if [[ -e "$poisoned" ]]; then
    bad "a poisoned cached file must be deleted on mismatch"
else
    ok "a poisoned cached file is deleted on mismatch"
fi

printf '\n%d passed, %d failed\n' "$PASS" "$FAIL"
[[ "$FAIL" -eq 0 ]]
