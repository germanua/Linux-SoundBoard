#!/usr/bin/env bash
set -euo pipefail

REPO_ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)"
INSTALLER="$REPO_ROOT/install.sh"
PREFLIGHT="$REPO_ROOT/packaging/linux/appimage-preflight-check.sh"
PASS=0
FAIL=0

ok() { printf 'ok   - %s\n' "$1"; PASS=$((PASS + 1)); }
bad() { printf 'not ok - %s\n' "$1"; FAIL=$((FAIL + 1)); }

installer_accepts() {
    local version=$1 arch=$2 home
    home="$(mktemp -d)"
    TEST_VERSION="$version" TEST_ARCH="$arch" HOME="$home" bash -c '
        source "$1" status >/dev/null 2>&1 || true
        getconf(){ printf "glibc %s\n" "$TEST_VERSION"; }
        uname(){ printf "%s\n" "$TEST_ARCH"; }
        fail(){ return 97; }
        ensure_appimage_host_compatibility
    ' _ "$INSTALLER"
    local rc=$?
    rm -rf "$home"
    return "$rc"
}

if installer_accepts 2.39 x86_64; then ok "installer accepts the release ABI baseline"; else bad "installer must accept x86_64 glibc 2.39"; fi
if installer_accepts 2.38 x86_64; then bad "installer must reject glibc below 2.39"; else ok "installer rejects glibc below 2.39"; fi
if installer_accepts 2.44 aarch64; then bad "installer must reject unsupported AppImage architectures"; else ok "installer rejects unsupported AppImage architectures"; fi

STUB="$(mktemp -d)"
trap 'rm -rf "$STUB"' EXIT
for cmd in uname getconf fusermount pgrep pactl ldconfig; do : >"$STUB/$cmd"; chmod +x "$STUB/$cmd"; done
cat >"$STUB/uname" <<'INNER'
#!/bin/sh
printf '%s\n' "${TEST_ARCH:-x86_64}"
INNER
cat >"$STUB/getconf" <<'INNER'
#!/bin/sh
printf 'glibc %s\n' "${TEST_GLIBC:-2.39}"
INNER
cat >"$STUB/ldconfig" <<'INNER'
#!/bin/sh
printf '%s\n' 'libfribidi.so.0 => /usr/lib/libfribidi.so.0' 'libharfbuzz.so.0 => /usr/lib/libharfbuzz.so.0' 'libfontconfig.so.1 => /usr/lib/libfontconfig.so.1' 'libwayland-client.so.0 => /usr/lib/libwayland-client.so.0' 'libfreetype.so.6 => /usr/lib/libfreetype.so.6' 'libX11-xcb.so.1 => /usr/lib/libX11-xcb.so.1' 'libX11.so.6 => /usr/lib/libX11.so.6' 'libpipewire-0.3.so.0 => /usr/lib/libpipewire-0.3.so.0' 'libcom_err.so.2 => /usr/lib/libcom_err.so.2' 'libgpg-error.so.0 => /usr/lib/libgpg-error.so.0'
INNER
for cmd in fusermount pgrep pactl; do printf '#!/bin/sh\nexit 0\n' >"$STUB/$cmd"; done
chmod +x "$STUB"/*

if TEST_GLIBC=2.39 TEST_ARCH=x86_64 PATH="$STUB:/usr/bin:/bin" bash "$PREFLIGHT" >/dev/null 2>&1; then ok "AppImage preflight accepts the release ABI baseline"; else bad "AppImage preflight must accept x86_64 glibc 2.39"; fi
if TEST_GLIBC=2.38 TEST_ARCH=x86_64 PATH="$STUB:/usr/bin:/bin" bash "$PREFLIGHT" >/dev/null 2>&1; then bad "AppImage preflight must reject glibc below 2.39"; else ok "AppImage preflight rejects glibc below 2.39"; fi
if TEST_GLIBC=2.44 TEST_ARCH=aarch64 PATH="$STUB:/usr/bin:/bin" bash "$PREFLIGHT" >/dev/null 2>&1; then bad "AppImage preflight must reject unsupported architectures"; else ok "AppImage preflight rejects unsupported architectures"; fi

printf '#!/bin/sh\nexit 1\n' >"$STUB/pgrep"
warning_output="$(TEST_GLIBC=2.39 TEST_ARCH=x86_64 XDG_RUNTIME_DIR="$STUB/runtime-none" PATH="$STUB:/usr/bin:/bin" bash "$PREFLIGHT" 2>&1)"
warning_rc=$?
if [[ "$warning_rc" -eq 0 && "$warning_output" == *"No PipeWire or PulseAudio daemon detected"* ]]; then ok "AppImage preflight reports audio warnings without aborting"; else bad "AppImage preflight must aggregate non-fatal audio warnings"; fi
printf '#!/bin/sh\nexit 0\n' >"$STUB/pgrep"

cat >"$STUB/ldconfig" <<'INNER'
#!/bin/sh
printf '%s\n' 'libfribidi.so.0 => /usr/lib/libfribidi.so.0' 'libharfbuzz.so.0 => /usr/lib/libharfbuzz.so.0' 'libfontconfig.so.1 => /usr/lib/libfontconfig.so.1' 'libwayland-client.so.0 => /usr/lib/libwayland-client.so.0' 'libfreetype.so.6 => /usr/lib/libfreetype.so.6' 'libX11-xcb.so.1 => /usr/lib/libX11-xcb.so.1' 'libX11.so.6 => /usr/lib/libX11.so.6' 'libpipewire-0.3.so.0 => /usr/lib/libpipewire-0.3.so.0' 'libcom_err.so.2 => /usr/lib/libcom_err.so.2'
INNER
set +e
missing_output="$(TEST_GLIBC=2.39 TEST_ARCH=x86_64 PATH="$STUB:/usr/bin:/bin" bash "$PREFLIGHT" 2>&1)"
missing_rc=$?
set -e
if [[ "$missing_rc" -ne 0 && "$missing_output" == *"libgpg-error.so.0"* ]]; then ok "AppImage preflight reports missing host runtime libraries"; else bad "AppImage preflight must report missing host runtime libraries"; fi

printf '\n%d passed, %d failed\n' "$PASS" "$FAIL"
[[ "$FAIL" -eq 0 ]]
