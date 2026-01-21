#!/bin/bash
set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
RULES_FILE="${SCRIPT_DIR}/99-piper-gs-usb.rules"
TARGET="/etc/udev/rules.d/99-piper-gs-usb.rules"

if [ ! -f "$RULES_FILE" ]; then
    echo "Error: Rules file not found: $RULES_FILE"
    exit 1
fi

echo "Installing udev rules for GS-USB devices..."
sudo cp "$RULES_FILE" "$TARGET"
sudo chmod 644 "$TARGET"

echo "Reloading udev rules..."
sudo udevadm control --reload-rules
sudo udevadm trigger

echo "Done! You may need to unplug and replug your GS-USB device."
echo ""
echo "To add your user to the plugdev group (if not already):"
echo "  sudo usermod -aG plugdev $USER"
echo "  (You may need to log out and log back in for this to take effect)"

