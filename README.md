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

Early versions are read-only. Hardware writes are disabled
until backend compatibility and recovery behavior are verified.

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
