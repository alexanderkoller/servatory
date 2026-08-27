#!/usr/bin/env bash

set -euo pipefail

flash_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
case "$flash_dir" in
    /tmp/s3-display-flash.*) ;;
    *)
        echo "Refusing to run from unexpected directory: $flash_dir" >&2
        exit 1
        ;;
esac

service=s3-display.service
service_masked=false

cleanup() {
    status=$?
    trap - EXIT
    set +e
    if [[ "$service_masked" == true ]]; then
        sudo systemctl unmask --runtime "$service"
        sudo udevadm settle
        if [[ -e /dev/m5stick-s3 ]]; then
            sudo systemctl start "$service"
        fi
    fi
    rm -rf -- "$flash_dir"
    exit "$status"
}
trap cleanup EXIT

if [[ $(id -u) -eq 0 ]]; then
    echo "Refusing to flash through a root SSH session." >&2
    exit 1
fi
if ! command -v sudo >/dev/null 2>&1; then
    echo "The remote account needs sudo access to flash the Stick." >&2
    exit 1
fi
for required_file in s3-display-firmware espflash; do
    if [[ ! -f "$flash_dir/$required_file" ]]; then
        echo "Missing uploaded file: $required_file" >&2
        exit 1
    fi
done
if [[ ! -e /dev/m5stick-s3 ]]; then
    echo "The M5Stick device /dev/m5stick-s3 is missing." >&2
    exit 1
fi

chmod 0755 "$flash_dir/espflash"
"$flash_dir/espflash" --version

echo "Temporarily stopping the display daemon..."
sudo -v
if systemctl cat "$service" >/dev/null 2>&1; then
    sudo systemctl mask --runtime "$service"
    service_masked=true
    sudo systemctl stop "$service"
fi

echo "Flashing the M5Stick attached to this server..."
sudo "$flash_dir/espflash" flash \
    --chip esp32s3 \
    --port /dev/m5stick-s3 \
    --non-interactive \
    --after watchdog-reset \
    --skip-update-check \
    "$flash_dir/s3-display-firmware"

echo "Firmware flashed successfully; restoring the display daemon."
