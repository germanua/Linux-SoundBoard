#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd -- "$SCRIPT_DIR/.." && pwd)"
CARGO_ABOUT_VERSION="${CARGO_ABOUT_VERSION:-0.8.4}"
OUTPUT="$REPO_ROOT/THIRD_PARTY_NOTICES.html"

if ! command -v cargo-about >/dev/null 2>&1; then
    echo "cargo-about is required." >&2
    echo "Install it with: cargo install cargo-about --version $CARGO_ABOUT_VERSION --locked" >&2
    exit 1
fi

actual_version="$(cargo about --version | awk '{print $2}')"
if [[ "$actual_version" != "$CARGO_ABOUT_VERSION" ]]; then
    echo "cargo-about $CARGO_ABOUT_VERSION is required; found $actual_version." >&2
    exit 1
fi

cargo about generate \
    --config "$REPO_ROOT/about.toml" \
    --manifest-path "$REPO_ROOT/Cargo.toml" \
    --workspace \
    --locked \
    --fail \
    --output-file "$OUTPUT" \
    "$REPO_ROOT/about.hbs"

# Upstream license files occasionally contain trailing spaces. Normalizing
# horizontal whitespace keeps the committed generated artifact diff-clean.
sed -i 's/[[:blank:]]\+$//' "$OUTPUT"

echo "Generated $OUTPUT"
