# Changelog

## 0.2.0 — 2026-10-11

* AUTO profiles: **quiet** (the previous behaviour: BIOS until 68 °C, debounced take-over) and
  **early** (take over from 58 °C at the lowest level with small steps, no audible jump).
  Selectable from the tray's *Auto profile* submenu, `pavfan set auto <quiet|early>`, or
  `auto_profile` in the config; remembered across reboots.
* `[thresholds]` keys in the config are now overrides on top of the selected profile.
* D-Bus: new `SetAutoProfile`, new `auto_profile` field in `GetStatus` (struct signature changed,
  hence the minor version bump). `pavfan run --auto-profile`, `pavfan curve --profile`.

## 0.1.1 — 2026-10-11

* AUTO: take-over is debounced (5 s above the threshold), engaged state is held at least 30 s,
  release waits 10 s below the exit threshold. Fixes few-second fan bursts on short load spikes.
  New config keys `engage_delay_s`, `release_delay_s`, `min_engaged_s`; new `pavfan run` flags.
* Tray: left click opens the menu (SNI `ItemIsMenu`); labels "Start at login", "Exit tray".
* `pavfan completions <shell>`; bash, zsh and fish completions shipped in the packages.
* Corrected NBFC profile for Windows in `windows/nbfc/`.
* Repository renamed to `Kyr4l/pavfan`; metadata updated.

## 0.1.0 — 2026-10-11

First release: `pavfand` systemd daemon with D-Bus API, `pavfan` CLI, `pavfan-tray`,
RPM/DEB packaging, EC register map and thermal measurements for the HP Pavilion Gaming
15-dk1xxx (board 8742, BIOS F.50).
