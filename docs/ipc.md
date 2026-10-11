# D-Bus interface

`pavfand` serves `org.pavfan.Daemon1` at `/org/pavfan/Daemon1` on the **system bus**.
Reading is open to every local user; changing the mode is limited by the bus policy
(`packaging/dbus/org.pavfan.Daemon1.conf`) to root and members of `wheel`, `sudo` or `pavfan`.

The Rust definition (proxy and `Status` struct) lives in `crates/pavfan-ipc`; the CLI and the
tray are plain clients of it.

## Methods

| Method | Signature | Notes |
|---|---|---|
| `GetStatus()` | `→ (ssybyyyybbddbdsss)` | See the `Status` fields below. Refreshed every control tick. |
| `SetMode(mode)` | `s →` | `"off"`, `"auto"`, `"fixed"` or `"manual"`. `"manual"` fails unless a curve is configured. |
| `SetFixedPercent(percent)` | `y →` | 0–100 % of the EC's fan range; used by FIXED mode. |
| `SetAutoProfile(profile)` | `s →` | `"quiet"` or `"early"`; the AUTO behaviour, remembered like the mode. |
| `SetManualCurve(points)` | `a(dy) →` | `(temperature C, percent)` points. Validated like a curve file: temperatures strictly increasing, cooling never decreasing, 2+ points. |
| `GetManualCurve()` | `→ a(dy)` | The current MANUAL curve (empty if none). |

Mode changes are **queued** and applied by the control loop at its next tick (≤ 1 s by
default): methods never touch the EC themselves. Call `GetStatus` afterwards to see the result.

## Signals and properties

| | Type | Notes |
|---|---|---|
| `ModeChanged(mode)` | signal `s` | Emitted when the effective mode changes. |
| `Version` | property `s` | Daemon version. |

## `Status` fields (in order)

| Field | Type | Meaning |
|---|---|---|
| `mode` | s | off / auto / fixed / manual |
| `auto_profile` | s | quiet / early |
| `fixed_percent` | y | FIXED target, percent |
| `has_manual_curve` | b | a MANUAL curve is configured |
| `level` | y | fan level the EC is running, units of 100 RPM (ours when `engaged`, the BIOS's otherwise) |
| `percent` | y | `level` as a percentage of the EC range |
| `min_level`, `max_level` | y, y | the EC's own range (22..42 on this machine) |
| `engaged` | b | pavfan is driving the fans; false = BIOS control |
| `hard_override` | b | hard thermal override active (maximum fans) |
| `cpu_c` | d | CPU package temperature; negative if unavailable |
| `gpu_c` | d | GPU temperature; negative if unavailable or asleep |
| `gpu_present` | b | an NVIDIA GPU sensor exists |
| `effective_c` | d | `max(CPU, GPU + bias)`, the input to the decisions |
| `health` | s | `ok`, `sensor-failure` (fans handed to the BIOS), `recovering` |
| `detail` | s | human-readable detail or latest warning; may be empty |
| `version` | s | daemon version |

## Examples

```sh
busctl introspect org.pavfan.Daemon1 /org/pavfan/Daemon1
busctl call org.pavfan.Daemon1 /org/pavfan/Daemon1 org.pavfan.Daemon1 GetStatus
busctl call org.pavfan.Daemon1 /org/pavfan/Daemon1 org.pavfan.Daemon1 SetMode s fixed
busctl call org.pavfan.Daemon1 /org/pavfan/Daemon1 org.pavfan.Daemon1 SetAutoProfile s early
busctl call org.pavfan.Daemon1 /org/pavfan/Daemon1 org.pavfan.Daemon1 SetManualCurve 'a(dy)' 3 55 20 70 50 85 100
```

The same through the CLI: `pavfan status`, `pavfan set fixed 75`, `pavfan set auto early`, `pavfan set manual`.

For development, `pavfand --dry-run --session-bus` serves the same interface on the session bus
against an in-memory EC; `pavfan --session-bus ...` and `pavfan-tray --session-bus` talk to it.
