#!/usr/bin/env bash

set -euo pipefail

install_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
case "$install_dir" in
    /tmp/health-stick-install.*) ;;
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

for required_file in health-stick-host 99-health-stick.rules health-stick.service health-stick.yaml; do
    if [[ ! -f "$install_dir/$required_file" ]]; then
        echo "Missing uploaded file: $required_file" >&2
        exit 1
    fi
done

chmod 0755 "$install_dir/health-stick-host"
if ! "$install_dir/health-stick-host" --help >/dev/null; then
    echo "The uploaded daemon cannot run on this server." >&2
    exit 1
fi

echo "Installing as $(id -un); sudo is used only for system changes."
sudo -v
privilege=(sudo)

echo "Installing systemd service and udev rule..."
if systemctl cat s3-display.service >/dev/null 2>&1; then
    "${privilege[@]}" systemctl disable --now s3-display.service
fi
"${privilege[@]}" rm -f \
    /etc/systemd/system/s3-display.service \
    /etc/udev/rules.d/99-m5stick-s3.rules
"${privilege[@]}" install -m 0755 \
    "$install_dir/health-stick-host" /usr/local/bin/health-stick-host
"${privilege[@]}" install -m 0644 \
    "$install_dir/99-health-stick.rules" /etc/udev/rules.d/99-health-stick.rules
"${privilege[@]}" install -m 0644 \
    "$install_dir/health-stick.service" /etc/systemd/system/health-stick.service
if [[ ! -e /etc/health-stick/config.yaml ]]; then
    "${privilege[@]}" install -d -m 0755 /etc/health-stick
    "${privilege[@]}" install -m 0644 "$install_dir/health-stick.yaml" /etc/health-stick/config.yaml
fi
"${privilege[@]}" /usr/local/bin/health-stick-host --config /etc/health-stick/config.yaml --check-config

"${privilege[@]}" udevadm control --reload-rules
"${privilege[@]}" systemctl daemon-reload
"${privilege[@]}" systemctl enable health-stick.service
"${privilege[@]}" systemctl restart health-stick.service
"${privilege[@]}" udevadm trigger
"${privilege[@]}" udevadm settle

if [[ -e /dev/health-stick ]]; then
    echo "Installed and started health-stick.service; the display is connected."
else
    echo "Installed and started health-stick.service; it is waiting for the display."
fi
