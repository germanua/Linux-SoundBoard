#!/usr/bin/env bash
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
mkdir -p "$WORK/dist"
APPIMAGE="$WORK/dist/linux-soundboard-2.4.7-x86_64.AppImage"
printf 'fake-appimage-payload' > "$APPIMAGE"
cat > "$WORK/summary" <<'SUMMARY'
headline: Linux Soundboard 2.4.7
- Fix update handling
- Harden release verification
SUMMARY
python3 "$REPO_ROOT/packaging/generate-update-metadata.py" \
    --version 2.4.7 \
    --tag v2.4.7 \
    --published-at 2026-10-01T00:00:00Z \
    --appimage "$APPIMAGE" \
    --summary-file "$WORK/summary" \
    --output "$WORK/dist/update.json"
python3 - "$WORK/dist/update.json" "$(stat -c '%s' "$APPIMAGE")" <<'PY'
import json
import sys
payload = json.load(open(sys.argv[1], encoding="utf-8"))
assert payload["schema"] == 1
assert payload["version"] == "2.4.7"
assert payload["tag"] == "v2.4.7"
assert payload["channel"] == "stable"
assert payload["minimum_updater_version"] == "2.4.6"
assert payload["appimage"]["name"] == "linux-soundboard-2.4.7-x86_64.AppImage"
assert payload["appimage"]["size"] == int(sys.argv[2])
assert payload["summary"] == ["Fix update handling", "Harden release verification"]
PY
bash "$REPO_ROOT/packaging/generate-checksums.sh" "$WORK/dist" >/dev/null
grep -Eq '^[0-9a-f]{64}  update\.json$' "$WORK/dist/SHA256SUMS.txt"
printf 'ok   - update metadata is deterministic and checksum-covered\n'
printf '\n1 passed, 0 failed\n'
