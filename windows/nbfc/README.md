# NBFC profile for Windows

`HP Pavilion Gaming Laptop 15-dk1xxx (pavfan).xml` is a [NoteBook FanControl](https://github.com/hirschmann/nbfc)
profile for this laptop, built from the EC register map and thermal measurements in `../../docs/`.
It is a separate file: the generic profiles shipped with NBFC are left untouched.

## Install

1. Copy the XML into NBFC's `Configs` folder (by default
   `C:\Program Files (x86)\NoteBook FanControl\Configs\`).
2. In the NBFC window, pick **HP Pavilion Gaming Laptop 15-dk1xxx (pavfan)** as the config
   and enable fan control. Because `NotebookModel` matches the machine's model string, NBFC
   also offers it as the recommended profile.

## What it fixes compared with the generic profile

| | generic "HP 15-BW00x" | this profile | why |
|---|---|---|---|
| Speed range written to the EC | 19..53 | **22..42** | the EC's own limits, read from registers 0x13/0x12 (`docs/ec-register-map.md`) |
| Override flag on exit | `0x0F = 5` | clear bit 3 only (`AND 0xF7`) | this machine boots with `0x0F = 0x01`; writing 5 sets bits the BIOS never set |
| Fan target on exit | left as is | `0xFF` ("unset") | the state the BIOS runs with |
| Curve | ad-hoc | measured points | mirrors pavfan's AUTO curve where NBFC can |
| Critical temperature | 85 | 88 | same as pavfan's hard override |

## What it cannot do

* NBFC reads only the **CPU** temperature. The BIOS's real flaw on this laptop is that it
  ignores **GPU** heat, and that cannot be fixed from a CPU-only curve. pavfan on Linux feeds
  `max(CPU, GPU + 8 °C)` into its curve; there is no NBFC equivalent.
* There is no "hand the fans back to the BIOS below X °C" state in NBFC: at the lowest step the
  fans run at level 22, which keeps the GPU fan audibly spinning, unlike the BIOS's own idle.
* NBFC depends on the WinRing0 driver, which Microsoft has put on its vulnerable-driver
  blocklist; on current Windows 11 it may not load at all. That is a NBFC problem, not a
  profile problem.

## Caveat

Tested on Linux through the same EC registers; the profile itself has not been run under
Windows yet. The one untested detail is the order in which NBFC performs its exit writes
(fan reset vs. register reset). Both orders are harmless here: at worst the fans run at
maximum for under a second before the BIOS takes over.
