#!/usr/bin/env bash

LSB_BUILD_PROFILE="${LSB_BUILD_PROFILE:-stable}"

case "$LSB_BUILD_PROFILE" in
  stable)
    APP_ID="com.linuxsoundboard.app"
    APP_ICON_NAME="linux-soundboard"
    APP_ICON_SOURCE_ID="com.linuxsoundboard.app"
    APP_ICON_SOURCE_NAME="linux-soundboard"
    APP_BINARY="linux-soundboard"
    APP_NAME="Linux Soundboard"
    APP_COMMENT="A Linux soundboard with PipeWire virtual mic support"
    APP_URL="https://github.com/germanua/Linux-SoundBoard"
    APP_REPO="germanua/Linux-SoundBoard"
    UPDATE_CHANNEL="stable"
    CONFIG_DIR_NAME="linux-soundboard"
    STATE_DIR_NAME="linux-soundboard"
    ENGINE_SERVICE_NAME="linux-soundboard-engine.service"
    ENGINE_TARGET_NAME="linux-soundboard-engine.target"
    PIPEWIRE_CONF_NAME="99-linuxsoundboard.conf"
    PIPEWIRE_NAMESPACE="linuxsoundboard"
    VIRTUAL_OUTPUT_DESCRIPTION="Linux_Soundboard_Output"
    VIRTUAL_MIC_DESCRIPTION="Linux_Soundboard_Mic"
    LSB_ALLOW_PRIVILEGED_HELPER=1
    LSB_ALLOW_DEFAULT_SOURCE_CLAIM=1
    HOTKEY_PIPE_NAME="lsb_hotkey.pipe"
    INSTALLER_COMMAND="curl -fsSL https://raw.githubusercontent.com/germanua/Linux-SoundBoard/main/bootstrap-install.sh | bash"
    ;;
  dev)
    PROFILE_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
    DEV_PROFILE_FILE="${LSB_DEV_PROFILE_FILE:-$PROFILE_DIR/dev/profile.env}"
    [[ -r "$DEV_PROFILE_FILE" ]] || { echo "DEV profile is missing: $DEV_PROFILE_FILE" >&2; return 2 2>/dev/null || exit 2; }
    source "$DEV_PROFILE_FILE"
    ;;
  *)
    echo "Unknown LSB_BUILD_PROFILE: $LSB_BUILD_PROFILE (expected stable or dev)" >&2
    return 2 2>/dev/null || exit 2
    ;;
esac

LOCAL_PLAYBACK_NODE_NAME="$PIPEWIRE_NAMESPACE.local_playback"
MIC_CAPTURE_NODE_NAME="$PIPEWIRE_NAMESPACE.mic_capture"
VIRTUAL_SOURCE_NAME="$PIPEWIRE_NAMESPACE.virtual_mic"
VIRTUAL_MIC_FEEDER_NODE_NAME="$PIPEWIRE_NAMESPACE.virtual_mic_feeder"
MANAGED_MARKER="managed-by: $APP_BINARY"
