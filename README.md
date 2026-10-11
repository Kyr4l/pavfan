# pavfan

> Repository: `fancontrol-hp-15-dk1xxx`

Fan control for the **HP Pavilion Gaming Laptop 15-dk1xxx** (board `8742`) on Linux: a
sandboxed systemd daemon, a CLI, and a system-tray icon.

This laptop has two independent fans (CPU on the right, GPU on the left) feeding one shared
heat-pipe assembly. Its BIOS ignores GPU heat and ramps the fans late, so the machine
thermal-throttles far too easily. There is no official fan control software for it on Linux or
Windows, and the in-kernel `hp-wmi` driver reports 0 RPM because the firmware's WMI fan
commands are stubs (verified in the ACPI tables).

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

| Component | What it is |
|---|---|
| `pavfand` | System daemon (systemd, root, sandboxed). Owns the fans. D-Bus API on the system bus. |
| `pavfan` | CLI: `status`, `set <mode>` talk to the daemon (no root); `run`/`release` drive the EC directly for testing (root). |
| `pavfan-tray` | Tray icon (StatusNotifierItem: KDE Plasma natively, GNOME with AppIndicator, XFCE, LXQt, Waybar…). Shows mode, fan % and temperatures; switches modes; opt-in "start at login". |

## Install

Build the packages (needs `cargo`, plus `cargo-deb` / `cargo-generate-rpm` via
`cargo install cargo-deb cargo-generate-rpm`; neither `rpmbuild` nor `dpkg` is required):

```sh
make rpm        # -> target/pkg/pavfan-<version>.x86_64.rpm
make deb        # -> target/pkg/pavfan_<version>_amd64.deb
```

Then, as root: `zypper install ./target/pkg/pavfan-*.rpm` (openSUSE), `dnf install ./target/pkg/pavfan-*.rpm`
(Fedora) or `apt install ./target/pkg/pavfan_*.deb` (Debian/Ubuntu). Installing:

* loads `ec_sys` with `write_support=1` (and on every boot, via `modules-load.d`);
* installs the D-Bus policy and creates the `pavfan` group;
* enables and starts `pavfand.service` in AUTO mode.

`make install` / `make uninstall` do the same without a package manager (`DESTDIR=` supported).
Shell completions for bash, zsh and fish are included (`pavfan completions <shell>` prints them).

**Who may change the mode:** root and members of `wheel`, `sudo` or `pavfan`. Reading status is
open to everyone. Add yourself with `usermod -aG pavfan $USER` if you are in none of those.

**Tray:** launch "pavfan tray" from the application menu; left or right click opens its menu.
Tick *Start at login* there to create `~/.config/autostart/pavfan-tray.desktop` for your user
only. Nothing starts system-wide. The icon colour is the state: grey OFF, blue AUTO deferring to the BIOS, green
pavfan driving the fans, orange hard thermal override, red daemon unreachable.

## Use

```sh
pavfan status                 # daemon status, EC state (root), temperatures
pavfan set auto               # the default
pavfan set fixed 60           # both fans at 60 % of the EC range
pavfan set off                # hand the fans to the BIOS (daemon keeps running)
pavfan set manual             # your curve (see below)
journalctl -u pavfand -f      # what the daemon is doing
```

The chosen mode is remembered across restarts and reboots (`/var/lib/pavfan/`).

Configuration: `/etc/pavfan/config.toml` (defaults, thresholds; restart the daemon after edits)
and `/etc/pavfan/curve.toml` (the MANUAL curve; validate with `pavfan curve --file …`).
`SetManualCurve` over D-Bus stores a user curve that wins over the admin's file.

Stand-alone testing without the daemon (`systemctl stop pavfand` first; `pavfan run` refuses
while the daemon owns the fans):

```sh
pavfan run --dry-run --mode auto                      # in-memory EC, nothing written
sudo pavfan run --mode fixed --percent 50 --duration 20
sudo pavfan release                                   # rescue: give the fans back to the BIOS
```

## Windows

There is no Windows port (yet; see `docs/`). What exists is a corrected **NBFC profile** built
from these measurements, in [windows/nbfc/](windows/nbfc/): right EC limits, a clean hand-back
that preserves the BIOS flags, and a measured curve. NBFC only sees the CPU temperature, so the
GPU-heat fix that matters most on Linux cannot be reproduced there.

## Control modes

| Mode | Behaviour |
|---|---|
| **OFF** | The BIOS controls the fans (stock behaviour). |
| **AUTO** (default) | Built-in curve over `max(CPU, GPU + 8 °C)`: reacts to GPU heat, which the BIOS ignores on Linux (measured), and ramps immediately instead of lagging. Hands back to the BIOS below 62 °C. |
| **FIXED** | Both fans held at a fixed percentage of the EC range. |
| **MANUAL** | Your own temperature → speed curve, always engaged. |

In every mode but OFF, a **hard thermal override** forces maximum fans at 88 °C effective.

## Measured effect

Numbers from [docs/thermal-measurements.md](docs/thermal-measurements.md), same load and protocol
for both rows, fans freshly cleaned:

| Load | BIOS | pavfan AUTO |
|---|---|---|
| GPU only (~50 W), CPU idle | CPU 73.9 °C, GPU 62.6 °C; the EC barely reacts (fan level 29) | CPU 70.9 °C, GPU 59.1 °C; fan level 32 |
| CPU + GPU together (synthetic worst case) | CPU reaches 85 °C after 26 s, peaks at 92 °C | CPU reaches 85 °C after 63 s, ~3 °C cooler at steady state |

What this does **not** do: under sustained full CPU + GPU load the machine still ends up near
90 °C with both fans at maximum. That is the heatsink's limit, not the fan control's. The tool
fixes the BIOS ignoring GPU heat and its slow ramp; it does not make the laptop a different laptop.

A hardware limitation worth knowing: the EC exposes **one** fan setpoint for both fans (the GPU
fan follows it strongly, the CPU fan only near the top). Independent per-fan control was not
found; see [docs/ec-register-map.md](docs/ec-register-map.md) and
[docs/firmware-notes.md](docs/firmware-notes.md) for how far the reverse engineering went.

## Safety design

The daemon runs as root and writes to the EC, so it is designed to fail towards "BIOS control":

* EC writes go through a **whitelist**: only the fan target (`0x14`) and the override bit
  (`0x0F` bit 3), with every target clamped to the EC's own min/max. No arbitrary register access.
* Refuses to start on a machine whose DMI identifiers don't match (exit code 3, not restarted).
* Hands control back to the BIOS on exit, `SIGTERM`/`SIGINT`, on error and on panic (the
  release lives in a destructor, and the release build keeps unwinding on for that reason).
* `ExecStopPost=pavfan release` returns the fans to the BIOS even if the daemon was `SIGKILL`ed;
  on start the daemon also clears any stale override it finds.
* A failed temperature sensor means BIOS control until several consecutive good readings.
* **Hard thermal override** at 88 °C in every mode but OFF.
* systemd **watchdog** (15 s), and a hardened unit: no capabilities, read-only system, no module
  loading, no new privileges, syscall filter. `systemd-analyze security` rates it 2.1 ("OK").
* D-Bus methods validate everything and only queue requests; one thread owns the EC.
* The CLI and daemon share one control-loop implementation (`pavfan-core::runtime`), so what
  was tested in the foreground is what the service runs.

## Status

| Item | State |
|---|---|
| EC fan registers identified | ✅ override target `0x14` + enable bit `0x0F`.3; see [docs/ec-register-map.md](docs/ec-register-map.md) |
| Real fan RPM readout | ❌ no tachometer found; `0x11` only echoes the commanded level |
| Write fan level (both fans, shared setpoint) | ✅ verified on hardware |
| Independent CPU / GPU fan control | ❌ not available through the EC window; firmware leads documented and parked |
| Return control to BIOS cleanly | ✅ verified (17 controlled runs, all restored) |
| CLI `pavfan` | ✅ `status`, `run` (fixed/auto), `release` verified on hardware; AUTO measured against the BIOS |
| Daemon `pavfand` | 🔧 built, unit-tested, end-to-end tested on the session bus with the in-memory EC; **first run as a real service pending** |
| Tray `pavfan-tray` | 🔧 built; registers with Plasma's StatusNotifierWatcher in testing; **visual check pending** |
| RPM / DEB packages | 🔧 built from the same manifest; **installation test pending** |

Items move to ✅ only once verified on the real laptop.

## Repository layout

| Path | Contents |
|---|---|
| `crates/pavfan-core` | Library: EC access, the safety-critical controller, curves, governor, sensors, the shared control loop. |
| `crates/pavfan-ipc` | The D-Bus contract ([docs/ipc.md](docs/ipc.md)). |
| `crates/pavfand`, `crates/pavfan`, `crates/pavfan-tray` | Daemon, CLI, tray. |
| `packaging/` | systemd unit, D-Bus policy, module configs, default config and curve, desktop entry, icons, Debian maintainer scripts. Package manifests are in `crates/pavfan/Cargo.toml`. |
| `docs/` | Verified hardware findings: [EC register map](docs/ec-register-map.md), [thermal measurements](docs/thermal-measurements.md), [firmware notes](docs/firmware-notes.md), [D-Bus API](docs/ipc.md). |
| `tools/` | Discovery and measurement scripts. `collect-ec-info.sh` and `ec-load-trace.sh` are **strictly read-only**; `ec-*-test.sh` are the guarded write tests; `thermal-sweep.sh` produced the measurements. |
| `discovery/`, `bios/` | Raw local dumps (ACPI tables, EC snapshots, BIOS extraction). Git-ignored: they contain machine-specific identifiers. |

## License

GPL-3.0-or-later. See [LICENSE](LICENSE). This program comes with ABSOLUTELY NO WARRANTY; it
writes to your laptop's embedded controller, so read the hardware warning at the top.
