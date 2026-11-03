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
programmatic access. JSON is an array of devices with `sysfs_path`,
`device_id`, `name`, `display_name`, and `sensors`. Each sensor has a `label`
and a tagged `value`: `kind: "temperature"` with `milli_celsius`, or
`kind: "fan_speed_rpm"` with `rpm`. Devices without readable sensors are
omitted.
