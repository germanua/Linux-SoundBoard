#!/usr/bin/env bash
set -euo pipefail
REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
STUB="$WORK/bin"
mkdir -p "$STUB"
INSTALLER="$WORK/good-install.sh"
cat > "$INSTALLER" <<'INNER'
#!/usr/bin/env bash
printf '%s\n' "$@" > "$BOOTSTRAP_MARKER"
INNER
HASH="$(sha256sum "$INSTALLER" | awk '{print $1}')"
cat > "$STUB/curl" <<'INNER'
#!/usr/bin/env bash
set -euo pipefail
url=""
out=""
while (($#)); do
    case "$1" in
        -o) out="$2"; shift 2 ;;
        http*) url="$1"; shift ;;
        *) shift ;;
    esac
done
if [[ -z "$out" ]]; then
    printf '{"tag_name":"v9.9.9","assets":[{"browser_download_url":"https://example/SHA256SUMS.txt"},{"browser_download_url":"https://example/SHA256SUMS.txt.minisig"},{"browser_download_url":"https://example/install.sh"}]}'
    exit 0
fi
case "$url" in
    */SHA256SUMS.txt) printf '%s  install.sh\n' "$TEST_HASH" > "$out" ;;
    */SHA256SUMS.txt.minisig) printf signature > "$out" ;;
    */install.sh)
        cp "$TEST_INSTALLER" "$out"
        [[ "${TAMPER:-0}" == 0 ]] || printf tampered >> "$out"
        ;;
    *) exit 1 ;;
esac
INNER
cat > "$STUB/minisign" <<'INNER'
#!/usr/bin/env bash
[[ "${MINISIGN_FAIL:-0}" == 0 ]] || exit 1
printf 'Linux Soundboard release v9.9.9\n'
INNER
chmod +x "$STUB/curl" "$STUB/minisign"
export PATH="$STUB:/usr/bin:/bin"
export TEST_HASH="$HASH"
export TEST_INSTALLER="$INSTALLER"
export BOOTSTRAP_MARKER="$WORK/executed"
bash "$REPO_ROOT/bootstrap-install.sh" report --output "$WORK/report.txt"
[[ -f "$BOOTSTRAP_MARKER" ]]
printf 'ok   - signed release installer executes after verification\n'
mapfile -t forwarded < "$BOOTSTRAP_MARKER"
[[ "${forwarded[0]}" == report && "${forwarded[1]}" == --output && "${forwarded[2]}" == "$WORK/report.txt" ]]
printf 'ok   - lifecycle subcommands and arguments reach the signed installer unchanged\n'
rm -f "$BOOTSTRAP_MARKER"
if TAMPER=1 bash "$REPO_ROOT/bootstrap-install.sh" >/dev/null 2>&1; then exit 1; fi
[[ ! -e "$BOOTSTRAP_MARKER" ]]
printf 'ok   - modified installer is rejected before execution\n'
if MINISIGN_FAIL=1 bash "$REPO_ROOT/bootstrap-install.sh" >/dev/null 2>&1; then exit 1; fi
[[ ! -e "$BOOTSTRAP_MARKER" ]]
printf 'ok   - invalid release signature is rejected\n'
printf '\n4 passed, 0 failed\n'
