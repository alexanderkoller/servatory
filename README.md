# health-stick

health-stick turns an M5Stack StickS3 into a small, USB-connected status display
for a Proxmox server. A daemon on the server collects host, storage, network,
UPS, backup, and guest information and sends it to the display. The StickS3 does
not use Wi-Fi or Bluetooth.

The display is configured from one YAML file. You choose which filesystems and
disks to monitor, which conditions count as warnings or critical failures, and
which views appear when you press the front button. Guest and storage views add
pages automatically when their contents do not fit on one screen.

## What you need

- an M5Stack StickS3 and a USB data cable;
- a Proxmox host;
- [Rust](https://rustup.rs/) on the computer that builds the host service.

The UPS view uses a local NUT server when `sources.ups.endpoint` is configured.
SMART monitoring requires `smartctl` with JSON support. health-stick queries
SMART data without waking sleeping disks.

## Build and install the host service

The host service is a Linux program and does not require macOS, Zig, or the
Espressif toolchain. Build it on the Proxmox host or another compatible Linux
computer:

```sh
cargo build --release -p health-stick-host
```

Copy the resulting binary and deployment files to the Proxmox host if you built
them elsewhere. Then install the service, configuration, and udev rule:

```sh
sudo install -m 0755 target/release/health-stick-host /usr/local/bin/
sudo install -d -m 0755 /etc/health-stick
sudo install -m 0644 deploy/health-stick.yaml /etc/health-stick/config.yaml
sudo install -m 0644 deploy/99-health-stick.rules /etc/udev/rules.d/
sudo install -m 0644 deploy/health-stick.service /etc/systemd/system/
sudo udevadm control --reload-rules
sudo systemctl daemon-reload
sudo systemctl enable --now health-stick.service
sudo udevadm trigger
```

The configuration command above is for a first installation. Do not overwrite
`/etc/health-stick/config.yaml` when updating an existing installation.

The supplied configuration assumes:

- a NUT UPS named `eaton@localhost`;
- filesystems at `/`, `/mnt/pve/hdd`, and `/mnt/pve/backup`;
- SMART devices `/dev/sda` through `/dev/sde`.

Edit these values to match your server. The [configuration
reference](docs/configuration.md) describes the complete YAML format, including
health rules, views, timings, and shutdown control.

After editing the installed file, validate it before restarting the service:

```sh
sudo /usr/local/bin/health-stick-host \
  --config /etc/health-stick/config.yaml \
  --check-config
sudo systemctl restart health-stick.service
```

The daemon rejects unknown schema fields, duplicate resource identifiers,
invalid health rules, unsupported LCD layouts, and display configurations that
are too large for the device.

## Optional: install remotely from a Mac

The supplied remote installer can cross-compile the host service on a Mac and
install it over SSH. This route is useful when you do not want a Rust toolchain
or build packages on the Proxmox host. It is not required to build or run the
service.

Install Zig and `cargo-zigbuild` on the Mac:

```sh
brew install zig
cargo install cargo-zigbuild
```

Connect the StickS3 to the Proxmox host, then run:

```sh
./deploy/install-remote.sh alex@pve.local
```

Replace `alex` with a regular server account that has `sudo` access and
`pve.local` with the server's hostname or IP address. The installer refuses root
SSH. It builds a static Linux binary, uploads the required files, and starts
`health-stick.service`. The first installation creates the configuration file;
later installations preserve it.

## Flash the StickS3

Install the Espressif Rust toolchain on the Mac once:

```sh
cargo install espup
espup install
```

Load the environment file printed by `espup`, then build locally and flash the
StickS3 while it remains connected to the Proxmox host:

```sh
./deploy/flash-remote-firmware.sh alex@pve.local
```

The script temporarily stops the health-stick service, flashes
`/dev/health-stick`, restores the service, and removes its temporary files. It
downloads a checksum-verified Linux `espflash` helper for the operation; it does
not install flashing tools on the server.

For the first flash over the factory UiFlow2 firmware, put the StickS3 into
download mode. Hold its side reset button for about two seconds, then release it
when the internal green LED flashes. Run the flashing command after the device
has entered that mode.

## Use the display

A short press on the front button advances to the next configured view. A
second press within 200 milliseconds opens the ABOUT screen, which shows the
firmware, daemon, and protocol versions. Press once to leave ABOUT.

Holding the front button for the configured duration requests a Proxmox
shutdown, but only when `actions.shutdown.enabled` is `true` and the daemon is
connected. The display follows guest shutdown progress and then shows the final
host power-off phase locally after the USB connection disappears. A request is
never saved and replayed after a disconnected session.

The StickS3 remains responsive without the daemon. Its offline screens
distinguish a missing USB data connection from a USB connection on which no
valid daemon session is active.

## Check the service

Show the current service state and recent messages with:

```sh
ssh alex@pve.local systemctl status health-stick.service
ssh alex@pve.local journalctl -u health-stick.service -n 100
```

The daemon starts even when the display is absent. It waits for
`/dev/health-stick`, reconnects when the device appears, and returns to waiting
after a disconnect.

## Try the display with sample data

The mock host sends deterministic sample metrics, VMs, and containers without
querying Proxmox. Connect the StickS3 directly to a Mac, Linux, or Windows
computer and run:

```sh
cargo run -p health-stick-mock-host
```

If more than one compatible serial device is present, select one explicitly:

```sh
cargo run -p health-stick-mock-host -- --device /dev/cu.usbmodem1101
```

A long press in mock mode prints the shutdown request and exits. It never shuts
down the computer.
