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

## Fan control

On the tested NCT6799 board, load the kernel's `nct6775` driver to expose fan
tachometers and PWM channels. To load it at every boot, put `nct6775` on its
own line in `/etc/modules-load.d/nct6775.conf`. `asusio fans list` shows
tachometers and PWM channels separately; `--json` includes `schema_version: 1`.
The PWM number has not been mapped to a physical fan header, so do not infer
that `fanN` belongs to `pwmN`.

`asusio fans set <pwm-number> <percent>` previews a 30–100% manual duty;
`--execute` applies it. `asusio fans auto <pwm-number>` previews restoration
of the observed Smart Fan IV firmware mode (`pwmN_enable=5`); add `--execute`
to apply it. Identify the physical channel before writing, especially for CPU
fans and pumps. Sysfs writes require appropriate permissions. BIOS Q-Fan
remains available for firmware-managed fan curves. The NCT6775 driver rejects
`pwmN` writes in automatic mode, so the manual command switches to
`pwmN_enable=1` before setting duty and checks both values afterward.
Rebuild the CLI after changing its source; keep shell comments on separate
lines when using an interactive zsh session.

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

`asusio lighting aura-static <device-id> <RRGGBB> --headers 0,1,2` previews
one vendor-derived static report per selected ARGB header. These header indexes
are unrelated to hwmon PWM numbers. Add `--execute` to re-query the advertised
header range and WDL state before writing. The command refuses enabled WDL
and sends command `05`, strip 0, Static effect 1, and GRB color bytes, as traced
in ASUS MB Manager's `MyAacArgbLedDevice::SetEffect`. It does not send WDL
ownership changes, S5 effects, or explicit persistent-save commands. Static ARGB color changes have been physically verified on the B850-A
controller. Other controllers, effects and persistence are not established.

`asusio lighting aura-config <device-id>` previews the configuration query
traced in the official ASUS MB Manager 1.00.11 HAL. `--execute` sends one
64-byte report beginning `41 03 00` and waits at most two seconds for a
64-byte `41 83` response. This is a protocol diagnostic, not a color command;
it does not send mode, color, or persistent-save commands. The command's
meaning is based on static analysis and still requires a device response
to validate on the local controller. Hidraw read/write permission is required.
The observed base reply advertises three ARGB configuration entries. Add
`--details --execute` to also query mainboard configuration (`08`), each
advertised ARGB entry (`03 01 index`, capped at five), and WDL state (`24`).
These are getter operations traced in the same HAL; no lighting changes are
requested. Each request has its own two-second response timeout.

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

## Native lighting backends

OpenRGB CLI integration has been removed. No OpenRGB process or server is needed.
`asusio lighting list [--json]` reads USB/HID metadata and descriptors to list
native interfaces; it does not test write permissions. HID IDs can change after
replugging, so use the current list. Existing discovery JSON remains unchanged.

```bash
cargo build -p asusio
./target/debug/asusio lighting list
./target/debug/asusio lighting keyboard-static <vendor-hid-id> FF0000
./target/debug/asusio lighting screen-static <usb-sysfs-id> FF0000
./target/debug/asusio lighting aura-board-static <aura-hid-id> FF0000 --locations 0,1
```

Each command previews by default. Add `--execute` to write using device access
permissions. `keyboard-static` supports only Scope II RX `0b05:1ab5`, with the
vendor `ff00` unnumbered 64-byte Input/Output interface. The hidraw write is 65
bytes including the required zero report-number prefix. It sends Static at
100% brightness without a persistent-save command. `screen-static` supports the
separate `0416:8050` LED USB device (60 LEDs, three bulk OUT packets to endpoint
1); it never accesses the LCD device `1cbe:a088`. The USB interface is claimed
temporarily and released on exit, with no driver detachment or reset.
`aura-board-static` uses onboard location IDs from `aura-config --details`,
which are distinct from ARGB headers and fan PWM channels. It validates the
advertised locations and WDL state before writing. Keyboard and onboard static color changes have been physically verified.
Screen lighting remains pending physical verification.

### ENE GPU and DRAM

```bash
./target/debug/asusio lighting i2c-list
./target/debug/asusio lighting ene-probe <bus> <hex-address>
sudo ./target/debug/asusio lighting ene-probe <bus> <hex-address> --execute
sudo ./target/debug/asusio lighting ene-static <bus> <hex-address> FF0000 --execute
```

Use an explicitly identified adapter and RGB address. RTX 3070 Strix has an ENE
candidate at `0x67` on its NVIDIA I2C adapters; DIMM RGB controllers use separate
addresses, commonly `0x70`–`0x77`. These are candidates, not confirmed local
addresses. SPD devices `0x51`/`0x53` are not RGB controllers and are rejected.
No automatic scan, `I2C_SLAVE_FORCE`, address remapping, Save (`AA`), or EEPROM
writes are implemented. Probe checks the ENE `A0..AF` signature before writing
register-selection pointers, then accepts only known firmware names and bounded
LED counts. Supported profiles are `DIMM_LED-0102`, `DIMM_LED-0103`,
`AUDA0-E6K5-0101`, and `AUMA0-E6K5-0107`. Unknown firmware is rejected.
Static control uses the firmware-specific effect buffer in RBG order, disables
Direct mode, selects Static and applies without Save. Adapter access uses
`/dev/i2c-N` and requires `i2c-dev`; adapters are not loaded or reconfigured by
this program. Local ENE probes identified the RTX 3070 at bus 1/address `0x67`
(`AUMA0-E6K5-0107`, 22 LEDs) and two DIMMs at bus 7/addresses `0x71`, `0x73`
(`AUDA0-E6K5-0101`, 8 LEDs each). Static red writes completed on all three;
physical color changes still require confirmation. Bus numbers may change after
reboot, so recheck adapter metadata and probe identity before use. DIMMs requiring initial address assignment remain unsupported.
An advisory adapter lock serializes asusio operations; it cannot coordinate with
unrelated applications that ignore that lock. A failed multi-packet operation
can leave some LEDs updated; commands stop on the first error without retry.

Protocol references (wire formats independently implemented in Rust):

- [Scope II keyboard](https://gitlab.com/CalcProgrammer1/OpenRGB/-/blob/next/Controllers/AsusAuraUSBController/AsusAuraTUFKeyboardController/AsusAuraTUFKeyboardController.cpp)
- [Universal Screen LED packets](https://github.com/CalcProgrammer1/OpenRGB/blob/master/Controllers/LianLiController/LianLiUniversalScreenController/LianLiUniversalScreenController.cpp)
- [ENE registers and firmware profiles](https://github.com/CalcProgrammer1/OpenRGB/blob/master/Controllers/ENESMBusController/ENESMBusController.cpp)
- [ENE SMBus transport](https://github.com/CalcProgrammer1/OpenRGB/blob/master/Controllers/ENESMBusController/ENESMBusInterface/ENESMBusInterface_i2c_smbus.cpp)

### Screen lighting while lianli-daemon owns USB

The local `lianli-daemon.service` owns the `0416:8050` LED USB node, so a second
USB claim returns `EBUSY`. Direct USB errors now include the operation and node.
Use the daemon's native Unix socket explicitly to keep its LCD session running:

```bash
./target/debug/asusio lighting screen-static 6-9.3 FF0000 --daemon-socket /run/user/1000/lianli-daemon.sock
./target/debug/asusio lighting screen-static 6-9.3 FF0000 --daemon-socket /run/user/1000/lianli-daemon.sock --execute
```

This option needs no OpenRGB and does not stop services or detach drivers. It
queries `GetDaemonInfo` and `GetRgbCapabilities`, verifies the exact device,
60-LED zone and Static support, then sends a version/instance-bound `Guarded`
`SetRgbEffect`. This adapter implements only daemon `1.2.4`, protocol `1`, with
`guarded_writes`; other versions are rejected rather than assumed compatible.
Responses are bounded to 1 MiB with 3-second read/write timeouts. A successful
reply acknowledges renderer acceptance, not physical completion. The local
daemon accepted this request; physical screen lighting still needs confirmation.
The daemon enforces its own service write gates. Raw USB remains the default
when no socket is specified; the program never silently switches transports.

IPC references: [request schema](https://github.com/sgtaziz/lian-li-linux/blob/main/crates/lianli-shared/src/ipc.rs),
[version and instance guards](https://github.com/sgtaziz/lian-li-linux/blob/main/crates/lianli-shared/src/daemon.rs).

### ENE color readback

`asusio lighting ene-state 1 67 --execute` reads Direct/Mode and the active
RGB buffer after firmware identification, without setting colors or applying
changes. ENE pointer-selection writes are required for these reads. JSON uses
`schema_version: 1`; `colors_rgb` decodes the wire RBG order. Register intensities
are requested values, not measured optical output. On the tested RTX 3070,
Static white readback was 255/255/255 for all 22 LEDs despite a yellow tint;
RGBW is not established by that observation. No automatic color correction is
applied.

## Named lighting control and local calibration

The high-level `lighting set` command applies a host-local configuration, so
hardware IDs and RGB compensation do not have to be repeated in every command.
The configured aliases on the tested machine are `fans` (fan LEDs), `board`,
`keyboard`, `gpu`, `ram-1`, `ram-2`, and `screen`. These aliases control lighting;
fan speed still uses `fans set` / `fans auto`.

```bash
./target/debug/asusio lighting targets
./target/debug/asusio lighting set all 80C0FF
./target/debug/asusio lighting set gpu FFFFFF
./target/debug/asusio lighting set all 80C0FF --brightness 50
```

These commands preview only. To apply all devices, including Aura nodes that
need elevated permissions, select the user's configuration explicitly:

```bash
sudo ./target/debug/asusio lighting --config "$HOME/.config/asusio/lighting.json" set all 80C0FF --execute
```

Under sudo, the program requires `--config` and never silently loads root's
configuration. Screen control still uses the explicitly configured native
lianli-daemon socket. Device execution errors are reported individually, other
targets continue, and any failure makes the command exit unsuccessfully. This
is not an atomic operation across devices; earlier updates cannot be undone
automatically. All selected bindings are resolved before color writes start.

Calibration is stored in `$XDG_CONFIG_HOME/asusio/lighting.json`, or
`$HOME/.config/asusio/lighting.json` when XDG_CONFIG_HOME is unset. An explicit
`lighting --config PATH` overrides this location. The file has mode 0600 and
is replaced atomically with file/directory synchronization. It is outside the
repository and is not written into firmware. A machine-id fingerprint binds it
to this host; copying the file to another machine causes loading to fail. This
fingerprint is an accidental-misuse check, not a security or licensing boundary.

The GPU's visually selected white balance is `[48,48,255]` (`3030FF`); other
targets use neutral `[255,255,255]`. Each channel is calculated as
`round(input * gain / 255 * brightness / 100)`. Thus requested white `FFFFFF`
becomes `3030FF` only on the GPU, and requested light blue `80C0FF` becomes
`1824FF` on the GPU. Pure blue stays blue. This is an empirical channel-gain
model, not measured colorimetric calibration, and may need further tuning for
other colors. Brightness ranges from 0 to 100 and can only attenuate output;
it cannot make an already saturated blue channel brighter.

Change or reset a target's gains as the normal user (no hardware color write):

```bash
./target/debug/asusio lighting calibrate gpu 3030FF
./target/debug/asusio lighting calibrate gpu FFFFFF
./target/debug/asusio lighting set gpu FFFFFF --uncalibrated
```

The second command disables compensation; the third bypasses it for that
invocation only. Existing low-level diagnostics such as `ene-static` remain
raw and do not apply local compensation. Use `lighting set` for calibrated
control. Calibration persists across rebuilds/reboots and is loaded on each
managed command. For automatic color restoration at boot, see the `asusiod`
system service below.

For initial installation on another machine, prepare a local schema-1 manifest
with `targets` and no `host_fingerprint`, then run
`lighting setup --from /path/to/local-manifest.json`. Setup validates and resolves
bindings, stamps the host fingerprint, and refuses to overwrite an existing
configuration. `targets --json` shows the complete manifest format. HID bindings
use VID/PID, physical USB interface identity and descriptor validation, rather
than transient HID instance numbers. I2C bindings use adapter name and parent
sysfs path, plus RGB address, expected ENE firmware and LED count. The current
bus number is resolved on each invocation; the firmware and count are checked
on the same locked fd before color writes. No bus scan or DIMM address remapping
is introduced. Changing ports, hardware, firmware or topology may require an
explicit configuration update. A moved DIMM address with the same firmware and
LED count cannot be distinguished by this protocol alone.

### Sync and individual lighting modes

asusiod keeps two independent lighting settings. **Individual** mode (the default,
and the behavior of `lighting set`) gives each target its own color. **Sync** mode
applies one color, brightness and effect to every configured target. Switching never
overwrites the other mode's settings, and boot restore uses the current mode.

```bash
asusio lighting sync 80C0FF --brightness 70                  # preview
asusio lighting sync 80C0FF --brightness 70 --execute        # every following target
asusio lighting mode individual --execute                    # back to per-target colors
asusio lighting set gpu FF0000 --execute                     # one target (see below)
asusio lighting source gpu custom --execute                  # gpu keeps its own color
asusio lighting source gpu sync --execute                    # gpu follows Sync again
asusio lighting status [--json]                              # per-target state and source
```

Within Sync mode each target either follows the Sync setting or keeps its own
(`custom`), like Armoury Crate's per-device choice. `lighting source` switches one
target; a target turning custom without a setting of its own starts from the Sync
setting, so nothing changes visibly. `lighting set` in Sync mode makes that target
custom and leaves the others following Sync; in individual mode it behaves as before.
`lighting sync` writes only the targets that follow it. From individual mode,
`lighting sync` and `lighting source` switch to Sync and every target with an individual
setting stays custom (nothing it shows changes); the others follow. `lighting mode` switches the whole mode and puts every target on the same side.
Status reports each target's `source`.

`sync`, `mode` and `status` need asusiod. Per-target calibration still applies in
Sync mode unless `--uncalibrated` is given. When individual mode is entered from
Sync, targets that have no individual setting yet keep the Sync setting as their
individual one, so nothing changes visibly and every target is restored at boot.
Existing individual settings are never overwritten. Individual settings belong to
one `lighting.json`; using another configuration drops them, Sync settings stay.

Each target is planned separately: a target that cannot be resolved (for example the
screen before lianli-daemon starts) is reported with its error, and the others are
still applied. Every target is resolved before the first write.

`--effect` selects a controller built-in effect. Aura controllers (onboard LEDs and ARGB
headers) take the effects of the table in ASUS's own HAL through the same `41 05` report as
Static, with another effect ID. Confirmed on the B850-A: `static`, `breathing`,
`strobing`, `color_cycle`, `rainbow`, `breath_rainbow`, `starry_night`. Rainbow comet,
comet, moving flash, flash and dash, wave and glowing yoyo did nothing with that report
alone (they probably need the speed/parameter reports, not implemented) and are not
offered. Effects run at the controller's default pace.

ENE GPU and DRAM controllers take the same seven effects as modes of register `0x8021`
(1 static, 2 breathing, 3 flashing, 4 spectrum cycle, 5 rainbow, 6 spectrum cycle
breathing, 13 random flicker for starry night); only the mode value differs from the
Static write. The Lian Li screen gets five from lianli-daemon's software modes: static,
breathing, color cycle (`RainbowMorph`), rainbow and starry night (`Twinkle`). These are
`experimental` until confirmed on the hardware. The keyboard only has `static`.
`lighting status` lists each target's effects.

A Sync setting with an effect a following target lacks is shown there as a static color
(same color and brightness), so an animated Sync setting works across mixed devices. A
target's own setting (`lighting set`) must be an effect it supports; otherwise the request
is refused.

`lighting status --json` (IPC `get_lighting`) returns `mode`, `sync`,
`lighting_config`, `recorded` (whether asusiod restores this configuration) and
`targets`. Each target has `alias`, `kind` (`board`, `argb`, `keyboard`, `graphics`,
`memory`, `screen` or `other`), `backend`, `white_balance`, `effects` (`effect`,
`experimental`), `individual`, `applied` (what the current mode shows), `availability`
(`state`: `available`, `not_ready` or `unavailable`, with a `reason`) and
`last_error`. Readiness only checks sysfs bindings and, for the screen, that the
lianli-daemon socket exists; results are cached for 10 seconds and refreshed after
each write. ENE targets are classified by firmware: `AUMA0-E6K5-0107` as graphics,
`AUDA0-*` and `DIMM_LED-*` as memory, matching the local identification above.

## CPU performance profiles

On `amd-pstate-epp` in active mode (tested: Ryzen 5 7500F, kernel 7.2), asusiod can
switch the governor, energy-performance preference (EPP) and boost of every online
CPU, plus an optional fan-curve preset:

| Profile | governor | EPP | boost | fans |
|---|---|---|---|---|
| `silent` | `powersave` | `power` | off | preset `silent`, if defined |
| `balanced` | `powersave` | `balance_performance` | on | default curves |
| `performance` | `powersave` | `performance` | on | preset `performance`, if defined |
| `custom` | any allowed | any allowed | on/off | any defined preset, or default |

The `performance` governor (minimum frequency raised to nominal) is only available
through `custom`; built-in profiles keep `powersave`, where amd-pstate-epp still
scales up to full speed under load.

```bash
asusio profile list                    # profiles, recorded selection, fan presets
asusio profile status                  # Tctl, fan RPM, PWM state, governor/EPP/boost
asusio profile set silent              # preview
asusio profile set silent --execute    # apply through asusiod; restored at boot
asusio profile set custom --epp balance_power --boost off --fan-preset silent --execute
```

All profile commands accept `--json`. `set` always goes through asusiod; `status`
falls back to local read-only readings (without profile and power) when the daemon
is not running. Custom flags are merged over the stored custom profile; switching to
a built-in profile keeps the custom settings for later.

**Safety.** Only `performance`/`powersave` and the EPP names `default`,
`performance`, `balance_performance`, `balance_power`, `power` are accepted, and each
must appear in every policy's `*_available_*` list. The kernel's `custom` EPP entry
and raw numbers are not exposed. The performance governor only permits EPP
`performance` (the driver returns `EBUSY` otherwise), so other combinations are
rejected before writing. Writes go boost, then per policy governor then EPP; each is
read back. On any failure the observed previous values are restored (governor before
EPP, because a governor switch changes the effective EPP) and the error reports
whether the restore succeeded. Other cpufreq drivers or `amd_pstate` modes are
reported as unsupported; nothing is written. Without a recorded profile, the daemon
leaves CPU settings untouched at boot. A failed CPU write leaves fans and the
recorded profile unchanged.

**Fan presets** are named curve sets, from two places: the root-owned
`/etc/asusio/asusiod.json` (below) and presets saved through asusiod (next section),
which replace configuration presets of the same name. A profile whose preset is not defined uses the
default `fan_curves`, which are empty by default, so fans stay under firmware
control; `profile list` and the status `fan` object say so explicitly (`firmware:
true` and a `note`). Switching presets first returns every running channel to
firmware, then starts the new set. On stop or crash, `--release-fans` releases every
channel of the default curves and all presets.

```json
"fan_presets": {
  "silent": [{"channel": 2, "sensor": {"chip": "k10temp", "label": "Tctl"},
              "points": [{"celsius": 50, "percent": 30}, {"celsius": 85, "percent": 100}]}]
}
```

**Telemetry** (`profile status --json`, IPC `telemetry`) is read-only:

| Field | Content |
|---|---|
| `mode`, `in_sync` | Recorded profile; `in_sync` is false when the CPU was changed outside asusio |
| `cpu` | `supported`, `reason`, `governor`, `epp`, `boost` (`null`: mixed or unknown) |
| `tctl_celsius`, `ccd_celsius` | k10temp `Tctl` and `Tccd1`, `Tccd2`, ... |
| `cpu_usage` | `total_percent` and `cores_percent` from `/proc/stat` deltas |
| `cpu_frequency` | `average_mhz`, `max_mhz` over online policies (`cpuinfo_avg_freq`) |
| `package_watts`, `core_watts` | powercap `package-0` and `core` zones (root only) |
| `memory` | `used_bytes` (`MemTotal - MemAvailable`), `total_bytes` |
| `voltages` | Board voltages named and scaled by the board profile, in volts |
| `fans` | Connected headers: `channel`, `header`, `label`, `kind` (`fan`, `pump`, `unknown`), `rpm`, `pwm` (`channel`, `percent`, raw `enable`, `control`: `curve`, `manual` or `firmware`) |
| `unused_headers` | Headers that have never reported a speed |
| `gpus` | NVIDIA GPUs through NVML: temperature, load, memory-controller load, power and limit, graphics/memory clocks, VRAM, P-state, PCIe link, and per fan `percent`, `target_percent`, `rpm` |
| `gpu_error`, `scheduler`, `board`, `fan`, `errors` | Why GPU data is missing; sched_ext scheduler; matched board; selected fan curves; failed readings |

A header counts as connected once it has reported a speed; asusiod remembers that
for its lifetime, so a fan that stops at low temperature stays listed. Without the
daemon, `profile status` samples once over 300 ms and shows only headers that are
spinning. Power comes from root-only energy counters, averaged over at least one
second and rounded to 0.1 W, so it is only available through asusiod.

GPU readings use NVML (`libnvidia-ml.so.1`), loaded at runtime; without the NVIDIA
driver `gpus` is empty and `gpu_error` says why. Only query functions are resolved;
no fan, clock or power control. NVML has no public core-voltage query, so GPU voltage
is not reported. Fans of a graphics card are never among the motherboard headers.

**Board profiles** in `hardware/boards/` are embedded at build time and match the DMI
vendor and board name exactly. They name each Super I/O fan input after its header,
mark pumps, and name and scale voltage inputs (external divider factors; the kernel
already applies the chip's internal ones). The ROG STRIX B850-A GAMING WIFI7 S NEO
profile follows LibreHardwareMonitor's table for the B850-A; its fan header order was
confirmed by the board's owner, voltages were checked for plausibility only. Daisy-
chained fans report one speed per header.

**Pumps never run on curves.** When the board profile marks a header as a pump,
asusiod refuses to start if `fan_curves` or any `fan_presets` entry uses its PWM
channel; the pump stays under firmware control (the B850-A runs AIO_PUMP at full
speed by default).

## System service: boot restore and fan curves

`asusiod` is a root system service. It reapplies your last lighting at boot,
runs optional fan curves, and lets locally logged-in users control lighting
without sudo. On Arch Linux, installation takes two commands:

```bash
cd packaging/arch && makepkg -si           # build, test, install and start asusiod
asusio lighting set all 80C0FF --execute   # as your user; restored at every boot
```

The package installs to `/usr/bin` and loads `i2c-dev` and `nct6775` at boot
(and immediately). It enables and starts `asusiod.service`. `pacman -R asusio`
stops and removes it, but keeps `/var/lib/asusio`. Without pacman, run
`cargo build --release --workspace`, then
`sudo ./target/release/asusio-helper install --execute`. The helper does the
same into `/usr/local/bin`. Without `--execute`, it only previews.

**How it works.** While asusiod is running, `lighting set --execute` goes through
its socket, `/run/asusio/asusiod.sock`. The output is labeled "via asusiod",
and no sudo is needed. The daemon records the request in
`/var/lib/asusio/state.json` with the path of your `lighting.json`. The
recorded fields are the requested color, brightness and calibration flag, so a
later `lighting calibrate` applies on the next restore. The configuration must
be a regular file owned by the requesting user, so the root daemon never reads
files on another user's behalf. `--direct` writes hardware without the daemon,
and that write is not recorded. At boot, targets that are not ready are retried
every 10 seconds for 10 minutes. The screen needs this, because its
lianli-daemon socket appears only after login. `asusio daemon status` shows
progress, and `asusio daemon restore` reapplies the recorded lighting now.

`/etc/asusio/asusiod.json` is optional; a missing file means defaults:

```json
{"allow_active_sessions": true, "client_group": "asusio", "restore_lighting": true, "fan_curves": [], "fan_presets": {}}
```

**Fan curves** in the root-owned `/etc/asusio/asusiod.json` are the administrator's
defaults and are not restricted beyond the validation below. The list is empty by
default. Example:

```json
"fan_curves": [{
  "channel": 2,
  "sensor": {"chip": "k10temp", "label": "Tctl"},
  "points": [{"celsius": 40, "percent": 30}, {"celsius": 60, "percent": 50},
             {"celsius": 80, "percent": 100}],
  "hysteresis_celsius": 3
}]
```

Curves must have at least two points with increasing temperature and
non-decreasing duty from 30% to 100%, and must end at 100%. Duty rises
immediately; it falls only after the temperature drops by the hysteresis. A
missing, ambiguous or implausible sensor reading, a failed PWM write, a daemon
stop, or a crash returns the channel to firmware control: `ExecStopPost` runs
`asusiod --release-fans`, and panics abort the process. A released curve stays
off until the daemon restarts. Restart the service after editing the file.

**Access control.** Each request is checked by `asusio-policy` against
`SO_PEERCRED` credentials. Root, the daemon's user, members of `client_group`, and
users with an active local logind session (like udev `uaccess`) are allowed. On
a shared machine, set `"allow_active_sessions": false`; the socket then becomes
`0660 root:asusio`, and only group members are allowed. Previews and status do
not write hardware. The IPC protocol (`asusio_core::service::ipc`) is
newline-delimited JSON with one request per connection. Methods: `status`,
`set_lighting`, `set_sync`, `set_lighting_mode`, `get_lighting`, `restore_lighting`,
`get_profiles`, `set_profile`, `telemetry`, `get_fans`, `set_fan_preset`, `delete_fan_preset`,
`detect_fans`, `set_fan_role` and `reset_fans`.
Setting a profile (`set_profile` with `execute`) is a hardware write and is
authorized like lighting writes.

### Saving fan curves without authentication

Any user the policy allows (root, the daemon's user, the client group, or an active local
session, like lighting and CPU profiles) can save fan presets through asusiod. There is
no password step: saving a preset that the active profile uses changes the running
curves within one fan interval (2 seconds by default), and the preset is stored in the
daemon's state file for the next boot.

```bash
asusio fans presets                                       # channels, presets, limits
asusio fans preset set silent --curve 2=40:30,60:45,80:100 --curve 4=45:30,85:100
asusio fans preset set silent --curve 2=40:30,60:45,80:100 --execute
asusio fans preset delete silent --execute
```

`--curve` is `PWM=TEMP:PERCENT,...` for the CPU temperature. Names are 1-32 characters of
`a-z`, `0-9`, `-` and `_`. Each built-in profile selects the preset of
its own name (`silent`, `balanced`, `performance`); while that preset is not defined the
fans stay under firmware control. `custom` can select any name
(`asusio profile set custom --fan-preset NAME`). IPC methods: `get_fans`, `set_fan_preset`, `delete_fan_preset`.

Because the socket is open to local sessions, curves that arrive over it are held to
stricter rules than the root configuration, so a bad or hostile curve cannot starve the
machine of cooling:

| Rule | Why |
|---|---|
| Follows `k10temp/Tctl` only | Another sensor could stay cold (an unconnected input reads a constant low value) and pin a fan at its minimum |
| Duty 30-100%, at least two points, at most 16, ending at 100% | The existing curve rules, plus a size limit |
| Reaches 100% by 85 °C | The existing rule only required the last point to be 100%, which could sit at 150 °C |
| Only PWM outputs of fan headers named by the board profile | Pumps are excluded and an unknown board gets no editing at all, because pumps could not be told apart |
| At most 16 saved presets | Bounds the state file |
| One curve per PWM output | `pwm2` drives CPU_OPT and CHA_FAN3 together, so a curve on it moves both |

**Over-temperature guard.** Independently of any curve, asusiod reads the CPU temperature
every interval. At 90 °C or above, or when it cannot be read, every curve channel is
handed back to firmware; the curves resume once the temperature is at or below 80 °C.
`fans presets` and the IPC `get_fans` report `thermal_hold`. A saved curve cannot turn
the guard off.

A rejected preset changes nothing. `--release-fans` (run by `ExecStopPost` after any exit
or crash) releases the outputs of the configuration, of every saved preset and of every
output asusiod ever took over, each to the firmware mode recorded for it, so a crash cannot
leave a fan at a manual duty. Outputs that were never touched are left alone.

### Any machine: detection, roles, permanence and reset

Nothing above depends on a particular board. Fan outputs and inputs are read from the fan
chip's hwmon driver (Nuvoton `nct6*` or ITE `it8*`; only `nct6799` has been checked on real
hardware, so `fans presets` flags other chips). A board profile in `hardware/boards/` only
adds names, pump marks and voltages when the DMI board name matches exactly.

**Detection** finds which fan inputs follow which PWM output (including outputs shared by
two headers) without a profile. `asusio fans detect` previews it; `--execute` starts it in
asusiod. Every output that is not a pump is raised in turn (never lowered), watched for
12 seconds and restored, about 25 seconds per output. It skips outputs the firmware already
runs near full speed (a pump is often run that way) and outputs another program controls,
stops and restores at 85 °C or when the temperature cannot be read, and the fan loop hands
its curves back while it runs. The result is saved and shown in `fans presets` and the
telemetry (`shares_pwm_with`, the `pwm` of each header).

**Roles.** An output can only be given a curve once it is known to be a fan. The board
profile says so for a known board (and cannot be overridden). On any other machine the user
confirms it: `asusio fans role 4 fan`, `asusio fans role 1 pump`, `asusio fans role 4 none`.
Detection only suggests (a fast input, or an output already at full speed, suggests a pump).
Nothing assumes "fan" on its own, because a curve that slows a pump to 30% could overheat the
CPU. An output without a role is refused, and so is a pump.

**Firmware modes.** Before an output is first taken over, asusiod records its `pwmN_enable`
and restores exactly that on release, instead of a fixed value. Only `nct6799` has a known
default; any other chip whose mode was not recorded is refused rather than guessed.

**Other hardware.** The state file carries a fingerprint of the board and fan chip. If it
turns up on a different machine, the recorded roles, detection result and firmware modes are
dropped at start (they could be wrong there); saved presets are kept. They are not applied
(`fans presets` says why) until their outputs are confirmed as fans on the new machine.

**Permanence.** Saved presets live in `/var/lib/asusio/state.json`. They never expire, are
not touched by detection, profile changes, upgrades or `pacman -R` (which keeps the
directory), and come back at every boot.

**Reset to the system defaults.** `asusio fans reset [--everything] [--execute]` deletes every
saved preset, clears a custom profile's reference to one, and hands the fans back to firmware
control at once. `--everything` also forgets the detection result and confirmed roles. The
configuration file and board profiles are not touched.

```bash
asusio fans detect --execute      # background; follow it in `asusio fans presets`
asusio fans role 4 fan            # confirm what is connected (unknown boards)
asusio fans reset --execute       # back to firmware control, saved presets deleted
asusiod --no-board-profile        # diagnostic: behave as on an unknown board
```

## Library architecture and integration

Dependencies flow from applications to `asusio-core`, then native backends and
read-only discovery, with shared data in `asusio-types`. The core owns reusable
lighting orchestration; applications own argument parsing and presentation.

| Module | Responsibility |
|---|---|
| `apps/asusio/src/cli.rs` | Clap command schema; colors, addresses and payloads are parsed here |
| `apps/asusio/src/commands/` | Presentation per area: `system`, `fans`, `profile`, `lighting` (named), `diagnostics` (raw) |
| `apps/asusio/src/output.rs` | Shared JSON envelope, warnings, hex formatting and value parsers |
| `asusio_core::lighting` | `Store`, `Request`, `plan`/`execute`, `discover_interfaces`, color helpers |
| `asusio_core::lighting::config` | Configuration schema, host validation, atomic storage |
| `asusio_core::lighting::info` | Target kind, supported effects and readiness for front ends |
| `lighting::runtime` (internal) | Resolve native device identities and dispatch backends |
| `asusio-backends` | Device protocols and OS transport; each exposes `DEVICE` (USB ID) and `inspect` |
| `asusio-discovery` | Read-only sysfs scans: `hid::discover`, `usb::discover`, …; default roots in `sysfs` |
| `asusio-types` | Shared data: `Rgb`, `UsbId`, device records, `DiscoveryReport`, `Capability` |
| `asusio_core::service` | Daemon configuration, recorded state and IPC protocol (`ipc::call`) |
| `asusio_core::fan_curve` | Curve validation, interpolation/hysteresis controller, hwmon/nct6799 adapters |
| `asusio_core::cpu_profile` | Built-in and custom CPU profiles, fan-preset resolution |
| `asusio_core::telemetry` | Read-only live readings: CPU, memory, voltages, fan headers, GPUs, sched_ext |
| `asusio_core::board` | Embedded board profiles: fan headers, pumps, voltage names and scaling |
| `asusio_core::fan_map` | Fan knowledge of the machine: detection results, roles, hardware fingerprint |
| `asusio_core::fan_probe` | Detection of which fan inputs follow which output, over an abstract chip |
| `asusio_backends::nvidia_nvml` | Read-only NVIDIA GPU readings through NVML, loaded at runtime |
| `asusio_backends::cpu_pstate` | Verified amd-pstate-epp governor/EPP/boost writes with rollback |
| `asusio-hal` | `TemperatureSensor` / `PwmOutput` traits used by control loops |
| `asusio-policy` | Per-request authorization of daemon clients |
| `apps/asusiod` | System service: `daemon` (state), `lighting`, `profile`, `fans`, `server` |
| `apps/asusio-helper` | Previewable installer for the unit, configuration and module loading |

A Rust application can use the same calibration and identity validation without
shelling out to the CLI:

```rust,no_run
use asusio_core::lighting::{self, Request, Store};

fn control() -> std::io::Result<()> {
    let store = Store::open(None)?; // or Some(path) for an explicit configuration
    let request = Request::new(lighting::ALL, [128, 192, 255]).brightness(100);
    let plan = store.plan(&request)?;
    // Present plan[i].alias() and plan[i].output() to the application's user.
    // Only execute after the application has authorized the hardware operation.
    for outcome in lighting::execute(plan) {
        outcome.result?;
    }
    Ok(())
}
```

`Request::new(target, rgb)` defaults to full brightness with stored calibration;
`.brightness(percent)` and `.calibrated(false)` adjust it. `plan` (or
`Store::plan`) validates the host/configuration, target selector and brightness
range, then resolves metadata without changing colors. A `PlannedTarget` exposes its
alias and calibrated output through read-only accessors; execution consumes the
plan. `execute` returns per-device `Outcome` values with `io::Result<()>` and
never prints, retries or rolls back. It processes every target before returning
results, so applications should inspect all outcomes when reporting partial
success. Resolution is a snapshot, not a reservation; device removal or access
changes between planning and execution may still cause execution errors.
`lighting::discover_interfaces()` lists validated native interfaces with their
`InterfaceKind`, without opening hardware nodes.

The compilable `asusio-core/examples/lighting_preview.rs` demonstrates a complete
read-only client. Run `cargo run -p asusio-core --example lighting_preview`.

To add a backend, implement its validated transport in `asusio-backends`
(export a `DEVICE` ID and an `inspect` function), add a configuration variant in
`lighting::Device`, and extend the runtime resolution/dispatch matches (plus
`InterfaceKind` if it should appear in `lighting list`). The common plan,
calibration and error collection then apply to it. Preserve existing serialized
variant names and schema-1 fields. Hardware-specific settings stay in the
host-local configuration.

### Shared configuration transactions

Library clients use `asusio_core::lighting::Store` for configuration
changes. `Store::install` validates and binds a manifest to the current host,
resolves targets, and refuses to replace an existing configuration.
`Store::calibrate` locks, reloads, changes one target and atomically saves it.
The CLI calls these same methods. Direct persistence and native runtime dispatch
are internal implementation details; use `plan`/`execute` for calibrated control.

```rust,no_run
use asusio_core::lighting::Store;
fn save_calibration() -> std::io::Result<()> {
    Store::open(None)?.calibrate("gpu", [48, 48, 255])
}
```

Writers coordinate through a persistent `lighting.json.lock` file (0600).
Do not delete the lock while clients are running. Readers see complete old or
new files through atomic replacement. Separate targets updated concurrently
are preserved because each transaction reloads after acquiring the lock.
Temporary files use process and operation IDs to avoid collisions between
multiple stores in one process. Manual editors and older clients that do not
use the lock are outside this coordination. Configuration changes through the
Store must run as the normal user; elevated hardware execution remains separate.
