#!/usr/bin/env bash

require_cmd() {
    local cmd="$1"
    local hint="${2:-}"

    if command -v "$cmd" >/dev/null 2>&1; then
        return 0
    fi

    echo "Error: $cmd not found." >&2
    if [[ -n "$hint" ]]; then
        echo "$hint" >&2
    fi
    return 1
}

sha256_file() {
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$1" | awk '{print $1}'
    elif command -v shasum >/dev/null 2>&1; then
        shasum -a 256 "$1" | awk '{print $1}'
    elif command -v openssl >/dev/null 2>&1; then
        openssl dgst -sha256 "$1" | awk '{print $NF}'
    else
        return 1
    fi
}

cargo_version_from_manifest() {
    local manifest_path="$1"
    local version=""

    version="$(sed -n 's/^version = "\(.*\)"$/\1/p' "$manifest_path" | head -n 1)"
    if [[ -z "$version" ]]; then
        echo "Error: could not read package version from $manifest_path" >&2
        return 1
    fi

    printf '%s\n' "$version"
}
