# Servatory configuration reference

The Servatory daemon reads one YAML document at startup. The default path is
`/etc/servatory/config.yaml`; use `--config PATH` to select another file.
Configuration changes take effect only after the daemon restarts.

Start with the supplied [`deploy/servatory.yaml`](../deploy/servatory.yaml)
and change the resources and rules that differ on your server. Before restarting
the service, validate the result:

```sh
sudo /usr/local/bin/servatory-host \
  --config /etc/servatory/config.yaml \
  --check-config
```

A valid file prints a confirmation and exits. An invalid file reports the field
or rule that must be corrected. Unknown fields are errors, so misspelled option
names do not silently fall back to another behavior.

## Document structure

Every configuration contains these top-level sections:

```yaml
version: 1
host: ...
connection: ...
actions: ...
sources: ...
health: ...
views: ...
outputs: ...
```

All sections are required. `version` must currently be `1`.

Duration values are strings containing a non-negative integer followed by
`ms`, `s`, `m`, or `h`. For example, `200ms`, `5s`, and `24h` are valid. A
quoted integer without a suffix is interpreted as milliseconds, but an explicit
suffix is clearer. Fractions and compound values such as `1.5s` and `1m30s` are
not supported. Fields described as positive must be greater than zero.

## Update and connection settings

```yaml
host:
  update_interval: 5s

connection:
  usb_serial:
    device: /dev/servatory
    reconnect_interval: 1s
```

`host.update_interval` controls how often the daemon collects and sends a full
health update. `connection.usb_serial.device` is the serial device to open. The
installed udev rule creates `/dev/servatory`; use a platform-specific serial
path only when running without that rule. `reconnect_interval` controls how
long the daemon waits after a failed connection attempt or disconnect.

Both duration fields must be positive.

## Shutdown action

```yaml
actions:
  shutdown:
    enabled: true
    hold_time: 3s
    animation_delay: 200ms
```

When `enabled` is `true`, holding the front button for `hold_time` asks the
daemon to run the system shutdown path. Set it to `false` if the display should
be read-only. This setting replaces the former command-line shutdown switch.

`animation_delay` delays the on-device hold animation so an ordinary press does
not briefly show shutdown progress. It must be shorter than `hold_time`. The
firmware represents both values in milliseconds, and each value must therefore
fit in an unsigned 16-bit integer (at most 65,535 ms).

## Data sources

The `sources` section tells the daemon what to measure. Its child sections are
all required, even when a source has no configured items.

### Host metrics

```yaml
sources:
  system:
    provider: procfs
```

`procfs` is the only supported provider. It supplies CPU use, memory use, load,
uptime, and I/O pressure from the Linux host.

### Filesystems

```yaml
sources:
  filesystems:
    - id: root
      path: /
      label: "/"
    - id: backup
      path: /mnt/pve/backup
      label: BACKUP
```

At least one filesystem is required. Each entry has:

- `id`: a stable name used by health rules and views;
- `path`: an absolute mount path passed to `df`;
- `label`: the text shown on the display.

An ID may contain ASCII letters, digits, `_`, and `-`. IDs must be non-empty and
unique among filesystems. Keep an ID stable when changing a label because rules
refer to the ID, not the label.

### SMART devices

```yaml
sources:
  smart:
    devices:
      - { id: root, path: /dev/sda, label: ROOT }
      - { id: backup, path: /dev/sdb, label: BACKUP }
```

Each device has a stable `id`, a non-empty display `label`, and a `path` that
starts with `/dev/`. SMART IDs follow the same character and uniqueness rules as
filesystem IDs, but the two groups have separate namespaces. The list may be
empty. Servatory uses `smartctl -n standby`, so collecting a status does not
wake a sleeping disk.

### UPS

```yaml
sources:
  ups:
    endpoint: eaton@localhost
    failures_before_unavailable: 2
```

`endpoint` is the target passed to the NUT `upsc` command. Set it to `null` when
no UPS should be queried. The daemon only reads status values and does not use
NUT control or shutdown credentials.

After a successful query, a temporary failure retains the previous UPS values
as stale. `failures_before_unavailable` sets the number of consecutive failures
after which the UPS becomes unavailable; it must be positive.

### Network interface

```yaml
sources:
  network:
    interface: default_ipv4_route
    resolve_physical_interface: true
```

These are currently fixed compatibility values. The daemon finds the interface
used by the default IPv4 route, then follows Proxmox bridges, bonds, and VLANs to
the active physical port. Other values are rejected.

### Internet probe

```yaml
sources:
  internet:
    target: www.google.de
    interval: 30s
    timeout: 4s
    link_up_settle_delay: 3s
    first_failure_retry_delay: 5s
    failures_before_failed: 2
```

The daemon runs an asynchronous IPv4 ping to `target`. `interval` is the normal
time between probes, and `timeout` limits one probe. After a link comes up,
`link_up_settle_delay` allows the route to become usable. If the first probe
then fails, `first_failure_retry_delay` controls the quicker retry.
`failures_before_failed` sets the number of consecutive missed probes reported
as `failed` and must be positive.

The target must be non-empty. All duration fields are required; `interval` and
`timeout` must be positive.

### Proxmox

```yaml
sources:
  proxmox:
    node: local_hostname
    backup:
      task_history_limit: 50
```

`local_hostname` is currently the only supported node selection. The daemon
queries enabled backup jobs and inspects up to `task_history_limit` recent
tasks. The limit must be positive.

## Health evaluation

The `health` section converts measurements into one primary status and message:

```yaml
health:
  default: { severity: healthy, message: HEALTHY }
  rules:
    - id: root_full
      severity: critical
      when:
        measurement: filesystem.root.used_percent
        greater_than: 90
      message: "{filesystem.root.label} {value}% FULL"
    - id: cpu_high
      severity: warning
      when: { measurement: system.cpu.percent, at_least: 85 }
      message: "CPU {value}%"
```

Rules are evaluated from top to bottom. The first matching rule supplies the
displayed severity and message; later rules are not considered for that update.
Put the conditions that should take priority first. If no rule matches,
`health.default` is used.

Rule IDs must be unique, non-empty, and limited to ASCII letters, digits, `_`,
and `-`. A severity is `healthy`, `warning`, or `critical`. Messages must be
non-empty.

### Conditions

A simple condition names one measurement and exactly one comparison:

```yaml
when: { measurement: system.cpu.percent, at_least: 85 }
```

The comparisons are:

| Comparison | Meaning |
| --- | --- |
| `equals` | The value equals a scalar value. |
| `in` | The value equals any member of a list. |
| `at_least` | A number is greater than or equal to the threshold. |
| `greater_than` | A number is strictly greater than the threshold. |
| `missing_or_greater_than` | A duration is absent or older than the threshold. |

Combine conditions with non-empty `all` and `any` lists. They may be nested:

```yaml
when:
  all:
    - { measurement: proxmox.backup.status, equals: healthy }
    - measurement: proxmox.backup.last_success_age
      missing_or_greater_than: 24h
```

`all` requires every child condition to match. `any` requires at least one.

### Measurements and values

| Measurement | Type and accepted text values |
| --- | --- |
| `system.cpu.percent` | number |
| `system.memory.used_percent` | number |
| `system.io_pressure.percent` | number |
| `filesystem.<id>.mounted` | Boolean |
| `filesystem.<id>.used_percent` | number |
| `smart.*.status` | `healthy`, `warning`, `failed`, `sleeping`, `unknown` |
| `ups.main.status` | `not_configured`, `unknown`, `online`, `on_battery`, `low_battery`, `charging`, `bypass`, `output_off`, `replace_battery`, `unavailable` |
| `ups.main.stale` | Boolean |
| `network.uplink.up` | Boolean |
| `network.internet.status` | `checking`, `reachable`, `missed`, `failed` |
| `proxmox.backup.status` | `unknown`, `no_job`, `healthy`, `running`, `failed`, `stale` |
| `proxmox.backup.last_success_age` | duration, or missing when no successful backup is known |

Replace `<id>` with an ID declared in `sources.filesystems`. The only supported
wildcard measurement is `smart.*.status`; it matches when any configured SMART
device satisfies the comparison.

Use comparisons that suit the measurement type. For example, compare Boolean
measurements with `equals: true` or `equals: false`, statuses with `equals` or
`in`, numeric measurements with `at_least` or `greater_than`, and backup age
with `missing_or_greater_than`.

### Message substitutions

Messages support two substitutions:

- `{value}` inserts the numeric measurement value, rounded to a whole number;
- `{filesystem.<id>.label}` inserts the configured label for that filesystem.

For predictable output, use `{value}` in a rule whose condition contains one
relevant numeric measurement, and do not reuse that message template in another
rule. Other text in a message is displayed literally.

## Views

`views` defines reusable named views. Each view has a display `title` and a
supported `layout`. The key under `views` is the view ID used by the output
lists.

The current firmware supports five layout families.

### Overview

```yaml
views:
  overview:
    title: OVERVIEW
    layout:
      columns:
        left: health
        right: { rows: [host_summary, network_summary, guest_summary] }
```

The overview places the primary health result on the left and host, network,
and guest summaries on the right.

### Resources

```yaml
views:
  resources:
    title: RESOURCES
    layout:
      grid: { columns: 2, children: [cpu, memory, io_pressure, load_average] }
```

The resources layout shows CPU, memory, I/O pressure, and load information.

### Storage and SMART

```yaml
views:
  storage:
    title: STORAGE + SMART
    layout:
      columns:
        left: { filesystems: [root, backup], footer: proxmox_backup }
        right: { smart: [root, backup] }
```

The lists contain IDs declared under `sources.filesystems` and
`sources.smart.devices`. Unknown IDs are rejected. A page holds up to three
filesystems and five SMART devices; the daemon creates additional pages as
needed. Put `filesystems` on the right and `smart` on the left to reverse the
two columns.

### Power and network

```yaml
views:
  power_network:
    title: UPS + ETHERNET
    layout:
      columns: { left: ups, right: network }
```

The `ups` and `network` children may exchange sides.

### Guests

For the normal paginated guest list, use:

```yaml
views:
  guests:
    title: GUESTS
    layout:
      single: { guest_list: { paginate: true } }
```

The daemon creates one page per four guests and updates the page count when the
guest list changes. To show one fixed slice instead, omit `paginate: true` and
set `offset` and `limit`:

```yaml
layout:
  single: { guest_list: { offset: 0, limit: 4 } }
```

`offset` defaults to `0` and `limit` defaults to `4`.

Selected LCD layouts outside these five families are rejected because the
current firmware cannot render them. Titles, view definitions, and generated
pages must also fit within the device's 16 KiB configuration frame.

## Outputs

```yaml
outputs:
  stick:
    lcd:
      views: [overview, resources, storage, power_network, guests]
  http:
    enabled: false
    views: [overview, resources, storage, power_network, guests]
```

`outputs.stick.lcd.views` selects reusable view IDs and sets their button order.
The list must not be empty, and every ID must exist under `views`.

`outputs.http` is optional and reserved for a future on-Stick HTTP server. The
current firmware does not provide that server, even if `enabled` is `true`.
Keep it disabled. Wi-Fi credentials are not part of this YAML format.
