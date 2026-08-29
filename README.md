# Health Stick

This repository contains a tiny USB-connected Proxmox health display. Its
240x135 landscape display shows configurable health, resource, storage, power,
network, and guest views.

- `firmware/`: Rust firmware for the M5Stick S3 (ESP32-S3), with a heap for dynamic manifests and snapshots.
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

The front button changes between configured health screens immediately on a
short press. A second press within 200 ms opens a persistent ABOUT screen with
firmware and daemon build identifiers (package version plus Git revision) and
the protocol version; one short press returns to the normal
display. ABOUT never closes on a timer.
Guest-list views are paginated over every guest supplied by the daemon. The
configured long press during a live daemon session sends a shutdown request. The host
acknowledges that request and immediately queues `/usr/bin/systemctl poweroff`.
The daemon remains alive while Proxmox's native `pve-guests` service shuts down
the guests, reporting the live number remaining. The display then animates the
final host phase locally, since the daemon and USB connection disappear during
poweroff. It distinguishes a lost daemon heartbeat while USB data remains active
from a confirmed loss of the USB data link; neither state claims that the host
has finished powering off. Shutdown handling is disabled unless the daemon is
started with `--allow-shutdown`.

## Prerequisites

Install Rust normally for the host. For firmware, install the Espressif Rust
toolchain and flashing tool:

```sh
cargo install espup espflash
espup install
```

After `espup install`, load the environment file it prints in new shells. The
current generic Xtensa linker also needs `XTENSA_GNU_CONFIG` to name its ESP32-S3
configuration on macOS; `deploy/flash-remote-firmware.sh` derives and exports it
automatically from the compiler installation. The
firmware pins follow M5Stack's StickS3 map. The LCD controller RAM offsets
(`52, 40`) are the only bring-up assumption that should be confirmed on hardware.

## Build and test

```sh
# Host and shared protocol
cargo test --workspace
cargo build --release -p health-stick-host

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
./deploy/flash-remote-firmware.sh alex@192.168.1.50
```

The script builds the firmware on the Mac, downloads and verifies the official
Linux `espflash` release, and uploads both to a temporary directory. On Proxmox,
it temporarily stops and runtime-masks the display service so it cannot seize the
USB device during resets, flashes `/dev/health-stick`, restores the service, and
removes the temporary files. Rust and `espflash` are not installed on the server.

For the first flash from the factory UiFlow2 firmware, manually enter download
mode by holding the StickS3 side reset button for about two seconds and releasing
it when the internal green LED flashes. The configured `watchdog-reset` post-flash
strategy exits native USB download mode and boots the application.

## Try the host without installing it

Identify the device (usually `/dev/ttyACM0` before installing the udev rule):

```sh
cargo run --release -p health-stick-host -- --config deploy/health-stick.yaml
```

Add `--allow-shutdown` only when you are ready to test the long-press path.

## Test with made-up guests

The mock host runs on macOS, Linux, or Windows. It auto-detects the StickS3 when
exactly one matching ESP32-S3 USB Serial/JTAG device is connected:

```sh
cargo run -p health-stick-mock-host
```

If auto-detection is ambiguous, select the serial device explicitly (for example,
`/dev/cu.usbmodem...` on macOS):

```sh
cargo run -p health-stick-mock-host -- --device /dev/cu.usbmodem1101
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
ssh alex@pve.local systemctl status health-stick.service
```

The installed service reads `/etc/health-stick/config.yaml`. The installer
creates this file from `deploy/health-stick.yaml` only when it does not already
exist, so upgrades preserve local changes. The supplied configuration queries
the local NUT server as `eaton@localhost` and monitors `/dev/sda` through
`/dev/sde` with the display labels `ROOT`, `HDD`, `BACKUP`, `SDD`, and `SDE`.
UPS access is status-only and anonymous; the daemon never uses NUT control or
shutdown credentials. SMART collection requires `smartctl` with JSON support
and uses `-n standby`, so a health update does not wake a sleeping disk.

### Manual installation on Proxmox

If you already have the matching static Linux binary, copy it to the Proxmox
machine along with `deploy/`, then install it without any server-side build:

```sh
sudo install -m 0755 health-stick-host /usr/local/bin/
sudo install -d -m 0755 /etc/health-stick
sudo install -m 0644 deploy/health-stick.yaml /etc/health-stick/config.yaml
sudo install -m 0644 deploy/99-health-stick.rules /etc/udev/rules.d/
sudo install -m 0644 deploy/health-stick.service /etc/systemd/system/
sudo udevadm control --reload
sudo udevadm trigger
sudo systemctl daemon-reload
sudo systemctl enable --now health-stick.service
```

The udev rule matches Espressif's standard USB Serial/JTAG VID/PID. If the server
has multiple matching boards, extend the rule with the StickS3's serial number.

## Health data

Every update contains a dynamically sized health snapshot. The production daemon reads
CPU, memory, load, uptime, and I/O pressure from `/proc`, plus usage and available
space for `/`, `/mnt/pve/hdd`, and `/mnt/pve/backup` from `df`. For networking,
it identifies the interface and source address used by the
IPv4 route to the internet, resolves Proxmox bridges, bonds, and VLANs down to
the active physical port, and reads that port's carrier and negotiated speed
from `/sys/class/net`. This avoids mistaking a fast virtual guest interface for
the host's Ethernet connection.

UPS health comes from the read-only `upsc` status fields for battery charge,
runtime, load, state, and approximate real power. The watt figure is displayed
with a `~` marker and never drives health decisions. On-battery and bypass states
are warnings; low battery, replace-battery, and output-off states are critical.
Two consecutive query failures produce an unavailable warning, while the first
failure retains the last values as stale.

SMART health is collected for the explicitly named disks. Storage and SMART
views are paginated as needed. Each row shows
the human-readable label, health state, and temperature when available. Sleeping
disks are neutral, unknown or degraded results are warnings, and a failed SMART
self-assessment is critical.

An asynchronous IPv4 ping to `www.google.de` runs every 30 seconds with a hard
four-second timeout. After a link comes up, the daemon allows three seconds for
the network path to settle; if that first probe misses, it retries after five
seconds while the display continues to show `CHECKING`. One subsequent missed
probe is a warning and two consecutive misses are reported as `PING FAILED`;
the detail screen then shows the elapsed time since the last successful probe.
Backup health comes from enabled jobs returned by `pvesh get /cluster/backup`
and recent `vzdump` task history. A failed latest task, a missing job, or a last
successful backup older than 24 hours produces a warning. Guest state comes from
`pvesh get /cluster/resources --type vm`; the resulting list is paginated on the
display without a configured guest-count ceiling.

The mock host sends deterministic values matching the Split View design, so all
six pages can be exercised away from a Proxmox installation.

## Configuration

The production daemon requires `/etc/health-stick/config.yaml` unless another
path is supplied with `--config`. Run `health-stick-host --check-config` before
restarting the service after an edit. Unknown fields, duplicate identifiers,
unsupported layouts, and display manifests exceeding the transport/device
budget are rejected at startup.

The YAML file configures collector paths and timings, stable resource IDs and
display labels, the ordered health rules and their messages, reusable views,
per-output view order, and shutdown interaction timing. Health rules exist only in the
top-level `health` section and are evaluated in order; the first matching rule
becomes the primary condition. Layouts describe structure while their children
name the content, such as `columns: { left: ups, right: network }`.

The host sends the validated LCD manifest to the Stick at session startup. An
optional `outputs.http` section already selects and orders reusable views for a
future on-Stick HTTP server; it is disabled in the supplied configuration.
Wi-Fi credentials are intentionally outside this configuration model and will
be provisioned only into the Stick's persistent memory. The future HTTP output
will consume the same measurements and single centrally evaluated health result.

## Wire protocol

Both sides import the same Rust `HostMessage` and `DeviceMessage` enums from the
`no_std` protocol crate. Messages are serialized with Postcard and framed with
COBS using a zero-byte delimiter. A magic-and-version prefix precedes the encoded body, so a
receiver rejects an incompatible protocol before attempting to decode a schema
it may not understand. The overall frame budget is 16 KiB; collection counts and
text fields are heap-backed and have no separate firmware maxima. The firmware
reserves a 64 KiB heap for decoded manifests and snapshots.

The daemon sends `Hello` immediately after opening the serial device and retries
until the Stick answers with its own version-compatible `Hello`. The Stick also
announces itself at boot and once per second while it has no live session. This
covers either side starting first, USB removal/reinsertion, daemon restarts, and
firmware resets that leave the serial port open. A fresh Stick `Hello` makes the
daemon resend the display manifest and a fresh daemon `Hello` resets the Stick's
session. Exact version mismatches are logged by the daemon and shown as an
upgrade-required state on the Stick; a silent peer produces a diagnostic after
five seconds while retries continue. Compatible Hello messages also exchange
the daemon and firmware build identifiers used by the ABOUT screen. These use
the form `0.1.0+g0123456789`, with `.dirty` appended when built from a modified
working tree and `gunknown` when Git metadata is unavailable.
