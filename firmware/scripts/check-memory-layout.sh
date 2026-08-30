#!/usr/bin/env bash

set -euo pipefail

firmware_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
elf=${1:-$firmware_dir/target/xtensa-esp32s3-none-elf/release/servatory-firmware}
minimum_stack=$((128 * 1024))
maximum_notification_task=$((14 * 1024))

if [[ ! -f "$elf" ]]; then
    echo "Firmware ELF not found: $elf" >&2
    exit 1
fi
for tool in xtensa-esp-elf-size xtensa-esp-elf-nm; do
    if ! command -v "$tool" >/dev/null 2>&1; then
        echo "Required Xtensa tool not found: $tool" >&2
        exit 1
    fi
done

sections=$(xtensa-esp-elf-size -A "$elf")
symbols=$(xtensa-esp-elf-nm -S -C "$elf")
stack_size=$(awk '$1 == ".stack" { print $2; exit }' <<<"$sections")
notification_hex=$(awk '/notification_worker::POOL$/ { print $2; exit }' <<<"$symbols")

if [[ ! "$stack_size" =~ ^[0-9]+$ ]]; then
    echo "Could not read the .stack section from $elf" >&2
    exit 1
fi
if [[ ! "$notification_hex" =~ ^[0-9A-Fa-f]+$ ]]; then
    echo "Could not read the notification task pool from $elf" >&2
    exit 1
fi
notification_size=$((16#$notification_hex))

if (( stack_size < minimum_stack )); then
    echo "Internal stack reserve is too small: $stack_size bytes (minimum $minimum_stack)" >&2
    exit 1
fi
if (( notification_size > maximum_notification_task )); then
    echo "Notification task state is too large: $notification_size bytes (maximum $maximum_notification_task)" >&2
    exit 1
fi
if grep -Fq 'SCREEN_BUFFER' <<<"$symbols"; then
    echo "Legacy SRAM SCREEN_BUFFER is still linked" >&2
    exit 1
fi
if ! grep -Fq 'servatory_firmware::memory::PSRAM_HEAP' <<<"$symbols"; then
    echo "Dedicated PSRAM heap is not linked" >&2
    exit 1
fi

printf 'Memory layout OK: stack=%d bytes, notification task=%d bytes, framebuffer/workspace=PSRAM\n' \
    "$stack_size" "$notification_size"
