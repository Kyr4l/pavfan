# EC register map: HP Pavilion Gaming Laptop 15-dk1xxx (board 8742, BIOS F.50)

Everything here was measured on the real machine. "Verified" means observed with a
read/write test on the hardware; "Unverified" means inferred (DSDT field names, NBFC
profiles) but not yet confirmed. Offsets are in the 256-byte EC space reachable through
ports `0x62`/`0x66` (Linux: `ec_sys`, debugfs).

Field names come from the firmware's own ACPI table (`ECMM` OperationRegion); the
meanings are inferred from names and tests.

## Verified

| Offset | DSDT name | Meaning | Evidence |
|---|---|---|---|
| `0x11` | `FRPM` | Current fan **level**, units of 100 RPM. Follows the BIOS curve in auto mode; **echoes `0x14` immediately** in override mode (no ramp), so it is a commanded level, not a tachometer. | trace under CPU/GPU load; override tests |
| `0x12` | `FNMX` | Maximum level (42 → "4200 RPM") | read |
| `0x13` | `FNMN` | Minimum level (22 → "2200 RPM") | read |
| `0x14` | `FWPM` | Override target level. `0xFF` = unset. Honoured only while `0x0F` bit 3 is set. Accepted range 22..42. | write + read-back; audible effect on both fans |
| `0x0F` bit 3 | `FNSW` | Override enable. Idle value of the byte is `0x01`. | write + read-back |

Observed behaviour of the override (user listening, one setpoint for both fans):

| Level | GPU fan (left) | CPU fan (right) |
|---|---|---|
| BIOS idle (auto, EC reports 22) | silent | silent |
| override 22 | audible | not audible |
| override 32 | ramps up | not noticeably |
| override 42 | full | ramps up, slower than the GPU fan |

**Full EC diff during the override** (all 256 bytes, versus the BIOS baseline, noisy bytes such as
temperatures and battery data excluded; run 2026-10-11, levels 32 / 42 / 22):

| Level | Bytes that changed |
|---|---|
| 32 | `0x0F` 0x01->0x09, `0x11` 22->32, `0x14` 0xFF->32, `0xAB` 0x08->0x00 |
| 42 | `0x0F` 0x01->0x09, `0x11` 22->42, `0x14` 0xFF->42, `0xAB` 0x08->0x00 |
| 22 | `0x0F` 0x01->0x09, `0x14` 0xFF->22, `0xAB` 0x08->0x00 |

* **No other byte moves.** There is no hidden per-fan target or readback in the 256-byte window.
* `0xAB` bit 3 (DSDT name `INIT`) is cleared while the override is engaged and restored on
  release. Meaning unknown (probably "EC auto fan control active"); not written by us.
* `0xAB` bits 4/5 (`FAN1`/`FAN2`) stay 0 in both states, so they are not fan-presence/status flags.

**Conclusion so far:** a single setpoint feeds both fans through *different* response
curves: the GPU fan reacts first, the CPU fan only at high levels. Override level 22 is
**not** the same as the BIOS idle state (GPU fan audible vs silent).

**Auto mode vs override mode (working model, from the thermal sweeps):** under a GPU-only
load the BIOS let the level creep to 29 and still cooled the CPU *worse* than override level
22 did (71.8 C vs 67.4 C steady, see `thermal-measurements.md`). So in auto mode `0x11`
follows the EC's CPU-temperature curve and the GPU fan idles, presumably because the EC never
receives a GPU temperature on Linux (`GPTV` stays `0xFF`); in override mode the setpoint drives
the GPU fan directly. This is consistent with everything heard and measured so far, but the
EC firmware itself has not been read, so treat it as a model, not a fact.

## Tested, inert

| Offset | DSDT name | Test | Result |
|---|---|---|---|
| `0x16` | `CPTV` | wrote 60 and 75 in BIOS auto mode, idle machine | value stuck; no other EC byte changed; no audible effect |
| `0x17` | `GPTV` | same | same |

## Unverified

| Offset | Name | Notes |
|---|---|---|
| `0x0F` bits 1, 2, 5 | `CPUO`, `M4GO`, `AMDK` | Unknown. An NBFC profile for another HP model restores `0x0F` to `0x05` on exit; this machine boots with `0x01`. The tool must restore the *captured* value, never a hardcoded one. |
| `0x15`, `0x18`-`0x1B` | `RSTV`, `PHTV`, `FNTV`, `BTTV`, `HDTV` | Same family as `0x16`/`0x17`; all `0xFF`/`0x00` at idle. |
| 16-bit indexed EC space | `ERIB` (`0x5D`-`0x5E`), commands `0xB0`/`0xB1` | The ACPI methods `FANG`/`FANW` read/write fan data through an indirect interface in a memory-mapped mailbox (`0xFC7E0800`+`0x580`). Possible route to independent per-fan control and a real tachometer. Not tested; needs a safe way to issue those commands. |

## Not usable

* `hp-wmi` hwmon (`fan1_input`, `fan2_input`): always `0`; this model is not supported by the driver.
* ACPI thermal tables expose only one fan ("CPU Fan #1"), via Intel DPTF.
* No GPU fan control via `nvidia-smi` (`fan.speed` is `[N/A]`).

## Notes for the daemon

* Restore the *captured* `0x0F` and `0x14` values on exit; never hardcode them.
* Read `0x12`/`0x13` at start-up and clamp every target to that range.
* Writes must be whitelisted per offset; `0x14` first, then the enable bit; on release,
  clear the enable bit first, then restore `0x14`.
