#!/usr/bin/env bash

set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
# Resolved relative to this script at runtime.
# shellcheck disable=SC1091
source "$SCRIPT_DIR/app-meta.sh"

MANAGED_MARKER="managed-by: linux-soundboard"
MANAGED_MARKER_LINE="# $MANAGED_MARKER"
END_MANAGED_MARKER_LINE="# end-managed-by: linux-soundboard"
VIRTUAL_SOURCE_NAME="linuxsoundboard.virtual_mic"
ENGINE_SERVICE_NAME="linux-soundboard-engine.service"
ENGINE_TARGET_NAME="linux-soundboard-engine.target"
PIPEWIRE_CONF_NAME="99-linuxsoundboard.conf"
SYSTEM_PIPEWIRE_CONF="/usr/share/pipewire/pipewire.conf.d/$PIPEWIRE_CONF_NAME"

INSTALL_ROOT="${INSTALL_ROOT:-$HOME/.local/opt/$APP_BINARY}"
INSTALL_BINARY="$INSTALL_ROOT/$APP_BINARY"
INSTALL_HELPER="$INSTALL_ROOT/install-swhkd-helper.sh"
INSTALL_DOC_DIR="$INSTALL_ROOT/docs"
INSTALL_VERSION_FILE="$INSTALL_ROOT/.installed-version"

XDG_DATA_HOME="${XDG_DATA_HOME:-$HOME/.local/share}"
XDG_CONFIG_HOME="${XDG_CONFIG_HOME:-$HOME/.config}"
XDG_STATE_HOME="${XDG_STATE_HOME:-$HOME/.local/state}"
XDG_CACHE_HOME="${XDG_CACHE_HOME:-$HOME/.cache}"

DESKTOP_DIR="$XDG_DATA_HOME/applications"
ICON_THEME_DIR="$XDG_DATA_HOME/icons/hicolor"
SYSTEMD_USER_DIR="$XDG_CONFIG_HOME/systemd/user"
ENGINE_SERVICE="$SYSTEMD_USER_DIR/$ENGINE_SERVICE_NAME"
ENGINE_TARGET="$SYSTEMD_USER_DIR/$ENGINE_TARGET_NAME"
PIPEWIRE_USER_CONF="$XDG_CONFIG_HOME/pipewire/pipewire.conf.d/$PIPEWIRE_CONF_NAME"
PULSE_DEFAULT_PA="$XDG_CONFIG_HOME/pulse/default.pa"

STATE_DIR="$XDG_STATE_HOME/$APP_BINARY/install-user"
BACKUP_DIR="$STATE_DIR/backups"
MANIFEST_FILE="$STATE_DIR/manifest.tsv"
BACKUP_MANIFEST_FILE="$STATE_DIR/backups.tsv"
AUDIO_SNAPSHOT_FILE="$STATE_DIR/preinstall-audio.env"
SNAPSHOT_DIR="$STATE_DIR/snapshots"
SNAPSHOT_KEEP=10

YES=0
KEEP_DATA=0
DEFAULT_SOURCE_POLICY="ask"
# Set to 1 whenever a managed PipeWire/PulseAudio/WirePlumber file is disabled,
# removed, or rewritten, so the audio stack is only restarted when needed.
AUDIO_CONFIG_CHANGED=0

log() {
    printf '[%s] %s\n' "$1" "$2"
}

info() {
    log INFO "$1"
}

warn() {
    log WARN "$1" >&2
}

fail() {
    log ERROR "$1" >&2
    exit 1
}

usage() {
    cat <<EOF
Linux Soundboard user installer

Usage:
  ./install-user.sh
  ./install-user.sh install [binary]
  ./install-user.sh repair [binary]
  ./install-user.sh setup-user
  ./install-user.sh remove [--yes] [--keep-data] [--restore-default-source|--keep-current-default-source]
  ./install-user.sh status
  ./install-user.sh snapshot [event]
  ./install-user.sh snapshot-diff [snapshot.env]
  ./install-user.sh restore-audio
  ./install-user.sh --help

No arguments opens the interactive menu when run from a terminal. In
noninteractive mode, pass an explicit command.

Use 'setup-user' after installing the native DEB/RPM package to enable the
engine service for your account and clean obsolete user-level audio routing,
without redeploying app files the package already owns.
EOF
}

ensure_state_dir() {
    mkdir -p "$STATE_DIR" "$BACKUP_DIR"
    touch "$MANIFEST_FILE" "$BACKUP_MANIFEST_FILE"
}

checksum_file() {
    local path=$1

    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$path" | awk '{print $1}'
    else
        cksum "$path" | awk '{print $1 ":" $2}'
    fi
}

path_in_manifest() {
    local path=$1

    [[ -f "$MANIFEST_FILE" ]] || return 1
    awk -F '\t' -v path="$path" '$1 == "file" && $3 == path { found = 1 } END { exit(found ? 0 : 1) }' "$MANIFEST_FILE"
}

backup_exists_for_path() {
    local path=$1

    [[ -f "$BACKUP_MANIFEST_FILE" ]] || return 1
    awk -F '\t' -v path="$path" '$1 == path { found = 1 } END { exit(found ? 0 : 1) }' "$BACKUP_MANIFEST_FILE"
}

backup_for_path() {
    local path=$1

    [[ -f "$BACKUP_MANIFEST_FILE" ]] || return 1
    awk -F '\t' -v path="$path" '$1 == path { print $2; exit }' "$BACKUP_MANIFEST_FILE"
}

sanitize_backup_name() {
    local path=$1

    printf '%s' "$path" | sed 's#[^A-Za-z0-9._-]#_#g'
}

backup_file_if_needed() {
    local path=$1

    [[ -e "$path" ]] || return 0
    ensure_state_dir

    if path_in_manifest "$path"; then
        return 0
    fi

    if backup_exists_for_path "$path"; then
        return 0
    fi

    local stamp
    local backup_name
    local backup_path
    stamp="$(date +%Y%m%d%H%M%S)"
    backup_name="$(sanitize_backup_name "$path")"
    backup_path="$BACKUP_DIR/$stamp-$backup_name"

    mkdir -p "$BACKUP_DIR"
    cp -p -- "$path" "$backup_path"
    printf '%s\t%s\t%s\n' "$path" "$backup_path" "$(checksum_file "$backup_path")" >>"$BACKUP_MANIFEST_FILE"
}

record_file() {
    local path=$1

    ensure_state_dir
    printf 'file\t%s\t%s\n' "$(checksum_file "$path")" "$path" >>"$MANIFEST_FILE"
}

record_dir_if_new() {
    local path=$1

    ensure_state_dir
    if [[ ! -d "$path" ]]; then
        mkdir -p "$path"
        printf 'dir\t-\t%s\n' "$path" >>"$MANIFEST_FILE"
    else
        mkdir -p "$path"
    fi
}

ensure_parent_dir() {
    local path=$1
    local parent

    parent="$(dirname "$path")"
    record_dir_if_new "$parent"
}

contains_managed_marker() {
    local path=$1

    [[ -f "$path" ]] && grep -Fq "$MANAGED_MARKER" "$path"
}

install_file_from_source() {
    local source=$1
    local dest=$2
    local mode=$3
    local source_real
    local dest_real

    ensure_parent_dir "$dest"
    if [[ -e "$dest" ]]; then
        source_real="$(realpath "$source")"
        dest_real="$(realpath "$dest")"
        if [[ "$source_real" == "$dest_real" ]]; then
            chmod "$mode" "$dest"
            record_file "$dest"
            return 0
        fi
    fi

    backup_file_if_needed "$dest"
    install -m "$mode" "$source" "$dest"
    record_file "$dest"
}

install_file_from_content() {
    local dest=$1
    local mode=$2
    local content=$3
    local tmp

    tmp="$(mktemp)"
    printf '%s' "$content" >"$tmp"
    install_file_from_source "$tmp" "$dest" "$mode"
    rm -f "$tmp"
}

find_existing_path() {
    local candidate

    for candidate in "$@"; do
        if [[ -e "$candidate" ]]; then
            printf '%s\n' "$candidate"
            return 0
        fi
    done

    return 1
}

resolve_binary_source() {
    local explicit_path=${1:-}

    if [[ -n "$explicit_path" ]]; then
        realpath "$explicit_path"
    elif [[ -x "$SCRIPT_DIR/$APP_BINARY" ]]; then
        realpath "$SCRIPT_DIR/$APP_BINARY"
    elif [[ -x "$SCRIPT_DIR/../../target/release/$APP_BINARY" ]]; then
        realpath "$SCRIPT_DIR/../../target/release/$APP_BINARY"
    elif [[ -x "$SCRIPT_DIR/../../src/target/release/$APP_BINARY" ]]; then
        realpath "$SCRIPT_DIR/../../src/target/release/$APP_BINARY"
    elif command -v "$APP_BINARY" >/dev/null 2>&1; then
        realpath "$(command -v "$APP_BINARY")"
    else
        return 1
    fi
}

resolve_icon_source_root() {
    find_existing_path \
        "$SCRIPT_DIR/icons" \
        "$SCRIPT_DIR/../../src/resources/icons"
}

desktop_quote() {
    local raw=$1

    raw="${raw//\\/\\\\}"
    raw="${raw//\"/\\\"}"
    printf '"%s"' "$raw"
}

systemd_quote() {
    local raw=$1

    raw="${raw//\\/\\\\}"
    raw="${raw//\"/\\\"}"
    printf '"%s"' "$raw"
}

render_desktop_file() {
    cat <<EOF
$MANAGED_MARKER_LINE
[Desktop Entry]
Version=1.0
Type=Application
Name=$APP_NAME
Comment=$APP_COMMENT
Exec=$(desktop_quote "$INSTALL_BINARY")
Icon=$APP_ICON_NAME
Terminal=false
Categories=AudioVideo;Audio;
Keywords=soundboard;audio;pipewire;microphone;
StartupNotify=true
StartupWMClass=$APP_BINARY
X-LinuxSoundboard-Managed=true
EOF
}

render_engine_service() {
    cat <<EOF
$MANAGED_MARKER_LINE
[Unit]
Description=$APP_NAME audio engine
Documentation=$APP_URL
After=pipewire.service pipewire-pulse.service wireplumber.service pulseaudio.service
PartOf=$ENGINE_TARGET_NAME
RefuseManualStop=yes
X-LinuxSoundBoard-Managed=true

[Service]
# Type=exec: systemd tracks the exec'd process PID and reports exec failures
# clearly, unlike Type=simple which considers the service started immediately.
Type=exec
ExecStart=$(systemd_quote "$INSTALL_BINARY") --audio-engine
Restart=on-failure
RestartSec=2s
# Exit 2 means the saved configuration is unreadable or incompatible.
RestartPreventExitStatus=2

# Hardening options tested compatible with PipeWire/PulseAudio user services.
# ProtectHome and ProtectSystem are omitted: the engine must access user sound
# files under \$HOME and does not run with mount namespaces in user sessions.
NoNewPrivileges=yes
RestrictSUIDSGID=yes
LockPersonality=yes
EOF
}

render_engine_target() {
    cat <<EOF
$MANAGED_MARKER_LINE
[Unit]
Description=$APP_NAME persistent audio engine
Documentation=$APP_URL
Wants=$ENGINE_SERVICE_NAME
X-LinuxSoundBoard-Managed=true

[Install]
WantedBy=default.target
EOF
}

runtime_dir() {
    if [[ -n "${XDG_RUNTIME_DIR:-}" ]]; then
        printf '%s\n' "$XDG_RUNTIME_DIR"
    else
        printf '/run/user/%s\n' "$(id -u)"
    fi
}

detect_audio_server() {
    local runtime
    runtime="$(runtime_dir)"

    if [[ -S "$runtime/pipewire-0" ]] || { command -v pw-cli >/dev/null 2>&1 && pw-cli info 0 >/dev/null 2>&1; }; then
        printf 'pipewire\n'
        return 0
    fi

    if [[ -S "$runtime/pulse/native" ]] || { command -v pactl >/dev/null 2>&1 && pactl info >/dev/null 2>&1; }; then
        printf 'pulseaudio\n'
        return 0
    fi

    if command -v pipewire >/dev/null 2>&1 || { command -v systemctl >/dev/null 2>&1 && systemctl --user list-unit-files pipewire.service >/dev/null 2>&1; }; then
        printf 'pipewire\n'
        return 0
    fi

    printf 'unsupported\n'
}

current_pipewire_default_source_name() {
    local value

    command -v wpctl >/dev/null 2>&1 || return 1
    value="$(wpctl inspect @DEFAULT_SOURCE@ 2>/dev/null \
        | awk '
            {
                line = $0
                sub(/^[[:space:]]*\*[[:space:]]*/, "", line)
                if (line ~ /^[[:space:]]*node.name[[:space:]]*=/) {
                    sub(/^[^=]*=[[:space:]]*"/, "", line)
                    sub(/".*$/, "", line)
                    print line
                    exit
                }
            }
        ')"
    [[ -n "$value" ]] || return 1
    printf '%s\n' "$value"
}

current_pulseaudio_default_source_name() {
    local value

    command -v pactl >/dev/null 2>&1 || return 1
    value="$(pactl get-default-source 2>/dev/null | sed '/^$/d' | head -n 1)"
    [[ -n "$value" ]] || return 1
    printf '%s\n' "$value"
}

current_default_source_name() {
    current_pipewire_default_source_name || current_pulseaudio_default_source_name || true
}

current_pipewire_default_sink_name() {
    local value

    command -v wpctl >/dev/null 2>&1 || return 1
    value="$(wpctl inspect @DEFAULT_SINK@ 2>/dev/null \
        | awk '
            {
                line = $0
                sub(/^[[:space:]]*\*[[:space:]]*/, "", line)
                if (line ~ /^[[:space:]]*node.name[[:space:]]*=/) {
                    sub(/^[^=]*=[[:space:]]*"/, "", line)
                    sub(/".*$/, "", line)
                    print line
                    exit
                }
            }
        ')"
    [[ -n "$value" ]] || return 1
    printf '%s\n' "$value"
}

current_default_sink_name() {
    current_pipewire_default_sink_name \
        || { command -v pactl >/dev/null 2>&1 && pactl get-default-sink 2>/dev/null | sed '/^$/d' | head -n 1; } \
        || true
}

capture_preinstall_audio_snapshot() {
    ensure_state_dir

    if [[ -f "$AUDIO_SNAPSHOT_FILE" ]]; then
        return 0
    fi

    local server
    local default_source
    server="$(detect_audio_server)"
    default_source="$(current_default_source_name)"

    {
        printf 'audio_server=%s\n' "$server"
        printf 'default_source_name=%q\n' "$default_source"
        printf 'captured_at=%q\n' "$(date -Is)"
    } >"$AUDIO_SNAPSHOT_FILE"
}

# Reads one key out of a snapshot env file. The files are written with %q
# escaping, so they are sourced rather than parsed.
snapshot_value() {
    local file=$1
    local key=$2

    [[ -f "$file" ]] || return 1
    (
        set +u
        # shellcheck disable=SC1090
        source "$file"
        printf '%s\n' "${!key:-}"
    )
}

source_snapshot_value() {
    snapshot_value "$AUDIO_SNAPSHOT_FILE" "$1"
}

# Every audio-relevant file this installer can touch, plus the ones a user is
# most likely to have configured themselves. Depth is bounded so a large
# WirePlumber tree cannot stall a snapshot.
audio_config_fingerprint() {
    local dir
    local file

    for dir in "$XDG_CONFIG_HOME/pipewire" "$XDG_CONFIG_HOME/wireplumber"; do
        [[ -d "$dir" ]] || continue
        while IFS= read -r file; do
            printf '%s\t%s\n' "$file" "$(checksum_file "$file")"
        done < <(find "$dir" -maxdepth 3 -type f 2>/dev/null | sort)
    done

    for file in "$PULSE_DEFAULT_PA" "$ENGINE_SERVICE" "$ENGINE_TARGET" "$PIPEWIRE_USER_CONF"; do
        [[ -f "$file" ]] || continue
        printf '%s\t%s\n' "$file" "$(checksum_file "$file")"
    done
}

engine_unit_state() {
    command -v systemctl >/dev/null 2>&1 || { printf 'unknown\n'; return 0; }
    printf '%s/%s\n' \
        "$(systemctl --user is-active "$ENGINE_SERVICE_NAME" 2>/dev/null || printf 'inactive')" \
        "$(systemctl --user is-enabled "$ENGINE_TARGET_NAME" 2>/dev/null || printf 'disabled')"
}

swhkd_state() {
    command -v swhkd >/dev/null 2>&1 || { printf 'missing\n'; return 0; }
    printf '%s\n' "$(swhkd --version 2>/dev/null | head -n 1 || printf 'present')"
}

# Records the audio world around an install, update or removal so a later
# uninstall can say what changed and offer to put it back.
capture_audio_snapshot() {
    local event=${1:-manual}
    local stamp
    local base

    ensure_state_dir
    mkdir -p "$SNAPSHOT_DIR"
    stamp="$(date -u +%Y%m%dT%H%M%SZ)"
    base="$SNAPSHOT_DIR/$stamp-$event"

    {
        printf 'event=%q\n' "$event"
        printf 'captured_at=%q\n' "$(date -Is)"
        printf 'audio_server=%q\n' "$(detect_audio_server)"
        printf 'default_source_name=%q\n' "$(current_default_source_name)"
        printf 'default_sink_name=%q\n' "$(current_default_sink_name)"
        printf 'engine_unit=%q\n' "$(engine_unit_state)"
        printf 'swhkd=%q\n' "$(swhkd_state)"
    } >"$base.env"

    audio_config_fingerprint >"$base.files"

    {
        printf '# wpctl status -n\n'
        command -v wpctl >/dev/null 2>&1 && wpctl status -n 2>&1 || printf 'wpctl not available\n'
    } >"$base.txt"

    prune_snapshots
    printf '%s\n' "$base.env"
}

prune_snapshots() {
    local file
    local keep=$SNAPSHOT_KEEP

    [[ -d "$SNAPSHOT_DIR" ]] || return 0
    while IFS= read -r file; do
        rm -f "${file%.env}.env" "${file%.env}.files" "${file%.env}.txt"
    done < <(find "$SNAPSHOT_DIR" -maxdepth 1 -name '*.env' 2>/dev/null | sort -r | tail -n "+$((keep + 1))")
}

latest_snapshot() {
    [[ -d "$SNAPSHOT_DIR" ]] || return 1
    local file
    file="$(find "$SNAPSHOT_DIR" -maxdepth 1 -name '*.env' 2>/dev/null | sort -r | head -n 1)"
    [[ -n "$file" ]] || return 1
    printf '%s\n' "$file"
}

# The oldest retained snapshot is the closest thing to "before this app", which
# is what an uninstall wants to compare against.
first_snapshot() {
    [[ -d "$SNAPSHOT_DIR" ]] || return 1
    local file
    file="$(find "$SNAPSHOT_DIR" -maxdepth 1 -name '*.env' 2>/dev/null | sort | head -n 1)"
    [[ -n "$file" ]] || return 1
    printf '%s\n' "$file"
}

# Prints only what moved between a snapshot and the live system. Empty output
# means nothing this installer cares about changed.
snapshot_diff() {
    local snapshot=${1:-}
    local changed=0
    local key
    local before
    local now

    if [[ -z "$snapshot" ]]; then
        snapshot="$(latest_snapshot)" || {
            warn "No snapshot has been recorded yet."
            return 1
        }
    fi
    [[ -f "$snapshot" ]] || fail "Snapshot not found: $snapshot"

    printf 'Changes since %s (%s):\n' \
        "$(snapshot_value "$snapshot" event || printf 'install')" \
        "$(snapshot_value "$snapshot" captured_at)"

    for key in default_source_name default_sink_name engine_unit swhkd; do
        before="$(snapshot_value "$snapshot" "$key" || true)"
        case "$key" in
            default_source_name) now="$(current_default_source_name)" ;;
            default_sink_name)   now="$(current_default_sink_name)" ;;
            engine_unit)         now="$(engine_unit_state)" ;;
            swhkd)               now="$(swhkd_state)" ;;
        esac
        if [[ "$before" != "$now" ]]; then
            printf '  %-20s %s -> %s\n' "$key:" "${before:-none}" "${now:-none}"
            changed=1
        fi
    done

    # The first-ever snapshot predates this format and has no file list.
    if [[ ! -f "${snapshot%.env}.files" ]]; then
        ((changed == 1)) || printf '  nothing changed\n'
        return 0
    fi

    local live
    live="$(mktemp)"
    audio_config_fingerprint >"$live"
    while IFS=$'\t' read -r path sum; do
        [[ -n "${path:-}" ]] || continue
        if ! grep -qF "$path"$'\t' "$live"; then
            printf '  %-20s %s\n' "removed:" "$path"
            changed=1
        elif ! grep -qxF "$path"$'\t'"$sum" "$live"; then
            printf '  %-20s %s\n' "modified:" "$path"
            changed=1
        fi
    done <"${snapshot%.env}.files"
    while IFS=$'\t' read -r path _; do
        [[ -n "${path:-}" ]] || continue
        if ! grep -qF "$path"$'\t' "${snapshot%.env}.files"; then
            printf '  %-20s %s\n' "added:" "$path"
            changed=1
        fi
    done <"$live"
    rm -f "$live"

    ((changed == 1)) || printf '  nothing changed\n'
    return 0
}

pipewire_source_id_by_name() {
    local name=$1

    command -v pw-cli >/dev/null 2>&1 || return 1
    pw-cli list-objects Node 2>/dev/null \
        | awk -v target="$name" '
            function flush() {
                if (id != "" && node == target && media ~ /^Audio\/Source/) {
                    print id
                    found = 1
                }
            }
            /^[[:space:]]*id [0-9]+,/ {
                if (!found) {
                    flush()
                }
                id = $2
                sub(/,.*$/, "", id)
                node = ""
                media = ""
                next
            }
            /node.name[[:space:]]*=/ {
                line = $0
                sub(/.*node.name[[:space:]]*=[[:space:]]*"/, "", line)
                sub(/".*$/, "", line)
                node = line
                next
            }
            /media.class[[:space:]]*=/ {
                line = $0
                sub(/.*media.class[[:space:]]*=[[:space:]]*"/, "", line)
                sub(/".*$/, "", line)
                media = line
                next
            }
            END {
                if (!found) {
                    flush()
                }
            }
        ' | head -n 1
}

set_pipewire_default_source() {
    local name=$1
    local source_id

    command -v wpctl >/dev/null 2>&1 || return 1
    source_id="$(pipewire_source_id_by_name "$name")"
    [[ -n "$source_id" ]] || return 1
    wpctl set-default "$source_id" >/dev/null 2>&1
}

set_pulseaudio_default_source() {
    local name=$1

    command -v pactl >/dev/null 2>&1 || return 1
    pactl set-default-source "$name" >/dev/null 2>&1
}

restore_preinstall_default_source() {
    local policy=$1
    local previous
    local current
    local server

    previous="$(source_snapshot_value default_source_name || true)"
    [[ -n "$previous" ]] || return 0

    current="$(current_default_source_name)"
    server="$(source_snapshot_value audio_server || true)"

    if [[ "$current" == "$previous" ]]; then
        return 0
    fi

    if [[ "$current" != "$VIRTUAL_SOURCE_NAME" ]]; then
        case "$policy" in
            keep)
                info "Keeping current default microphone: ${current:-unknown}"
                return 0
                ;;
            restore)
                ;;
            ask)
                if [[ -t 0 ]]; then
                    printf 'Current default microphone is "%s", not Linux Soundboard.\n' "${current:-unknown}"
                    printf 'Restore preinstall default "%s"? [y/N] ' "$previous"
                    local answer
                    read -r answer
                    case "${answer,,}" in
                        y|yes)
                            ;;
                        *)
                            info "Keeping current default microphone."
                            return 0
                            ;;
                    esac
                else
                    info "Keeping current default microphone in noninteractive remove."
                    return 0
                fi
                ;;
        esac
    fi

    case "$server" in
        pipewire)
            if set_pipewire_default_source "$previous"; then
                info "Restored default microphone: $previous"
            else
                warn "Could not restore PipeWire default microphone '$previous'."
            fi
            ;;
        pulseaudio)
            if set_pulseaudio_default_source "$previous"; then
                info "Restored default microphone: $previous"
            else
                warn "Could not restore PulseAudio default microphone '$previous'."
            fi
            ;;
        *)
            set_pipewire_default_source "$previous" \
                || set_pulseaudio_default_source "$previous" \
                || warn "Could not restore default microphone '$previous'."
            ;;
    esac
}

strip_managed_block() {
    local input=$1
    local output=$2

    awk -v start="$MANAGED_MARKER_LINE" -v end="$END_MANAGED_MARKER_LINE" '
        index($0, start) {
            skip = 1
            next
        }
        index($0, end) {
            skip = 0
            next
        }
        !skip {
            print
        }
    ' "$input" >"$output"
}

next_disabled_path() {
    local path=$1
    local candidate="$path.disabled"
    local index=1

    while [[ -e "$candidate" ]]; do
        candidate="$path.disabled.$index"
        index=$((index + 1))
    done

    printf '%s\n' "$candidate"
}

managed_linuxsoundboard_audio_file() {
    local path=$1

    [[ -f "$path" ]] || return 1
    grep -Fq "$MANAGED_MARKER" "$path" || return 1
    grep -Fq "$VIRTUAL_SOURCE_NAME" "$path" || return 1
}

disable_file() {
    local path=$1
    local label=$2
    local disabled_path

    disabled_path="$(next_disabled_path "$path")"
    mv -- "$path" "$disabled_path"
    AUDIO_CONFIG_CHANGED=1
    info "Disabled obsolete $label: $disabled_path"
}

disable_managed_audio_file() {
    local path=$1
    local label=$2

    [[ -e "$path" ]] || return 0
    if ! managed_linuxsoundboard_audio_file "$path"; then
        warn "Skipped non-managed $label: $path"
        return 0
    fi

    disable_file "$path" "$label"
}

remove_system_managed_audio_file() {
    local path=$1
    local label=$2

    [[ -e "$path" ]] || return 0
    if ! managed_linuxsoundboard_audio_file "$path"; then
        warn "Skipped non-managed system $label: $path"
        return 0
    fi

    if [[ -w "$path" && -w "$(dirname "$path")" ]]; then
        rm -f -- "$path"
        AUDIO_CONFIG_CHANGED=1
        info "Removed obsolete system $label: $path"
    elif command -v sudo >/dev/null 2>&1 && sudo -n true >/dev/null 2>&1; then
        sudo rm -f -- "$path"
        AUDIO_CONFIG_CHANGED=1
        info "Removed obsolete system $label with sudo: $path"
    else
        warn "Obsolete system $label still exists: $path"
        warn "Remove it manually with: sudo rm -f '$path'"
    fi
}

cleanup_legacy_wireplumber_config() {
    local path
    local candidates=(
        "$XDG_CONFIG_HOME/wireplumber/main.lua.d/99-linuxsoundboard-autoroute.lua"
        "$XDG_CONFIG_HOME/wireplumber/wireplumber.conf.d/50-linuxsoundboard-capture.conf"
        "$XDG_CONFIG_HOME/wireplumber/wireplumber.conf.d/51-linuxsoundboard-force-capture.conf"
        "$XDG_DATA_HOME/wireplumber/scripts/50-linuxsoundboard-force-capture.lua"
    )

    for path in "${candidates[@]}"; do
        [[ -f "$path" ]] || continue
        if grep -Fiq 'linuxsoundboard' "$path" && { grep -Fq 'target.object' "$path" || grep -Fq "$VIRTUAL_SOURCE_NAME" "$path" || grep -Fq 'LinuxSoundboard_Mic' "$path"; }; then
            disable_file "$path" "WirePlumber routing file"
        fi
    done
}

cleanup_legacy_audio_config() {
    capture_preinstall_audio_snapshot
    disable_managed_audio_file "$PIPEWIRE_USER_CONF" "PipeWire virtual mic config"
    remove_system_managed_audio_file "$SYSTEM_PIPEWIRE_CONF" "PipeWire virtual mic config"
    cleanup_legacy_wireplumber_config
    remove_pulse_managed_block
}

install_audio_config() {
    cleanup_legacy_audio_config
}

active_user_unit() {
    local unit=$1

    command -v systemctl >/dev/null 2>&1 || return 1
    systemctl --user is-active --quiet "$unit" >/dev/null 2>&1
}

restart_audio_services() {
    command -v systemctl >/dev/null 2>&1 || return 0

    local units=()
    local unit
    for unit in wireplumber.service pipewire-media-session.service pipewire-pulse.service pipewire.service pulseaudio.service; do
        if active_user_unit "$unit"; then
            units+=("$unit")
        fi
    done

    if ((${#units[@]} > 0)); then
        systemctl --user restart "${units[@]}" >/dev/null 2>&1 || warn "Could not restart active audio user services."
    fi
}

virtual_mic_present() {
    if command -v wpctl >/dev/null 2>&1 && wpctl status -n 2>/dev/null | grep -Fq "$VIRTUAL_SOURCE_NAME"; then
        return 0
    fi

    if command -v pw-cli >/dev/null 2>&1 && pw-cli list-objects Node 2>/dev/null | grep -Fq "$VIRTUAL_SOURCE_NAME"; then
        return 0
    fi

    if command -v pactl >/dev/null 2>&1 && pactl list short sources 2>/dev/null | awk '{print $2}' | grep -Fxq "$VIRTUAL_SOURCE_NAME"; then
        return 0
    fi

    return 1
}

reload_start_engine_service() {
    command -v systemctl >/dev/null 2>&1 || return 0

    systemctl --user daemon-reload >/dev/null 2>&1 || true
    systemctl --user disable "$ENGINE_SERVICE_NAME" >/dev/null 2>&1 || true
    systemctl --user enable "$ENGINE_TARGET_NAME" >/dev/null 2>&1 || true
    systemctl --user restart "$ENGINE_TARGET_NAME" >/dev/null 2>&1 || true
}

stop_disable_engine_service() {
    command -v systemctl >/dev/null 2>&1 || return 0

    systemctl --user disable --now "$ENGINE_TARGET_NAME" >/dev/null 2>&1 || true
    systemctl --user disable "$ENGINE_SERVICE_NAME" >/dev/null 2>&1 || true
    systemctl --user daemon-reload >/dev/null 2>&1 || true
}

refresh_desktop_caches() {
    if command -v gtk-update-icon-cache >/dev/null 2>&1; then
        gtk-update-icon-cache -q -t "$ICON_THEME_DIR" >/dev/null 2>&1 || true
    fi

    if command -v update-desktop-database >/dev/null 2>&1; then
        update-desktop-database "$DESKTOP_DIR" >/dev/null 2>&1 || true
    fi
}

install_icons() {
    local icon_root=$1
    local icon_path
    local size_dir
    local icon_name
    local dest
    local installed=0

    while IFS= read -r icon_path; do
        size_dir="$(basename "$(dirname "$(dirname "$icon_path")")")"
        for icon_name in "$APP_ID" "$APP_ICON_NAME"; do
            dest="$ICON_THEME_DIR/$size_dir/apps/$icon_name.png"
            install_file_from_source "$icon_path" "$dest" 644
            installed=1
        done
    done < <(find "$icon_root" -path "*/apps/$APP_ID.png" -type f | sort)

    if ((installed == 0)); then
        fail "Could not find app icons below $icon_root."
    fi
}

resolve_project_file() {
    local name=$1
    local candidate

    for candidate in "$SCRIPT_DIR/$name" "$SCRIPT_DIR/../../$name"; do
        if [[ -f "$candidate" ]]; then
            printf '%s\n' "$candidate"
            return 0
        fi
    done

    return 1
}

install_legal_documents() {
    local legal_file
    local source

    for legal_file in LICENSE NOTICE.md THIRDPARTY_LICENSES.md THIRD_PARTY_NOTICES.html COMMERCIAL-LICENSE.md DONATIONS.md; do
        if source="$(resolve_project_file "$legal_file")"; then
            install_file_from_source "$source" "$INSTALL_DOC_DIR/$legal_file" 644
        else
            warn "Legal document not found in installer bundle: $legal_file"
        fi
    done
}

install_or_repair() {
    local mode=$1
    local binary_arg=${2:-}
    local binary_source
    local icon_source_root
    AUDIO_CONFIG_CHANGED=0

    # Before the first mutation, so an uninstall can say what this run changed.
    capture_preinstall_audio_snapshot
    capture_audio_snapshot "$([[ -x "$INSTALL_BINARY" ]] && printf 'update' || printf 'install')" >/dev/null

    binary_source="$(resolve_binary_source "$binary_arg")" || fail "Could not find a built $APP_BINARY binary. Pass the binary path after '$mode'."
    icon_source_root="$(resolve_icon_source_root)" || fail "Could not find the bundled icon set."

    info "$([[ "$mode" == "repair" ]] && printf 'Repairing' || printf 'Installing') $APP_NAME."

    install_file_from_source "$binary_source" "$INSTALL_BINARY" 755

    if [[ -x "$SCRIPT_DIR/install-swhkd-helper.sh" ]]; then
        install_file_from_source "$SCRIPT_DIR/install-swhkd-helper.sh" "$INSTALL_HELPER" 755
    fi

    install_legal_documents
    install_icons "$icon_source_root"
    install_file_from_content "$DESKTOP_DIR/$APP_ID.desktop" 644 "$(render_desktop_file)"
    install_file_from_content "$ENGINE_SERVICE" 644 "$(render_engine_service)"
    install_file_from_content "$ENGINE_TARGET" 644 "$(render_engine_target)"
    install_audio_config
    maybe_restart_audio_services
    reload_start_engine_service
    refresh_desktop_caches
    if [[ -n "${LSB_INSTALL_VERSION:-}" ]]; then
        install_file_from_content "$INSTALL_VERSION_FILE" 644 "$LSB_INSTALL_VERSION"
    fi

    if virtual_mic_present; then
        info "Virtual microphone is visible."
    else
        warn "Virtual microphone is not visible yet. It may appear after audio services or the session restart."
    fi

    printf '\n'
    printf '%s complete:\n' "$([[ "$mode" == "repair" ]] && printf 'Repair' || printf 'Install')"
    printf '  Binary:   %s\n' "$INSTALL_BINARY"
    printf '  Notices:  %s\n' "$INSTALL_DOC_DIR"
    printf '  Launcher: %s\n' "$DESKTOP_DIR/$APP_ID.desktop"
    printf '  Engine:   %s\n' "$ENGINE_SERVICE"
}

maybe_restart_audio_services() {
    if ((AUDIO_CONFIG_CHANGED == 1)); then
        restart_audio_services
    fi
}

# Service-only setup for native package (DEB/RPM) installs. The package owns the
# binary, desktop entry, icons, and the systemd user unit, so this only enables
# the engine service for the installing account and clears obsolete user-level
# audio routing. It deliberately does not deploy a second copy of those files
# into ~/.local, which would shadow the package and run a stale binary after a
# package upgrade.
setup_user_service() {
    AUDIO_CONFIG_CHANGED=0

    if ! command -v systemctl >/dev/null 2>&1; then
        warn "systemctl not found; cannot configure the user service."
        return 0
    fi

    info "Configuring $APP_NAME user service."
    cleanup_legacy_audio_config
    maybe_restart_audio_services
    reload_start_engine_service

    if virtual_mic_present; then
        info "Virtual microphone is visible."
    else
        warn "Virtual microphone is not visible yet. It may appear after the engine starts or the session restarts."
    fi

    printf '\n'
    printf 'User service configured:\n'
    printf '  Engine:   %s (systemctl --user)\n' "$ENGINE_SERVICE_NAME"
}

remove_managed_file() {
    local path=$1
    local backup

    [[ -e "$path" ]] || return 0

    backup="$(backup_for_path "$path" || true)"
    if [[ -n "$backup" && -f "$backup" ]]; then
        ensure_parent_dir "$path"
        cp -p -- "$backup" "$path"
        info "Restored previous file: $path"
        return 0
    fi

    if path_in_manifest "$path" || contains_managed_marker "$path"; then
        rm -f -- "$path"
        info "Removed managed file: $path"
    else
        warn "Skipped non-managed file: $path"
    fi
}

remove_known_app_file() {
    local path=$1
    local label=$2

    [[ -e "$path" ]] || return 0

    if backup_exists_for_path "$path" || path_in_manifest "$path" || contains_managed_marker "$path"; then
        remove_managed_file "$path"
        return 0
    fi

    rm -f -- "$path"
    info "Removed Linux Soundboard $label: $path"
}

remove_pipewire_config() {
    remove_system_managed_audio_file "$SYSTEM_PIPEWIRE_CONF" "PipeWire virtual mic config"
    if [[ -f "$PIPEWIRE_USER_CONF" ]]; then
        if path_in_manifest "$PIPEWIRE_USER_CONF" || contains_managed_marker "$PIPEWIRE_USER_CONF"; then
            rm -f -- "$PIPEWIRE_USER_CONF"
            info "Removed Linux Soundboard PipeWire config."
        else
            warn "Skipped non-managed PipeWire config: $PIPEWIRE_USER_CONF"
        fi
    fi
}

remove_pulse_managed_block() {
    local tmp
    local stripped_content
    local has_backup=0

    [[ -f "$PULSE_DEFAULT_PA" ]] || return 0

    if ! contains_managed_marker "$PULSE_DEFAULT_PA" && ! path_in_manifest "$PULSE_DEFAULT_PA"; then
        warn "Skipped non-managed PulseAudio config: $PULSE_DEFAULT_PA"
        return 0
    fi

    tmp="$(mktemp)"
    strip_managed_block "$PULSE_DEFAULT_PA" "$tmp"
    stripped_content="$(sed '/^[[:space:]]*$/d' "$tmp")"

    if backup_exists_for_path "$PULSE_DEFAULT_PA"; then
        has_backup=1
    fi

    if [[ -z "$stripped_content" ]] || { [[ "$stripped_content" == ".include /etc/pulse/default.pa" ]] && ((has_backup == 0)); }; then
        rm -f -- "$PULSE_DEFAULT_PA"
        AUDIO_CONFIG_CHANGED=1
        info "Removed Linux Soundboard PulseAudio config."
    else
        backup_file_if_needed "$PULSE_DEFAULT_PA"
        install_file_from_source "$tmp" "$PULSE_DEFAULT_PA" 644
        AUDIO_CONFIG_CHANGED=1
        info "Removed Linux Soundboard PulseAudio block."
    fi
    rm -f "$tmp"
}

pulse_config_status() {
    if [[ -f "$PULSE_DEFAULT_PA" ]] && contains_managed_marker "$PULSE_DEFAULT_PA"; then
        printf '%s' "$PULSE_DEFAULT_PA"
    else
        printf 'no managed block'
    fi
}

remove_icons() {
    local path
    local size

    while IFS= read -r path; do
        remove_known_app_file "$path" "icon"
    done < <(
        {
            awk -F '\t' -v app_id="$APP_ID" -v icon_name="$APP_ICON_NAME" \
                '$1 == "file" && index($3, "/icons/hicolor/") > 0 {
                    n = split($3, a, "/"); base = a[n]; sub(/\.png$/, "", base)
                    if (base == app_id || base == icon_name) print $3
                }' "$MANIFEST_FILE" 2>/dev/null
            for size in 16x16 24x24 32x32 48x48 64x64 128x128 256x256 512x512; do
                printf '%s/%s/apps/%s.png\n' "$ICON_THEME_DIR" "$size" "$APP_ID"
                printf '%s/%s/apps/%s.png\n' "$ICON_THEME_DIR" "$size" "$APP_ICON_NAME"
            done
        } | sort -u
    )
}

remove_empty_recorded_dirs() {
    [[ -f "$MANIFEST_FILE" ]] || return 0

    tac "$MANIFEST_FILE" 2>/dev/null \
        | awk -F '\t' '$1 == "dir" { print $3 }' \
        | while IFS= read -r path; do
            [[ -n "$path" && -d "$path" ]] || continue
            rmdir "$path" >/dev/null 2>&1 || true
        done
}

purge_app_data() {
    rm -rf -- \
        "${XDG_CONFIG_HOME:?}/$APP_BINARY" \
        "${XDG_CACHE_HOME:?}/$APP_BINARY"

    if [[ -d "$XDG_STATE_HOME/$APP_BINARY" ]]; then
        find "$XDG_STATE_HOME/$APP_BINARY" -mindepth 1 -maxdepth 1 ! -name install-user -exec rm -rf -- {} +
    fi

    info "Purged Linux Soundboard config/state/cache data."
}

confirm_remove() {
    if ((YES == 1)); then
        return 0
    fi

    [[ -t 0 ]] || fail "Removal requires --yes in noninteractive mode."

    printf 'This will remove Linux Soundboard user install files and restore managed audio changes.\n'
    if ((KEEP_DATA == 0)); then
        printf 'Linux Soundboard app config/state/cache will be purged. External sound folders will not be deleted.\n'
    else
        printf 'Linux Soundboard app config/state/cache will be kept.\n'
    fi
    printf 'Continue? [y/N] '

    local answer
    read -r answer
    case "${answer,,}" in
        y|yes)
            ;;
        *)
            fail "Remove cancelled."
            ;;
    esac
}

# Resolves the restore decision once, with the snapshot diff in front of the
# user, so restore_preinstall_default_source never has to ask blindly.
# Everything but the policy goes to stderr; stdout is the policy.
resolve_restore_policy() {
    if [[ "$DEFAULT_SOURCE_POLICY" != "ask" ]]; then
        printf '%s\n' "$DEFAULT_SOURCE_POLICY"
        return 0
    fi

    if ((YES == 1)) || [[ ! -t 0 ]]; then
        printf 'keep\n'
        return 0
    fi

    local baseline
    baseline="$(first_snapshot || printf '%s' "$AUDIO_SNAPSHOT_FILE")"
    if [[ -f "$baseline" ]]; then
        printf '\n' >&2
        snapshot_diff "$baseline" >&2 || true
    fi

    printf '\nRestore the audio setup recorded before %s was installed? [y/N] ' "$APP_NAME" >&2
    local answer
    read -r answer || answer=""
    case "${answer,,}" in
        y|yes)
            printf 'restore\n'
            ;;
        *)
            printf 'keep\n'
            ;;
    esac
}

remove_installation() {
    local keep_state=0

    ensure_state_dir
    confirm_remove
    capture_audio_snapshot remove >/dev/null

    stop_disable_engine_service
    restore_preinstall_default_source "$(resolve_restore_policy)"

    remove_known_app_file "$ENGINE_SERVICE" "engine service"
    remove_known_app_file "$ENGINE_TARGET" "engine target"
    remove_known_app_file "$DESKTOP_DIR/$APP_ID.desktop" "desktop entry"
    remove_icons
    remove_pipewire_config
    remove_pulse_managed_block
    remove_known_app_file "$INSTALL_HELPER" "helper"
    remove_known_app_file "$INSTALL_BINARY" "binary"
    remove_known_app_file "$INSTALL_VERSION_FILE" "installed version marker"

    restart_audio_services
    refresh_desktop_caches
    remove_empty_recorded_dirs
    rmdir "$INSTALL_ROOT" >/dev/null 2>&1 || true

    if ((KEEP_DATA == 0)); then
        purge_app_data
    fi

    if [[ -s "$BACKUP_MANIFEST_FILE" ]]; then
        while IFS=$'\t' read -r original backup _checksum; do
            [[ -n "${original:-}" && -n "${backup:-}" ]] || continue
            if [[ -f "$backup" && ! -e "$original" ]]; then
                keep_state=1
            fi
        done <"$BACKUP_MANIFEST_FILE"
    fi

    if ((keep_state == 0)); then
        rm -rf -- "$STATE_DIR"
        rmdir "$XDG_STATE_HOME/$APP_BINARY" >/dev/null 2>&1 || true
    else
        warn "Keeping installer backups at $STATE_DIR because not every backup was restored."
    fi

    info "Remove complete."
}

print_status() {
    local service_state="unknown"
    local service_enabled="unknown"
    local default_source

    if command -v systemctl >/dev/null 2>&1; then
        service_state="$(systemctl --user is-active "$ENGINE_SERVICE_NAME" 2>/dev/null || true)"
        service_enabled="$(systemctl --user is-enabled "$ENGINE_TARGET_NAME" 2>/dev/null || true)"
    fi

    default_source="$(current_default_source_name)"

    printf '%s status:\n' "$APP_NAME"
    printf '  Binary:        %s\n' "$([[ -x "$INSTALL_BINARY" ]] && printf '%s' "$INSTALL_BINARY" || printf 'missing')"
    printf '  Launcher:      %s\n' "$([[ -f "$DESKTOP_DIR/$APP_ID.desktop" ]] && printf '%s' "$DESKTOP_DIR/$APP_ID.desktop" || printf 'missing')"
    printf '  Engine unit:   %s\n' "$([[ -f "$ENGINE_SERVICE" ]] && printf '%s' "$ENGINE_SERVICE" || printf 'missing')"
    printf '  Engine target: %s\n' "$([[ -f "$ENGINE_TARGET" ]] && printf '%s' "$ENGINE_TARGET" || printf 'missing')"
    printf '  Engine active: %s\n' "${service_state:-unknown}"
    printf '  Engine enable: %s\n' "${service_enabled:-unknown}"
    printf '  Legacy conf:   %s\n' "$([[ -f "$PIPEWIRE_USER_CONF" ]] && printf '%s' "$PIPEWIRE_USER_CONF" || printf 'missing')"
    printf '  Pulse config:  %s\n' "$(pulse_config_status)"
    printf '  Virtual mic:   %s\n' "$(virtual_mic_present && printf 'visible' || printf 'not visible')"
    printf '  Default mic:   %s\n' "${default_source:-unknown}"
    printf '  State dir:     %s\n' "$([[ -d "$STATE_DIR" ]] && printf '%s' "$STATE_DIR" || printf 'missing')"
}

prompt_keep_data_for_menu() {
    KEEP_DATA=0

    printf 'Purge Linux Soundboard app config/state/cache? External sound folders are never deleted. [Y/n] '
    local answer
    read -r answer
    case "${answer,,}" in
        n|no)
            KEEP_DATA=1
            ;;
    esac
}

interactive_menu() {
    while true; do
        printf '\n'
        printf '%s User Manager\n' "$APP_NAME"
        printf '1) Install Linux Soundboard\n'
        printf '2) Repair Linux Soundboard\n'
        printf '3) Remove Linux Soundboard\n'
        printf '4) Show current install status\n'
        printf '5) Help\n'
        printf '0) Exit\n'
        printf 'Choose an option: '

        local choice
        read -r choice

        case "$choice" in
            1)
                install_or_repair install
                ;;
            2)
                install_or_repair repair
                ;;
            3)
                YES=0
                DEFAULT_SOURCE_POLICY="ask"
                prompt_keep_data_for_menu
                remove_installation
                ;;
            4)
                print_status
                ;;
            5)
                usage
                ;;
            0)
                exit 0
                ;;
            *)
                warn "Unknown option: $choice"
                ;;
        esac
    done
}

parse_remove_args() {
    while (($# > 0)); do
        case "$1" in
            --yes|-y)
                YES=1
                ;;
            --keep-data)
                KEEP_DATA=1
                ;;
            --restore-default-source)
                DEFAULT_SOURCE_POLICY="restore"
                ;;
            --keep-current-default-source)
                DEFAULT_SOURCE_POLICY="keep"
                ;;
            *)
                fail "Unknown remove option: $1"
                ;;
        esac
        shift
    done
}

main() {
    local command=${1:-}

    if [[ -z "$command" ]]; then
        if [[ -t 0 && -t 1 ]]; then
            interactive_menu
        else
            usage
            exit 0
        fi
    fi

    case "$command" in
        install)
            shift
            install_or_repair install "${1:-}"
            ;;
        repair)
            shift
            install_or_repair repair "${1:-}"
            ;;
        setup-user)
            shift
            setup_user_service
            ;;
        remove)
            shift
            parse_remove_args "$@"
            remove_installation
            ;;
        status)
            print_status
            ;;
        snapshot)
            shift
            capture_audio_snapshot "${1:-manual}"
            ;;
        snapshot-diff)
            shift
            snapshot_diff "${1:-}"
            ;;
        restore-audio)
            restore_preinstall_default_source restore
            ;;
        --help|-h|help)
            usage
            ;;
        *)
            # Backward compatibility: old installer accepted a binary path as
            # the first positional argument.
            if [[ -e "$command" ]]; then
                install_or_repair install "$command"
            else
                usage
                exit 1
            fi
            ;;
    esac
}

main "$@"
