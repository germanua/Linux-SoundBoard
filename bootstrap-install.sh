#!/usr/bin/env bash
set -euo pipefail

REPO="germanua/Linux-SoundBoard"
TESTING_TAG="v2.4.7-testing.3"
PUBLIC_KEY="RWTEtl8HnYs8Fg7BOmAXxuC9PUxqlamX5+C0w4FgUUxXGB6DipbZl8tY"
MINISIGN_URL="https://github.com/jedisct1/minisign/releases/download/0.12/minisign-0.12-linux.tar.gz"
MINISIGN_SHA256="9a599b48ba6eb7b1e80f12f36b94ceca7c00b7a5173c95c3efc88d9822957e73"
WORK_DIR="$(mktemp -d)"
trap 'rm -rf "$WORK_DIR"' EXIT

if command -v curl >/dev/null 2>&1; then
    fetch() { curl --proto '=https' --tlsv1.2 -fsSL "$1" -o "$2"; }
    fetch_stdout() { curl --proto '=https' --tlsv1.2 -fsSL "$1"; }
elif command -v wget >/dev/null 2>&1; then
    fetch() { wget -qO "$2" "$1"; }
    fetch_stdout() { wget -qO- "$1"; }
else
    printf 'curl or wget is required\n' >&2
    exit 1
fi

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

asset_url() {
    printf '%s' "$1" \
        | grep -oE '"browser_download_url":[[:space:]]*"[^"]+"' \
        | sed -E 's/.*"([^"]+)"/\1/' \
        | grep -E "/$2$" \
        | head -n 1
}

tag="$TESTING_TAG"
install_args=()
while [[ $# -gt 0 ]]; do
    case "$1" in
        --version)
            [[ -n "${2:-}" ]] || { printf '%s\n' '--version requires a release tag' >&2; exit 1; }
            tag="$2"
            shift 2
            ;;
        --version=*)
            tag="${1#--version=}"
            shift
            ;;
        *)
            install_args+=("$1")
            shift
            ;;
    esac
done
[[ "$tag" =~ ^v[0-9]+\.[0-9]+\.[0-9]+([.+-][0-9A-Za-z.-]+)?$ ]] || { printf 'invalid release tag\n' >&2; exit 1; }
printf 'Installing Linux Soundboard testing release %s...\n' "$tag"
release_json="$(fetch_stdout "https://api.github.com/repos/$REPO/releases/tags/$tag")"

sums_url="$(asset_url "$release_json" 'SHA256SUMS\.txt')"
sig_url="$(asset_url "$release_json" 'SHA256SUMS\.txt\.minisig')"
installer_url="$(asset_url "$release_json" 'install\.sh')"
[[ -n "$sums_url" && -n "$sig_url" && -n "$installer_url" ]] || { printf 'signed installer assets are missing\n' >&2; exit 1; }

fetch "$sums_url" "$WORK_DIR/SHA256SUMS.txt"
fetch "$sig_url" "$WORK_DIR/SHA256SUMS.txt.minisig"
fetch "$installer_url" "$WORK_DIR/install.sh"

if command -v minisign >/dev/null 2>&1; then
    verifier="$(command -v minisign)"
else
    archive="$WORK_DIR/minisign.tar.gz"
    fetch "$MINISIGN_URL" "$archive"
    actual="$(sha256_file "$archive")" || { printf 'SHA-256 tool is required\n' >&2; exit 1; }
    [[ "${actual,,}" == "$MINISIGN_SHA256" ]] || { printf 'minisign bootstrap checksum mismatch\n' >&2; exit 1; }
    case "$(uname -m)" in
        x86_64|amd64) arch=x86_64 ;;
        aarch64|arm64) arch=aarch64 ;;
        *) printf 'unsupported architecture\n' >&2; exit 1 ;;
    esac
    tar -xzf "$archive" -C "$WORK_DIR" "minisign-linux/$arch/minisign"
    verifier="$WORK_DIR/minisign-linux/$arch/minisign"
    chmod 700 "$verifier"
fi

trusted_comment="$("$verifier" -V -H -Q -P "$PUBLIC_KEY" -m "$WORK_DIR/SHA256SUMS.txt" -x "$WORK_DIR/SHA256SUMS.txt.minisig" 2>/dev/null)" \
    || { printf 'invalid release signature\n' >&2; exit 1; }
[[ "$trusted_comment" == "Linux Soundboard release $tag" ]] || { printf 'release signature tag mismatch\n' >&2; exit 1; }

expected="$(awk '$2 == "install.sh" || $2 == "*install.sh" { print $1; exit }' "$WORK_DIR/SHA256SUMS.txt")"
[[ "$expected" =~ ^[0-9a-fA-F]{64}$ ]] || { printf 'install.sh is not covered by the signed manifest\n' >&2; exit 1; }
actual="$(sha256_file "$WORK_DIR/install.sh")" || { printf 'SHA-256 tool is required\n' >&2; exit 1; }
[[ "${actual,,}" == "${expected,,}" ]] || { printf 'install.sh checksum mismatch\n' >&2; exit 1; }
chmod 700 "$WORK_DIR/install.sh"
exec bash "$WORK_DIR/install.sh" install --version "$tag" "${install_args[@]}"
