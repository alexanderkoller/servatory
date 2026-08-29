#!/usr/bin/env bash

set -euo pipefail

install_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
case "$install_dir" in
    /tmp/servatory-install.*) ;;
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

for required_file in servatory-host 99-servatory.rules servatory.service servatory.yaml; do
    if [[ ! -f "$install_dir/$required_file" ]]; then
        echo "Missing uploaded file: $required_file" >&2
        exit 1
    fi
done

chmod 0755 "$install_dir/servatory-host"
if ! "$install_dir/servatory-host" --help >/dev/null; then
    echo "The uploaded daemon cannot run on this server." >&2
    exit 1
fi

echo "Installing as $(id -un); sudo is used only for system changes."
sudo -v
privilege=(sudo)

echo "Installing systemd service and udev rule..."
for legacy_service in s3-display.service health-stick.service; do
    if systemctl cat "$legacy_service" >/dev/null 2>&1; then
        "${privilege[@]}" systemctl disable --now "$legacy_service"
    fi
done
"${privilege[@]}" rm -f \
    /etc/systemd/system/s3-display.service \
    /etc/systemd/system/health-stick.service \
    /etc/udev/rules.d/99-m5stick-s3.rules \
    /etc/udev/rules.d/99-health-stick.rules \
    /usr/local/bin/health-stick-host
for legacy_device in /dev/health-stick /dev/m5stick-s3; do
    if [[ -L "$legacy_device" ]]; then
        echo "Removing legacy device link $legacy_device..."
        "${privilege[@]}" rm -f -- "$legacy_device"
    fi
done
"${privilege[@]}" install -m 0755 \
    "$install_dir/servatory-host" /usr/local/bin/servatory-host
"${privilege[@]}" install -m 0644 \
    "$install_dir/99-servatory.rules" /etc/udev/rules.d/99-servatory.rules
"${privilege[@]}" install -m 0644 \
    "$install_dir/servatory.service" /etc/systemd/system/servatory.service
config_path=/etc/servatory/config.yaml
if [[ ! -e "$config_path" ]]; then
    "${privilege[@]}" install -d -m 0755 /etc/servatory
    "${privilege[@]}" install -m 0644 "$install_dir/servatory.yaml" "$config_path"
fi

# The packaged udev rule owns the stable device name. Enforce it on every
# deployment instead of carrying forward obsolete or machine-specific names.
"${privilege[@]}" sed -E -i \
    's#^([[:space:]]*device:[[:space:]]*).*$#\1/dev/servatory#' \
    "$config_path"

"${privilege[@]}" /usr/local/bin/servatory-host --config "$config_path" --check-config

"${privilege[@]}" udevadm control --reload-rules
"${privilege[@]}" systemctl daemon-reload
"${privilege[@]}" systemctl enable servatory.service
"${privilege[@]}" systemctl restart servatory.service
"${privilege[@]}" udevadm trigger
"${privilege[@]}" udevadm settle

if [[ -e /dev/servatory ]]; then
    echo "Installed and started servatory.service; the display is connected."
else
    echo "Installed and started servatory.service; it is waiting for the display."
fi
