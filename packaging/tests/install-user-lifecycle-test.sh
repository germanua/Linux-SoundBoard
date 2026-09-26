#!/usr/bin/env bash
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

BUNDLE="$WORK/bundle"
HOME_DIR="$WORK/home"
STUB="$WORK/bin"
mkdir -p "$BUNDLE" "$HOME_DIR" "$STUB"

cp "$REPO_ROOT/packaging/linux/install-user.sh" "$BUNDLE/install-user.sh"
cp "$REPO_ROOT/packaging/linux/app-meta.sh" "$BUNDLE/app-meta.sh"
cp "$REPO_ROOT/packaging/linux/install-swhkd-helper.sh" "$BUNDLE/install-swhkd-helper.sh"
cp "$REPO_ROOT/packaging/linux/build-swhkd-locked.sh" "$BUNDLE/build-swhkd-locked.sh"
cp "$REPO_ROOT/packaging/linux/swhkd-Cargo.lock.pinned" "$BUNDLE/swhkd-Cargo.lock.pinned"
cp -a "$REPO_ROOT/src/resources/icons" "$BUNDLE/icons"
chmod +x "$BUNDLE/install-user.sh" "$BUNDLE/install-swhkd-helper.sh" "$BUNDLE/build-swhkd-locked.sh"
for legal in LICENSE NOTICE.md THIRDPARTY_LICENSES.md THIRD_PARTY_NOTICES.html COMMERCIAL-LICENSE.md; do
    cp "$REPO_ROOT/$legal" "$BUNDLE/$legal"
done

cat > "$STUB/systemctl" <<'EOF'
#!/usr/bin/env bash
exit 0
EOF
cat > "$STUB/wpctl" <<'EOF'
#!/usr/bin/env bash
if [[ "${1:-}" == "status" ]]; then
    printf 'Audio\n  Sources:\n    linuxsoundboard.virtual_mic\n'
fi
exit 0
EOF
for cmd in pw-cli pactl gtk-update-icon-cache update-desktop-database sudo; do
    printf '#!/usr/bin/env bash\nexit 1\n' > "$STUB/$cmd"
    chmod +x "$STUB/$cmd"
done
chmod +x "$STUB/systemctl" "$STUB/wpctl"
FAKE_BIN="$WORK/linux-soundboard"
printf '#!/usr/bin/env bash\nexit 0\n' > "$FAKE_BIN"
chmod +x "$FAKE_BIN"

export HOME="$HOME_DIR"
export XDG_DATA_HOME="$HOME/.local/share"
export XDG_CONFIG_HOME="$HOME/.config"
export XDG_STATE_HOME="$HOME/.local/state"
export XDG_CACHE_HOME="$HOME/.cache"
export PATH="$STUB:/usr/bin:/bin"
export LSB_INSTALL_VERSION=2.4.5

"$BUNDLE/install-user.sh" install "$FAKE_BIN" >/dev/null
INSTALL_ROOT="$HOME/.local/opt/linux-soundboard"
[[ -x "$INSTALL_ROOT/linux-soundboard" ]]
[[ -x "$INSTALL_ROOT/install-user.sh" ]]
[[ -f "$INSTALL_ROOT/app-meta.sh" ]]
[[ -f "$INSTALL_ROOT/installer-icons/64x64/apps/com.linuxsoundboard.app.png" ]]

echo "ok   - install persists the lifecycle script, metadata, and repair icons"
[[ "$(stat -c '%a' "$XDG_STATE_HOME/linux-soundboard/install-user")" == 700 ]]
[[ "$(stat -c '%a' "$XDG_STATE_HOME/linux-soundboard/install-user/manifest.tsv")" == 600 ]]
[[ "$(stat -c '%a' "$INSTALL_ROOT/.installed-version")" == 600 ]]
find "$XDG_STATE_HOME/linux-soundboard/install-user/snapshots" -type f -print0 | while IFS= read -r -d '' file; do
    [[ "$(stat -c '%a' "$file")" == 600 ]]
done
echo "ok   - lifecycle state and version files are private"

UPDATED_BIN="$WORK/linux-soundboard-updated"
printf '#!/usr/bin/env bash\nprintf updated\n' > "$UPDATED_BIN"
chmod +x "$UPDATED_BIN"
UPDATED_SHA="$(sha256sum "$UPDATED_BIN" | awk '{print $1}')"
LSB_INSTALL_VERSION=2.4.6 LSB_INSTALL_EXPECTED_SHA256="$UPDATED_SHA" \
    "$INSTALL_ROOT/install-user.sh" install "$UPDATED_BIN" >/dev/null
[[ "$(sha256sum "$INSTALL_ROOT/linux-soundboard" | awk '{print $1}')" == "$UPDATED_SHA" ]]
[[ "$(cat "$INSTALL_ROOT/.installed-version")" == 2.4.6 ]]
echo "ok   - authenticated update atomically installs the expected binary"

BEFORE_BAD="$(sha256sum "$INSTALL_ROOT/linux-soundboard" | awk '{print $1}')"
BAD_BIN="$WORK/linux-soundboard-bad"
printf '#!/usr/bin/env bash\nprintf bad\n' > "$BAD_BIN"
chmod +x "$BAD_BIN"
if LSB_INSTALL_VERSION=2.4.7 LSB_INSTALL_EXPECTED_SHA256="$(printf wrong | sha256sum | awk '{print $1}')" \
    "$INSTALL_ROOT/install-user.sh" install "$BAD_BIN" >/dev/null 2>&1; then
    printf 'authenticated install unexpectedly accepted a bad hash\n' >&2
    exit 1
fi
[[ "$(sha256sum "$INSTALL_ROOT/linux-soundboard" | awk '{print $1}')" == "$BEFORE_BAD" ]]
echo "ok   - hash mismatch leaves the stable binary unchanged"

DEPLOYED_ICON="$XDG_DATA_HOME/icons/hicolor/64x64/apps/com.linuxsoundboard.app.png"
rm -f "$DEPLOYED_ICON"
"$INSTALL_ROOT/install-user.sh" repair "$INSTALL_ROOT/linux-soundboard" >/dev/null
[[ -f "$DEPLOYED_ICON" ]]
echo "ok   - offline repair restores a deleted icon from persisted resources"

STATUS_OUT="$WORK/status.txt"
"$INSTALL_ROOT/install-user.sh" status > "$STATUS_OUT"
grep -Fq 'Linux Soundboard status:' "$STATUS_OUT"
grep -Fq "$INSTALL_ROOT/linux-soundboard" "$STATUS_OUT"
echo "ok   - persisted lifecycle script reports installed status"

"$INSTALL_ROOT/install-user.sh" remove --yes --keep-data --keep-current-default-source >/dev/null
[[ ! -e "$INSTALL_ROOT/linux-soundboard" ]]
[[ ! -e "$INSTALL_ROOT/install-user.sh" ]]
[[ ! -e "$INSTALL_ROOT/app-meta.sh" ]]
[[ ! -e "$INSTALL_ROOT/installer-icons" ]]
echo "ok   - uninstall removes the persisted lifecycle assets"

printf '\n7 passed, 0 failed\n'
