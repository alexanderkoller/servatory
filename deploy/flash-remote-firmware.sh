#!/usr/bin/env bash

set -euo pipefail

ESPFLASH_VERSION=4.5.0

usage() {
    echo "Usage: $0 [user@]linux-host" >&2
    echo "Example: $0 koller@192.168.1.50" >&2
    echo "Build locally and flash the M5Stick attached to the Linux host." >&2
}

if [[ $# -eq 1 && ("$1" == -h || "$1" == --help) ]]; then
    usage
    exit 0
fi
if [[ $# -ne 1 || "$1" == -* ]]; then
    usage
    exit 2
fi

remote_host=$1
if [[ ! "$remote_host" =~ ^([A-Za-z0-9_.%+-]+@)?[A-Za-z0-9.-]+$ ]] && \
    [[ ! "$remote_host" =~ ^([A-Za-z0-9_.%+-]+@)?\[[0-9A-Fa-f:]+\]$ ]]; then
    echo "Invalid SSH destination: $remote_host" >&2
    exit 2
fi
if [[ "$remote_host" == root@* ]]; then
    echo "Refusing root SSH. Use a regular account with sudo access." >&2
    exit 2
fi

for command_name in cargo rustup curl shasum unzip ssh scp file grep; do
    if ! command -v "$command_name" >/dev/null 2>&1; then
        echo "Required local command not found: $command_name" >&2
        exit 1
    fi
done

espup_export=${ESPUP_EXPORT_FILE:-${HOME:?}/export-esp.sh}
if [[ -f "$espup_export" ]]; then
    # espup writes the Xtensa compiler and LLVM paths here on macOS/Linux.
    source "$espup_export"
fi

if ! rustup run esp rustc --version >/dev/null 2>&1; then
    echo "The local Espressif Rust toolchain is not installed." >&2
    echo "Run 'espup install', then load the environment file it creates." >&2
    exit 1
fi
if ! command -v xtensa-esp-elf-gcc >/dev/null 2>&1; then
    echo "The Espressif linker is not on PATH." >&2
    echo "Load '$espup_export' or set ESPUP_EXPORT_FILE to its location." >&2
    exit 1
fi
if [[ -z ${XTENSA_GNU_CONFIG:-} ]]; then
    xtensa_compiler=$(command -v xtensa-esp-elf-gcc)
    xtensa_root=$(cd -- "$(dirname -- "$xtensa_compiler")/.." && pwd)
    xtensa_config=$xtensa_root/lib/xtensa_esp32s3.so
    if [[ ! -f "$xtensa_config" ]]; then
        echo "The ESP32-S3 Xtensa linker configuration is missing: $xtensa_config" >&2
        exit 1
    fi
    export XTENSA_GNU_CONFIG=$xtensa_config
fi

ssh_options=(-o ConnectTimeout=10 -o ConnectionAttempts=1)
echo "Inspecting $remote_host..."
remote_uid=$(ssh "${ssh_options[@]}" -- "$remote_host" 'id -u')
if [[ "$remote_uid" == 0 ]]; then
    echo "Refusing to flash through a UID 0 SSH session." >&2
    exit 1
elif [[ ! "$remote_uid" =~ ^[0-9]+$ ]]; then
    echo "The server returned an unexpected user ID: $remote_uid" >&2
    exit 1
fi

remote_arch=$(ssh "${ssh_options[@]}" -- "$remote_host" 'uname -m')
case "$remote_arch" in
    x86_64 | amd64)
        espflash_target=x86_64-unknown-linux-musl
        espflash_sha256=542c5cc81f0cca384cbead1cacb7ccc9f35072a989b2de0fb95333d814272c22
        ;;
    aarch64 | arm64)
        espflash_target=aarch64-unknown-linux-gnu
        espflash_sha256=2d5972b9c18fc89bf253e60fe6df6a4f8db3aee5db0166b2c97b53bd21c01f09
        ;;
    *)
        echo "Unsupported server architecture: $remote_arch" >&2
        exit 1
        ;;
esac

if ! ssh "${ssh_options[@]}" -- "$remote_host" 'test -e /dev/servatory'; then
    echo "The server does not currently expose /dev/servatory." >&2
    echo "Check that the Stick is connected and the repository's udev rule is installed." >&2
    exit 1
fi

script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
repo_dir=$(cd -- "$script_dir/.." && pwd)
firmware_dir=$repo_dir/firmware
firmware_target=xtensa-esp32s3-none-elf
firmware_binary=$firmware_dir/target/$firmware_target/release/servatory-firmware

echo "Building release firmware locally..."
(cd "$firmware_dir" && cargo build --locked --release)
"$firmware_dir/scripts/check-memory-layout.sh" "$firmware_binary"

tool_dir=$repo_dir/target/remote-tools/espflash-$ESPFLASH_VERSION/$espflash_target
archive=$tool_dir/espflash.zip
remote_espflash=$tool_dir/espflash
mkdir -p "$tool_dir"

if [[ ! -f "$archive" ]]; then
    url=https://github.com/esp-rs/espflash/releases/download/v$ESPFLASH_VERSION/espflash-$espflash_target.zip
    echo "Downloading the official espflash $ESPFLASH_VERSION Linux helper..."
    curl -fL "$url" -o "$archive"
fi

if ! printf '%s  %s\n' "$espflash_sha256" "$archive" | shasum -a 256 -c -; then
    echo "The downloaded espflash archive failed checksum verification." >&2
    exit 1
fi
unzip -qo "$archive" -d "$tool_dir"
chmod 0755 "$remote_espflash"
if ! file "$remote_espflash" | grep -Fq 'ELF 64-bit'; then
    echo "The downloaded remote flashing helper is not a Linux ELF binary." >&2
    exit 1
fi

remote_stage=$(ssh "${ssh_options[@]}" -- "$remote_host" \
    'mktemp -d /tmp/servatory-flash.XXXXXX')
if [[ ! "$remote_stage" =~ ^/tmp/servatory-flash\.[A-Za-z0-9]+$ ]]; then
    echo "The server returned an unexpected temporary path: $remote_stage" >&2
    exit 1
fi

echo "Uploading firmware and the temporary Linux flashing helper..."
scp "${ssh_options[@]}" -- "$firmware_binary" "$remote_espflash" \
    "$script_dir/flash-on-host.sh" "$remote_host:$remote_stage/"

ssh -t "${ssh_options[@]}" -- "$remote_host" \
    "bash '$remote_stage/flash-on-host.sh'"
