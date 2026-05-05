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
