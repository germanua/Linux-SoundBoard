#!/usr/bin/env bash

set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
source "$SCRIPT_DIR/app-meta.sh"

MANAGED_MARKER="managed-by: linux-soundboard"
MANAGED_MARKER_LINE="# $MANAGED_MARKER"
END_MANAGED_MARKER_LINE="# end-managed-by: linux-soundboard"
VIRTUAL_SOURCE_NAME="linuxsoundboard.virtual_mic"
ENGINE_SERVICE_NAME="linux-soundboard-engine.service"
PIPEWIRE_CONF_NAME="99-linuxsoundboard.conf"
SYSTEM_PIPEWIRE_CONF="/usr/share/pipewire/pipewire.conf.d/$PIPEWIRE_CONF_NAME"

INSTALL_ROOT="${INSTALL_ROOT:-$HOME/.local/opt/$APP_BINARY}"
INSTALL_BINARY="$INSTALL_ROOT/$APP_BINARY"
INSTALL_HELPER="$INSTALL_ROOT/install-swhkd-helper.sh"

XDG_DATA_HOME="${XDG_DATA_HOME:-$HOME/.local/share}"
XDG_CONFIG_HOME="${XDG_CONFIG_HOME:-$HOME/.config}"
XDG_STATE_HOME="${XDG_STATE_HOME:-$HOME/.local/state}"
XDG_CACHE_HOME="${XDG_CACHE_HOME:-$HOME/.cache}"

DESKTOP_DIR="$XDG_DATA_HOME/applications"
ICON_THEME_DIR="$XDG_DATA_HOME/icons/hicolor"
SYSTEMD_USER_DIR="$XDG_CONFIG_HOME/systemd/user"
ENGINE_SERVICE="$SYSTEMD_USER_DIR/$ENGINE_SERVICE_NAME"
PIPEWIRE_USER_CONF="$XDG_CONFIG_HOME/pipewire/pipewire.conf.d/$PIPEWIRE_CONF_NAME"
PULSE_DEFAULT_PA="$XDG_CONFIG_HOME/pulse/default.pa"
APP_CONFIG_FILE="$XDG_CONFIG_HOME/$APP_BINARY/config.json"

STATE_DIR="$XDG_STATE_HOME/$APP_BINARY/install-user"
BACKUP_DIR="$STATE_DIR/backups"
MANIFEST_FILE="$STATE_DIR/manifest.tsv"
BACKUP_MANIFEST_FILE="$STATE_DIR/backups.tsv"
AUDIO_SNAPSHOT_FILE="$STATE_DIR/preinstall-audio.env"

YES=0
KEEP_DATA=0
DEFAULT_SOURCE_POLICY="ask"

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
  ./install-user.sh remove [--yes] [--keep-data] [--restore-default-source|--keep-current-default-source]
  ./install-user.sh status
  ./install-user.sh --help

No arguments opens the interactive menu when run from a terminal. In
noninteractive mode, pass an explicit command.
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

    ensure_parent_dir "$dest"
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
    elif [[ -x "$SCRIPT_DIR/../../src/target/release/$APP_BINARY" ]]; then
        realpath "$SCRIPT_DIR/../../src/target/release/$APP_BINARY"
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
After=pipewire.service pipewire-pulse.service wireplumber.service pulseaudio.service

[Service]
Type=simple
ExecStart=$(systemd_quote "$INSTALL_BINARY") --audio-engine
Restart=on-failure
RestartSec=2

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

source_snapshot_value() {
    local key=$1

    [[ -f "$AUDIO_SNAPSHOT_FILE" ]] || return 1
    (
        set +u
        source "$AUDIO_SNAPSHOT_FILE"
        case "$key" in
            audio_server)
                printf '%s\n' "${audio_server:-}"
                ;;
            default_source_name)
                printf '%s\n' "${default_source_name:-}"
                ;;
        esac
    )
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
        info "Removed obsolete system $label: $path"
    elif command -v sudo >/dev/null 2>&1 && sudo -n true >/dev/null 2>&1; then
        sudo rm -f -- "$path"
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
    systemctl --user enable "$ENGINE_SERVICE_NAME" >/dev/null 2>&1 || true
    systemctl --user restart "$ENGINE_SERVICE_NAME" >/dev/null 2>&1 || true
}

stop_disable_engine_service() {
    command -v systemctl >/dev/null 2>&1 || return 0

    systemctl --user disable --now "$ENGINE_SERVICE_NAME" >/dev/null 2>&1 || true
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

install_or_repair() {
    local mode=$1
    local binary_arg=${2:-}
    local binary_source
    local icon_source_root

    binary_source="$(resolve_binary_source "$binary_arg")" || fail "Could not find a built $APP_BINARY binary. Pass the binary path after '$mode'."
    icon_source_root="$(resolve_icon_source_root)" || fail "Could not find the bundled icon set."

    info "$([[ "$mode" == "repair" ]] && printf 'Repairing' || printf 'Installing') $APP_NAME."

    install_file_from_source "$binary_source" "$INSTALL_BINARY" 755

    if [[ -x "$SCRIPT_DIR/install-swhkd-helper.sh" ]]; then
        install_file_from_source "$SCRIPT_DIR/install-swhkd-helper.sh" "$INSTALL_HELPER" 755
    fi

    install_icons "$icon_source_root"
    install_file_from_content "$DESKTOP_DIR/$APP_ID.desktop" 644 "$(render_desktop_file)"
    install_file_from_content "$ENGINE_SERVICE" 644 "$(render_engine_service)"
    install_audio_config
    restart_audio_services
    reload_start_engine_service
    refresh_desktop_caches

    if virtual_mic_present; then
        info "Virtual microphone is visible."
    else
        warn "Virtual microphone is not visible yet. It may appear after audio services or the session restart."
    fi

    printf '\n'
    printf '%s complete:\n' "$([[ "$mode" == "repair" ]] && printf 'Repair' || printf 'Install')"
    printf '  Binary:   %s\n' "$INSTALL_BINARY"
    printf '  Launcher: %s\n' "$DESKTOP_DIR/$APP_ID.desktop"
    printf '  Engine:   %s\n' "$ENGINE_SERVICE"
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
        info "Removed Linux Soundboard PulseAudio config."
    else
        backup_file_if_needed "$PULSE_DEFAULT_PA"
        install_file_from_source "$tmp" "$PULSE_DEFAULT_PA" 644
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

    while IFS= read -r path; do
        remove_managed_file "$path"
    done < <(
        awk -F '\t' -v app_id="$APP_ID" -v icon_name="$APP_ICON_NAME" \
            '$1 == "file" && index($3, "/icons/hicolor/") > 0 {
                n = split($3, a, "/"); base = a[n]; sub(/\.png$/, "", base)
                if (base == app_id || base == icon_name) print $3
            }' "$MANIFEST_FILE" 2>/dev/null | sort -u
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
        "$XDG_CONFIG_HOME/$APP_BINARY" \
        "$XDG_CACHE_HOME/$APP_BINARY"

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

remove_installation() {
    local keep_state=0

    ensure_state_dir
    confirm_remove

    stop_disable_engine_service
    restore_preinstall_default_source "$DEFAULT_SOURCE_POLICY"

    remove_managed_file "$ENGINE_SERVICE"
    remove_managed_file "$DESKTOP_DIR/$APP_ID.desktop"
    remove_icons
    remove_pipewire_config
    remove_pulse_managed_block
    remove_managed_file "$INSTALL_HELPER"
    remove_managed_file "$INSTALL_BINARY"

    restart_audio_services
    refresh_desktop_caches
    remove_empty_recorded_dirs

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
        service_enabled="$(systemctl --user is-enabled "$ENGINE_SERVICE_NAME" 2>/dev/null || true)"
    fi

    default_source="$(current_default_source_name)"

    printf '%s status:\n' "$APP_NAME"
    printf '  Binary:        %s\n' "$([[ -x "$INSTALL_BINARY" ]] && printf '%s' "$INSTALL_BINARY" || printf 'missing')"
    printf '  Launcher:      %s\n' "$([[ -f "$DESKTOP_DIR/$APP_ID.desktop" ]] && printf '%s' "$DESKTOP_DIR/$APP_ID.desktop" || printf 'missing')"
    printf '  Engine unit:   %s\n' "$([[ -f "$ENGINE_SERVICE" ]] && printf '%s' "$ENGINE_SERVICE" || printf 'missing')"
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
        remove)
            shift
            parse_remove_args "$@"
            remove_installation
            ;;
        status)
            print_status
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
