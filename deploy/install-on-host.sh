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
    privilege=()
elif command -v sudo >/dev/null 2>&1; then
    echo "Administrator access is required to install and start the service."
    sudo -v
    privilege=(sudo)
else
    echo "Run the installer as root or install sudo on the Proxmox host." >&2
    exit 1
fi

install_packages=()
command -v cc >/dev/null 2>&1 || install_packages+=(build-essential)
command -v curl >/dev/null 2>&1 || install_packages+=(curl ca-certificates)

if ((${#install_packages[@]})); then
    if ! command -v apt-get >/dev/null 2>&1; then
        echo "Missing build tools, and this host does not provide apt-get." >&2
        exit 1
    fi
    echo "Installing build prerequisites..."
    "${privilege[@]}" apt-get update
    "${privilege[@]}" apt-get install -y "${install_packages[@]}"
fi

source_dir=$install_dir/source
mkdir "$source_dir"
tar -C "$source_dir" -xzf "$install_dir/source.tar.gz"

cargo_command=()
if command -v cargo >/dev/null 2>&1 && command -v rustc >/dev/null 2>&1; then
    rust_version=$(rustc --version | awk '{print $2}')
    if printf '%s\n%s\n' 1.85.0 "$rust_version" | sort -V -C; then
        cargo_command=(cargo)
    fi
fi

if ((${#cargo_command[@]} == 0)); then
    echo "Installing a current Rust toolchain for the build user..."
    rustup_installer=$install_dir/rustup-init.sh
    curl --proto '=https' --tlsv1.2 -fsS https://sh.rustup.rs -o "$rustup_installer"
    sh "$rustup_installer" -y --profile minimal --default-toolchain stable
    cargo_command=("$HOME/.cargo/bin/cargo" +stable)
fi

echo "Building s3-display-host..."
(cd "$source_dir" && "${cargo_command[@]}" build --locked --release -p s3-display-host)

echo "Installing systemd service and udev rule..."
"${privilege[@]}" install -m 0755 \
    "$source_dir/target/release/s3-display-host" /usr/local/bin/s3-display-host
"${privilege[@]}" install -m 0644 \
    "$source_dir/deploy/99-m5stick-s3.rules" /etc/udev/rules.d/99-m5stick-s3.rules
"${privilege[@]}" install -m 0644 \
    "$source_dir/deploy/s3-display.service" /etc/systemd/system/s3-display.service

"${privilege[@]}" udevadm control --reload-rules
"${privilege[@]}" systemctl daemon-reload
"${privilege[@]}" systemctl enable s3-display.service
"${privilege[@]}" udevadm trigger
"${privilege[@]}" udevadm settle

if [[ -e /dev/m5stick-s3 ]]; then
    "${privilege[@]}" systemctl restart s3-display.service
    echo "Installed and started s3-display.service."
else
    echo "Installed s3-display.service. Plug the display into the Proxmox host to start it."
fi
