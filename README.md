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
queued across disconnected sessions.

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
./deploy/install-remote.sh root@pve.local
```

Replace `pve.local` with the server's hostname or IP address. The installer uses
SSH to copy the small host workspace, builds it on Proxmox, installs the daemon,
udev rule, and systemd unit, and enables the service. If you connect as a non-root
user, it uses `sudo` and may ask for that user's password. A current Rust toolchain
is installed for the remote build user only when the server does not already have
a sufficiently recent one.

The M5Stick must remain plugged into the Proxmox server, not the Mac. If it is
already connected, the installer starts the daemon immediately; otherwise udev
starts it when the display is plugged in.

To check it later:

```sh
ssh root@pve.local systemctl status s3-display.service
```

### Directly on Proxmox

Build the host binary on the Proxmox machine (or cross-compile it), then install:

```sh
sudo install -m 0755 target/release/s3-display-host /usr/local/bin/
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
