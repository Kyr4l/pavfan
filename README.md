# pavfan

> Repository: `fancontrol-hp-15-dk1xxx`

Fan control for the **HP Pavilion Gaming Laptop 15-dk1xxx** (board `8742`) on Linux.

This laptop has two independent fans (CPU on the right, GPU on the left) feeding one shared
heat-pipe assembly, and the BIOS fan curve ignores that coupling: the GPU fan speed affects CPU
temperatures, and the machine thermal-throttles far too easily. There is no official fan control
software for it on Linux or Windows, and the in-kernel `hp-wmi` hwmon driver misreports its fans
(0 RPM, always).

> ## ⚠️ Made with AI, tested on real hardware
>
> **This tool was written by an AI (Claude, by Anthropic), working under the direction of the
> repository owner.** It was **not** hand-written line by line.
>
> **It is tested on a real machine**: an actual HP Pavilion Gaming 15-dk1xxx (BIOS F.50), with the
> results measured on the hardware itself. Nothing here is "should work in theory". Anything that
> hasn't been verified on the real laptop is listed as unverified in the [status table](#status).
>
> **Hardware-specific warning:** this software writes to the laptop's Embedded Controller. The
> register map was reverse-engineered for *this exact model and BIOS*. On any other model (or even
> a different BIOS version) the same writes can do something else entirely. **Do not run it on
> other hardware.** The tool refuses to start unless the DMI product/board/BIOS identifiers match.
> Use at your own risk; GPL-3.0-or-later, no warranty (see [License](#license)).

## Components

| Component | Description | State |
|---|---|---|
| `pavfan` | CLI test unit: runs in the foreground as root and drives the fans directly. | usable, see below |
| `pavfand` | Main daemon, managed by systemd, sandboxed. | planned |
| `pavfan-plasmoid` | KDE Plasma tray applet. | planned |

## Try it

```
cargo build --release
./target/release/pavfan status --dry-run                       # no root, nothing touched
./target/release/pavfan run --dry-run --mode auto              # watch decisions on live temperatures
sudo ./target/release/pavfan status                            # read-only look at the real EC
sudo ./target/release/pavfan run --mode fixed --percent 50 --duration 20
sudo ./target/release/pavfan run --mode auto
sudo ./target/release/pavfan release                           # rescue: give the fans back to the BIOS
```

Ctrl-C, `SIGTERM`, `SIGHUP`, an error or a panic all hand the fans back to the BIOS. A
`SIGKILL`ed process cannot: run `pavfan release` (the future systemd unit does it in `ExecStopPost=`).

## Control modes

| Mode | Behaviour |
|---|---|
| **OFF** | Tool disabled; the BIOS controls the fans (stock behaviour). |
| **AUTO** (default when ON) | Built-in curve over `max(CPU, GPU + bias)`: reacts to GPU heat, which the BIOS ignores on Linux (measured), and ramps immediately instead of lagging. Hands back to the BIOS when cool. |
| **FIXED** | Both fans held at a fixed speed. |
| **MANUAL** | User-defined fan curve. |

## Measured effect

Numbers from [docs/thermal-measurements.md](docs/thermal-measurements.md), same load and protocol
for both rows, fans freshly cleaned:

| Load | BIOS | pavfan AUTO |
|---|---|---|
| GPU only (~50 W), CPU idle | CPU 73.9 C, GPU 62.6 C; the EC barely reacts (fan level 29) | CPU 70.9 C, GPU 59.1 C; fan level 32 |
| CPU + GPU together (synthetic worst case) | CPU reaches 85 C after 26 s, peaks at 92 C | CPU reaches 85 C after 63 s, ~3 C cooler at steady state |

What this does **not** do: under sustained full CPU + GPU load the machine still ends up near
90 C with both fans at maximum. That is the heatsink's limit, not the fan control's. The tool
fixes the BIOS ignoring GPU heat and its slow ramp; it does not make the laptop a different laptop.

## Safety design

The daemon runs as root and writes to the EC, so it is designed to fail towards "more cooling":

- EC writes go through a **whitelist** of known fan registers, with range clamping. No arbitrary
  register access.
- Refuses to start on a machine whose DMI identifiers don't match.
- Hands control back to the BIOS on exit, on `SIGTERM`/`SIGINT`, and on panic.
- A **watchdog** returns control to the BIOS if the control loop stalls or a temperature sensor
  can't be read.
- A **hard thermal override** forces the fans to maximum above a threshold, whatever the mode.
- The systemd unit is sandboxed (minimal capabilities and filesystem access).

## Status

Work in progress. The first phase is reverse-engineering the EC register map.

| Item | State |
|---|---|
| Identify EC fan registers | ✅ override target `0x14` + enable bit `0x0F`.3; see [docs/ec-register-map.md](docs/ec-register-map.md) |
| Read real fan RPM | ❌ no tachometer found; `0x11` only echoes the commanded level |
| Write fan level (both fans, shared setpoint) | ✅ verified on hardware |
| Independent CPU / GPU fan control | 🔧 not found yet: one setpoint drives both fans through different curves |
| Return control to BIOS cleanly | ✅ verified (restore of captured values) |
| CLI test unit (`pavfan`) | ✅ `status`, `run --mode fixed`, `run --mode auto`, `release` verified on the hardware; 17 controlled runs in the thermal sweeps, all handed back to the BIOS cleanly |
| systemd daemon | ⏳ not started |
| KDE Plasma applet | ⏳ not started |

Items move to ✅ only once verified on the real laptop.

## Repository layout

| Path | Contents |
|---|---|
| `crates/pavfan-core` | Library: EC access, the safety-critical controller, curves, governor, sensors. |
| `crates/pavfan` | The CLI. |
| `docs/` | Verified hardware findings: [EC register map](docs/ec-register-map.md), [thermal measurements](docs/thermal-measurements.md), [firmware notes](docs/firmware-notes.md). |
| `tools/` | Discovery helpers. `collect-ec-info.sh` and `ec-load-trace.sh` are **strictly read-only** (the `ec_sys` module is loaded *without* `write_support`). |
| `discovery/` | Raw local dumps (ACPI tables, EC snapshots). Git-ignored: firmware dumps stay out of the repo. |

## License

GPL-3.0-or-later. See [LICENSE](LICENSE). This program comes with ABSOLUTELY NO WARRANTY; it
writes to your laptop's embedded controller, so read the hardware warning at the top.
