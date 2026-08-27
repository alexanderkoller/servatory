#!/usr/bin/env bash

set -euo pipefail

install_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
case "$install_dir" in
    /tmp/s3-display-install.*) ;;
    *)
        echo "Refusing to run from unexpected directory: $install_dir" >&2
        exit 1
        ;;
esac

cleanup_remote() {
    rm -rf -- "$install_dir"
}
trap cleanup_remote EXIT

if [[ $(id -u) -eq 0 ]]; then
    echo "Refusing to deploy as root." >&2
    echo "Run this installer through a regular account with sudo access." >&2
    exit 1
fi
if ! command -v sudo >/dev/null 2>&1; then
    echo "The remote account needs sudo access to install the service." >&2
    exit 1
fi

for required_file in s3-display-host 99-m5stick-s3.rules s3-display.service; do
    if [[ ! -f "$install_dir/$required_file" ]]; then
        echo "Missing uploaded file: $required_file" >&2
        exit 1
    fi
done

chmod 0755 "$install_dir/s3-display-host"
if ! "$install_dir/s3-display-host" --help >/dev/null; then
    echo "The uploaded daemon cannot run on this server." >&2
    exit 1
fi

echo "Installing as $(id -un); sudo is used only for system changes."
sudo -v
privilege=(sudo)

echo "Installing systemd service and udev rule..."
"${privilege[@]}" install -m 0755 \
    "$install_dir/s3-display-host" /usr/local/bin/s3-display-host
"${privilege[@]}" install -m 0644 \
    "$install_dir/99-m5stick-s3.rules" /etc/udev/rules.d/99-m5stick-s3.rules
"${privilege[@]}" install -m 0644 \
    "$install_dir/s3-display.service" /etc/systemd/system/s3-display.service

"${privilege[@]}" udevadm control --reload-rules
"${privilege[@]}" systemctl daemon-reload
"${privilege[@]}" systemctl enable s3-display.service
"${privilege[@]}" systemctl restart s3-display.service
"${privilege[@]}" udevadm trigger
"${privilege[@]}" udevadm settle

if [[ -e /dev/m5stick-s3 ]]; then
    echo "Installed and started s3-display.service; the display is connected."
else
    echo "Installed and started s3-display.service; it is waiting for the display."
fi
