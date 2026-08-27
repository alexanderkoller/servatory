# M5Stick S3 Proxmox display

This repository contains a tiny USB-connected Proxmox health display. Its
240x135 landscape Split View shows an overview followed by resources,
storage/network, and guest screens.

- `firmware/`: allocation-free Rust firmware for the M5Stick S3 (ESP32-S3).
- `host/`: Linux daemon that sends regular updates and handles button events.
- `mock-host/`: portable test daemon with deterministic made-up guests and safe shutdown.
- `protocol/`: versioned, typed binary protocol shared by both sides.
- `deploy/`: a udev rule and hardened systemd unit for Proxmox/Debian.

The firmware is USB-only. It links no `esp-radio`, Wi-Fi, or Bluetooth stack, so
the RF PHY is never initialized and cannot provide wireless connectivity. USB
Serial/JTAG remains the sole host transport. We deliberately avoid active-mode
RTC power-domain register writes because the Bluetooth domain includes SRAM that
may be used by the application runtime.

The display boots and remains responsive without a USB host or daemon. Its
offline view distinguishes physical USB activity from a validated daemon
session. All USB transmission is non-blocking, and shutdown requests are never
queued across disconnected sessions. After an accepted shutdown, the first
valid update from the rebooted host returns the display to its normal status.

The front button changes between four health screens on a short press. Holding it
for three seconds during a live daemon session sends a shutdown request. The host
acknowledges that request on the display before invoking
`/usr/bin/systemctl poweroff`. Shutdown handling is disabled unless the daemon is
started with `--allow-shutdown`.

## Prerequisites

Install Rust normally for the host. For firmware, install the Espressif Rust
toolchain and flashing tool:

```sh
cargo install espup espflash
espup install
```

After `espup install`, load the environment file it prints in new shells. The
firmware pins follow M5Stack's StickS3 map. The LCD controller RAM offsets
(`52, 40`) are the only bring-up assumption that should be confirmed on hardware.

## Build and test

```sh
# Host and shared protocol
cargo test --workspace
cargo build --release -p s3-display-host

# Firmware (uses firmware/.cargo/config.toml and the `esp` toolchain)
cd firmware
cargo check
cargo run --release
```

`cargo run` flashes the connected board and boots it with a watchdog reset. It
does not open a text monitor because the application uses the same USB
Serial/JTAG interface for its binary protocol. The interface appears on Linux as
a CDC-ACM serial device. There is no meaningful baud rate for this hardware
interface; 115200 is specified for compatibility with serial APIs.

To build locally and reflash a Stick that remains attached to the Proxmox server,
use the deployment script with a regular SSH account:

```sh
./deploy/flash-firmware.sh alex@192.168.1.50
```

The script builds the firmware on the Mac, downloads and verifies the official
Linux `espflash` release, and uploads both to a temporary directory. On Proxmox,
it temporarily stops and runtime-masks the display service so it cannot seize the
USB device during resets, flashes `/dev/m5stick-s3`, restores the service, and
removes the temporary files. Rust and `espflash` are not installed on the server.

For the first flash from the factory UiFlow2 firmware, manually enter download
mode by holding the StickS3 side reset button for about two seconds and releasing
it when the internal green LED flashes. The configured `watchdog-reset` post-flash
strategy exits native USB download mode and boots the application.

## Try the host without installing it

Identify the device (usually `/dev/ttyACM0` before installing the udev rule):

```sh
cargo run --release -p s3-display-host -- --device /dev/ttyACM0
```

Add `--allow-shutdown` only when you are ready to test the long-press path.

## Test with made-up guests

The mock host runs on macOS, Linux, or Windows. It auto-detects the StickS3 when
exactly one matching ESP32-S3 USB Serial/JTAG device is connected:

```sh
cargo run -p s3-display-mock-host
```

If auto-detection is ambiguous, select the serial device explicitly (for example,
`/dev/cu.usbmodem...` on macOS):

```sh
cargo run -p s3-display-mock-host -- --device /dev/cu.usbmodem1101
```

It repeatedly sends a deterministic health snapshot with fake host metrics, VMs,
and containers. A three-second button hold is acknowledged like the production
daemon, but only prints a message and exits; it never invokes an operating-system
shutdown command.

## Install on Proxmox

### From a Mac on the same network

Enable SSH on the Proxmox host, then run this from the repository on your Mac:

```sh
./deploy/install-remote.sh alex@pve.local
```

Replace `alex` with your regular server account and `pve.local` with the server's
hostname or IP address. Root SSH is deliberately rejected. The installer detects
whether the server is x86_64 or AArch64, cross-compiles a static Linux binary on
the Mac with Zig, and uploads only that binary and the deployment files. The
server does not need Rust, a compiler, or build packages. The installer uses
`sudo` only to copy files into system locations and reload or start udev/systemd;
it may ask for the account's sudo password.

The Mac needs Zig and cargo-zigbuild:

```sh
brew install zig
cargo install cargo-zigbuild
```

The M5Stick connects to the Proxmox server, not the Mac. The installer starts the
daemon immediately whether or not the display is present. The daemon waits for
the USB serial device, connects when it appears, and returns to waiting after a
disconnect without exiting.

To check it later:

```sh
ssh alex@pve.local systemctl status s3-display.service
```

### Manual installation on Proxmox

If you already have the matching static Linux binary, copy it to the Proxmox
machine along with `deploy/`, then install it without any server-side build:

```sh
sudo install -m 0755 s3-display-host /usr/local/bin/
sudo install -m 0644 deploy/99-m5stick-s3.rules /etc/udev/rules.d/
sudo install -m 0644 deploy/s3-display.service /etc/systemd/system/
sudo udevadm control --reload
sudo udevadm trigger
sudo systemctl daemon-reload
sudo systemctl enable --now s3-display.service
```

The udev rule matches Espressif's standard USB Serial/JTAG VID/PID. If the server
has multiple matching boards, extend the rule with the StickS3's serial number.

## Health data

Every update contains a fixed-size health snapshot. The production daemon reads
CPU, memory, load, uptime, and I/O pressure from `/proc`; root usage from `df`;
link state and speed from `/sys/class/net`; and the first IPv4 address reported by
`hostname -I`. A mounted Proxmox storage path below `/mnt/pve/` is shown as the
backup connection. Guest state comes from `pvesh get /cluster/resources --type
vm` and is bounded to eight entries on the display.

The mock host sends deterministic values matching the Split View design, so all
four pages can be exercised away from a Proxmox installation.

## Wire protocol

Both sides import the same Rust `HostMessage` and `DeviceMessage` enums from the
`no_std` protocol crate. Messages are serialized with Postcard and framed with
COBS using a zero-byte delimiter. Receive buffers are statically bounded, and a
versioned envelope rejects incompatible schemas. Within one protocol version,
enum variants are append-only and must not be reordered.

The current health snapshot is allocation-free on the device and remains within
the protocol's fixed 512-byte frame buffer, including the maximum guest list.
