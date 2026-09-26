#!/usr/bin/env bash
set -euo pipefail

CHECKOUT="${1:-}"
if [[ -z "$CHECKOUT" || ! -f "$CHECKOUT/Cargo.toml" ]]; then
    printf 'build-swhkd-locked: usage: %s <swhkd-checkout-dir>\n' "$0" >&2
    exit 2
fi

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
PINNED_LOCK="$SCRIPT_DIR/swhkd-Cargo.lock.pinned"
PINNED_LOCK_SHA256="b0ae3f27d0e371b76a5f033ab7137a7a5808d425ab588c38b8a0fa10231c81b1"

ENV_LOGGER_PIN="0.11"

fail() {
    printf 'build-swhkd-locked: %s\n' "$1" >&2
    exit 1
}

[[ -f "$PINNED_LOCK" ]] || fail "missing pinned lockfile at $PINNED_LOCK"
actual="$(sha256sum "$PINNED_LOCK" | awk '{print $1}')"
[[ "$actual" == "$PINNED_LOCK_SHA256" ]] \
    || fail "pinned lockfile sha256 is $actual, expected $PINNED_LOCK_SHA256"

pin_member_manifest() {
    local manifest="$1"
    grep -qF "env_logger = \"$ENV_LOGGER_PIN\"" "$manifest" && return 0
    grep -qF 'env_logger = "0.9.0"' "$manifest" \
        || fail "unexpected env_logger requirement in $manifest"
    sed -i "s|^env_logger = \"0.9.0\"\$|env_logger = \"$ENV_LOGGER_PIN\"|" "$manifest"
}

if command -v rustup >/dev/null 2>&1 && rustup toolchain list 2>/dev/null | grep -q '^1\.85\.0'; then
    export RUSTUP_TOOLCHAIN=1.85.0
fi
command -v cargo >/dev/null 2>&1 || fail "cargo is not installed"
cargo_version="$(cargo --version | awk '{print $2}')"
printf '1.78.0\n%s\n' "$cargo_version" | sort -V -C \
    || fail "cargo $cargo_version cannot read lockfile v4; install a newer toolchain (e.g. rustup)"

cp -- "$PINNED_LOCK" "$CHECKOUT/Cargo.lock"
pin_member_manifest "$CHECKOUT/swhkd/Cargo.toml"
pin_member_manifest "$CHECKOUT/swhks/Cargo.toml"

( cd "$CHECKOUT/swhkd" && cargo build --release --locked --features no_rfkill )
( cd "$CHECKOUT/swhks" && cargo build --release --locked )

binary="$CHECKOUT/target/release/swhkd"
[[ -f "$binary" ]] || fail "swhkd binary was not produced at $binary"
if LC_ALL=C grep -aF -e '/dev/rfkill' -e 'SW_RFKILL_ALL' "$binary" >/dev/null 2>&1; then
    fail "built swhkd still contains rfkill support"
fi
