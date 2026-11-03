# asusio

ASUS Hardware I/O Framework for Linux.

## Goals

- Hardware discovery and capability detection
- Modular hardware abstraction and backends
- Safe, permission-controlled hardware operations
- Desktop-independent API
- CLI, GUI and desktop integrations

## Workspace

- `crates/` — reusable Rust libraries
- `apps/` — CLI, daemon, helper and GUI
- `hardware/` — board and device compatibility data
- `integrations/` — desktop integrations
- `packaging/` — distribution and service files
- `tests/` — reusable hardware fixtures

## Development

```sh
cargo check --workspace
cargo test --workspace
cargo clippy --workspace --all-targets
```

Discovery and descriptor inspection are read-only. The `lighting raw-output`
research command can write an explicit vendor payload only with `--execute`.
The vendor color protocol has not been verified.

## Sensor output

Use `asusio sensors` for terminal output or `asusio sensors --json` for
programmatic access. JSON contains `schema_version: 1`, a `devices` array,
and a `warnings` array for skipped entries. Each device has `sysfs_path`,
`device_id`, `name`, `display_name`, and `sensors`. Each sensor has a `label`
and a tagged `value`: `kind: "temperature"` with `milli_celsius`, or
`kind: "fan_speed_rpm"` with `rpm`. Devices without readable sensors are
omitted.

## Device output

Use `asusio devices` to list detected PCI and USB devices, or
`asusio devices --json` to read them as a JSON object with `schema_version: 1`,
`pci`, `usb`, and `warnings` arrays. PCI entries contain `address`, `vendor_id`, `device_id`, and
`class_id`. USB entries contain `sysfs_id`, `vendor_id`, `product_id`,
`manufacturer`, and `product`. Missing or blank optional USB names are
represented as `null`; devices without required IDs are omitted.

Add `--hid` to include HID devices. With `--json`, the result gains a `hid`
array containing HID IDs, name, driver, optional hidraw path, and report
descriptor size. The `hid` key is absent unless `--hid` is specified.
Warnings contain the sysfs `path` and a `reason`. In text mode they are
printed to standard error.

## Capability model

`asusio-types::Capability` keeps compatibility (`support`), local evidence
(`detection`), and OS-level read/write access separate. A detected capability
can still be unsupported by a backend or inaccessible to the current user.
OS-level write access does not authorize hardware writes; policy and backend
verification are separate requirements. Capability `id` values are intended
to be stable machine-readable keys.
`asusio-core::CapabilityRegistry` stores and queries capabilities by ID and
device ID. Run `asusio capabilities` or `asusio capabilities --json` to inspect
the capabilities derived from readable hwmon sensors and matching USB/HID IDs.
The JSON output contains `schema_version: 1`, `capabilities`, and `warnings`.
Successful sensor reads verify read access only. USB/HID ID matches are
candidates; they do not establish protocol or RGB control support.

## HID report descriptor diagnostics

Use `asusio hid-descriptor <device-id>` or add `--json` to inspect the
read-only report layout from `/sys/bus/hid/devices/<device-id>/report_descriptor`.
Get the device ID from `asusio devices --hid`. This command reports Input,
Output, and Feature payload bits and bytes separately from transfer bytes;
transfer bytes include the one-byte Report ID prefix when present. JSON uses
an independent `descriptor` object with `schema_version: 1` and does not
change the `HidDevice` output from `devices --hid --json`.

## Native Aura HID transport

Use `asusio lighting probe <device-id>` to validate the detected
`0b05:1cd2` Aura interface and its 63-byte Output payload with Report ID
`0x41`. The probe reads only the sysfs report descriptor and reports whether
the corresponding `/dev/hidraw*` node exists.

`asusio lighting raw-output <device-id> <126-hex-digits>` validates an
explicit 63-byte vendor payload without sending it. Add `--execute` to send
one 64-byte HID Output Report (Report ID followed by the payload) through the
detected hidraw node. This low-level command does not encode RGB colors or
effects. Use only a payload whose meaning has been verified for this exact
device. No hidraw number is fixed in the program.
