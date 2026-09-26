#!/usr/bin/env bash

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
PROFILE_FILE="$SCRIPT_DIR/../profile.sh"
if [[ -r "$PROFILE_FILE" ]]; then


    source "$PROFILE_FILE"
else


    LSB_BUILD_PROFILE="stable"
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
    LOCAL_PLAYBACK_NODE_NAME="$PIPEWIRE_NAMESPACE.local_playback"
    MIC_CAPTURE_NODE_NAME="$PIPEWIRE_NAMESPACE.mic_capture"
    VIRTUAL_SOURCE_NAME="$PIPEWIRE_NAMESPACE.virtual_mic"
    VIRTUAL_MIC_FEEDER_NODE_NAME="$PIPEWIRE_NAMESPACE.virtual_mic_feeder"
    MANAGED_MARKER="managed-by: $APP_BINARY"
fi
