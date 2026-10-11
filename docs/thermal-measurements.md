# Thermal measurements

Measured on the real HP Pavilion Gaming 15-dk1xxx (board 8742, BIOS F.50) with
`tools/thermal-sweep.sh`, fans freshly cleaned, 2026-10-11. Each case: cool down under BIOS
control to <= 48 C CPU / 45 C GPU, apply the fan setting, run the load for 90 s, log every 2 s.
"steady" = mean of the last 30 s. Room temperature was not controlled: compare cases within one
sweep, not across sweeps. Expect about +/-1.5 C of noise between cases.

Fan levels are the shared EC setpoint (`0x14`, units of 100 RPM, range 22..42), applied with
`pavfan run --mode fixed`. "BIOS" = no override. `ec_level` = what the EC reported in `0x11`
during the last 30 s (for the BIOS case: what the firmware chose by itself).

## Sweep 1: CPU-only load (`yes` on all threads)

| Fan setting | CPU max | CPU steady | GPU max | GPU steady | ec_level |
|---|---|---|---|---|---|
| BIOS control | 76 C | 74.9 C | 48 C | 47.5 C | not logged |
| fixed 22 | 77 C | 75.1 C | 48 C | 48.0 C | 22 |
| fixed 32 | 75 C | 74.1 C | 48 C | 47.1 C | 32 |
| fixed 36 | 75 C | 73.7 C | 47 C | 47.0 C | 36 |
| fixed 42 | 75 C | 73.1 C | 47 C | 46.9 C | 42 |

The whole 22..42 range is worth about 2 C on the CPU. BIOS control sits where fixed 32 does.
For CPU-only heat the override brings little and costs GPU-fan noise.

## Sweep 2: GPU-only load (headless CUDA, ~50 W), CPU idle

| Fan setting | CPU max | CPU steady | GPU max | GPU steady | ec_level |
|---|---|---|---|---|---|
| BIOS control | 74 C | **71.8 C** | 62 C | 60.1 C | 22 -> 29 (slow creep) |
| fixed 22 | 69 C | 67.4 C | 60 C | 59.4 C | 22 |
| fixed 32 | 74 C | 68.0 C | 59 C | 58.1 C | 32 |
| fixed 36 | 71 C | 69.4 C | 58 C | 57.5 C | 36 |
| fixed 42 | 71 C | **66.0 C** | 58 C | 57.1 C | 42 |

This is the shared-cooler problem in numbers: with the CPU idle, GPU heat alone drags the CPU to
72 C under BIOS control, and the firmware barely reacts (level 29 after 90 s). Any override level
cools the CPU 3-6 C better. Override **22** beats BIOS **29**, so the level does not mean the same
thing in the two modes: in auto mode the GPU fan is evidently not driven by it (see the working
model in `ec-register-map.md`). The level *within* the override range matters less than the
override itself here (differences are within noise except 42).

## Sweep 3: CPU + GPU load together (gaming-like)

| Fan setting | CPU max | CPU steady | GPU max | GPU steady | ec_level |
|---|---|---|---|---|---|
| BIOS control | **92 C** (case stopped by the 92 C safety limit at ~50 s) | 87.5 C (still rising) | 68 C | 65.1 C | 22 -> 42, reaching 42 only at t=37 s with the CPU already at 88 C |
| fixed 22 | 89 C | 88.1 C | 67 C | 66.5 C | 42 (pavfan hard override) |
| fixed 32 | 88 C | 87.7 C | 67 C | 66.1 C | 32 -> 42 (hard override) |
| fixed 36 | 89 C | 87.4 C | 67 C | 66.1 C | 36 -> 42 (hard override) |
| fixed 42 | 90 C | 87.9 C | 67 C | 66.2 C | 42 |

Two things to read here:

1. **The ceiling.** With both fans at maximum this cooler holds CPU+GPU load at about 88 C.
   No fan curve can beat that; it is the heatsink, not the airflow. (All pavfan cases ended at
   level 42 because pavfan's hard thermal override forces maximum fans at 88 C effective, so the
   low "fixed" cases are not real low-fan measurements under this load.)
2. **The lag.** The BIOS took 37 s to reach maximum fans and the CPU spiked to 92 C on the way.
   pavfan ramps immediately and peaked at 88-90 C. That spike is the throttling scenario; the
   immediate ramp is what the tool buys under combined load.

## Sweep 4: GPU-only load, BIOS vs AUTO (back to back, same protocol as sweep 2)

| Fan setting | CPU max | CPU steady | GPU max | GPU steady | ec_level |
|---|---|---|---|---|---|
| BIOS control | 78 C | **73.9 C** | 64 C | 62.6 C | 22 -> 29..32 (slow creep) |
| pavfan AUTO | 72 C | **70.9 C** | 60 C | 59.1 C | engaged at 14 s (30), settled at 32 |

AUTO ran the CPU 3 C cooler, the GPU 3.5 C cooler and cut the CPU peak by 6 C, at a moderate
fan level (32). It took over on the *CPU* side of the effective temperature (the CPU reached
68 C from GPU heat before `GPU + 8` did), which is fine: that is the coupling it exists for.

## Sweep 5: CPU + GPU load, BIOS vs AUTO (same protocol as sweep 3)

| Fan setting | CPU max | CPU steady | GPU max | GPU steady | first time >= 85 C | samples >= 90 C |
|---|---|---|---|---|---|---|
| BIOS control | 92 C (case stopped by the safety limit at 73 s) | 90.1 C | 70 C | 68.9 C | 26 s | 11 of 36 |
| pavfan AUTO | 91 C (at 89 s, still rising) | 87.1 C | 67 C | 65.3 C | 63 s | 3 of 44 |

AUTO engaged 4 s in, ramped 32 -> 42 by 65 s following the curve (the hard override was not the
driver), reached 85 C 2.4x later than the BIOS and ran about 3 C cooler. **But the honest
headline is the ceiling:** under sustained full CPU + GPU load this cooler ends up at 89-91 C with
both fans at maximum, whatever controls them. AUTO buys time and a few degrees; it does not
change the heatsink. Real workloads are lighter than this synthetic worst case (`yes` on all
threads plus a CUDA burn), so the margin in practice should be larger than measured here.

Both AUTO cases released cleanly to the BIOS at the end (see the `*_auto.log` files).

## What the AUTO mode is built on

* The override never cooled the CPU measurably worse than the BIOS at any level, in any sweep.
* Its gain is largest under GPU heat (sweep 2), which the BIOS ignores. So the AUTO input is
  `max(CPU, GPU + 8 C)`: GPU heat is weighted *up*.
* CPU-only heat gains little (sweep 1), so AUTO does not take over for moderate CPU heat:
  it engages at 68 C effective and releases at 62 C.
* Under combined load the machine needs maximum fans anyway (sweep 3), so the curve reaches 42 at
  85 C and the hard override at 88 C is a backstop rather than the normal driver.
* Curve: 68 C -> 30, 76 C -> 34, 82 C -> 39, 85 C -> 42 (levels in x100 RPM).
* Timing (added after real use): taking over instantly on crossing 68 C produced few-second
  fan bursts on short CPU spikes (silent -> level 30 -> silent). AUTO now takes over only
  after 5 s above the threshold, holds at least 30 s, and releases after 10 s below 62 C.
  The hard override at 88 C stays instant.

Sweeps 4 and 5 measured this AUTO configuration against the BIOS: see above.
