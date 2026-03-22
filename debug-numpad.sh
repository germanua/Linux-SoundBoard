#!/usr/bin/env bash

echo "=== Numpad Key Debugging ==="
echo

echo "Testing NumpadSubtract key (keycode 82):"
xmodmap -pke | grep "keycode  82"
echo

echo "The app needs to:"
echo "1. Recognize the key name from GTK (could be KP_Subtract or minus)"
echo "2. Map it to XKB name KPSU"
echo "3. Register grab for keycode 82"
echo

echo "Let's check if the app can find KPSU in XKB:"
xkbcomp $DISPLAY - 2>/dev/null | grep -A 3 "KPSU ="

echo
echo "Now let's test the app with logging enabled."
echo "Run: RUST_LOG=debug ~/.local/opt/linux-soundboard/linux-soundboard"
echo
echo "Then try to assign NumpadSubtract as a hotkey and watch for:"
echo "  - 'normalize_capture_key' calls"
echo "  - 'register_hotkey_blocking' calls"
echo "  - Any error messages about grab failures"
