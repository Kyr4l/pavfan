# Firmware notes: HP Pavilion Gaming 15-dk1xxx (board 8742, BIOS F.50 / package F50A-08740)

Findings from reading the machine's own ACPI tables and the official BIOS package.
No firmware is stored in this repository (it is copyrighted and may embed
machine-specific data); the paths below refer to local, git-ignored copies.

## BIOS image layout

* `08740.bin`: 16 MiB Intel SPI image: descriptor, ME region, BIOS region (UEFI, Insyde H2O).
* **No EC region** in the flash descriptor: the EC firmware is not part of this image, so
  the EC's own fan tables cannot be read from it.
* ODM is Compal; many modules are named `Compal*` / `H19*`.

## hp-wmi: why it reports 0 RPM (verified in the ACPI code)

The HP BIOS WMI interface (`WMAA` -> `WHCM`, command `0x20008`) implements sub-commands
`0x01`..`0x2F` as methods `GCxx`. The ones Linux `hp-wmi` uses for fans are **stubs**: they
read/write ACPI scratch buffers (`W26B`, `W2AB`, `W2EB`) and never touch the EC.
`GC2D` (fan speed) returns zeros from such a buffer. Conclusion: **WMI cannot read or
control the fans on this model**; the driver is not "mis-reading", the firmware does not
implement it.

`GC2F` returns static data: 7 rows x 3 levels (20..42, x100 RPM), consistent with the EC's
22..42 range. Informational only.

## Compal CMFC interface (decoded as far as it is useful)

`\_SB.PCI0.LPCB.EC0.CMFC(function, subfunction, data, ...)` (SSDT19) is Compal's OEM
management call. It raises one of two software SMIs; each is handled by a separate SMM driver.

**SMI `0xCF`** (`PFUN`; functions `0x80`-`0x86` and `0x92`), module `CompalCMFCOEMSwSmi`:
a table of 16-byte `{function, subfunction, handler}` entries. The handlers read and write the
BIOS `Setup` NVRAM variable (boot/priority style data). Its only port access is the SMI data
port `0xB3`; it has no EC access and no thermal content. **Not fan-related (verified in
decompiled code).**

**SMI `0xCE`** (`CFUN`; every other function), module `CompalCMFCCommonSwSmi`:
 * `0x24`, `0x25`, `0x28`: dedicated handlers that return identity strings and GUIDs
   (serial-number / product-name / UUID / OS-type variable names). Not thermal.
 * `0x13`, `0x14`, `0x17`, `0x20`, `0x21`, `0x22`, `0x90`-`0x99`, `0xFE`: forwarded to a
   protocol (GUID `CBEB63D0-76E3-11E0-A1F0-0800200C9A66`) whose implementer was **not found**
   in the image (only this module contains the GUID). What these functions do is unknown.

The SMM drivers share an EC library: it sends OEM command bytes with the standard ACPI EC
protocol (ports `0x66`/`0x62`, or the memory mailbox) and reaches EC registers through the
window at `0xFC7E0000`: register `r` is read at `0xFC7D1400 + r` and written at
`0xFC7E1400 + r`; `5A A5` at `0xFC7E0D80` is an "EC window alive" signature. The mailbox at
`0xFC7E0D80`+ is the `ECCD` region of the DSDT (busy/trigger bits at `+2`, result `+3`,
command byte `+0xB`, data after).

**Conclusion:** no function that drives the fans was identified. Continuing means finding the
protocol implementer for the forwarded `0xCE` functions, and would also need a way to call
`CMFC` from Linux. Parked; revisit only if the shared-setpoint approach proves insufficient.

## Other facts

* EC memory window: `OperationRegion ECMM/ECCD, SystemMemory, 0xFC7E0800, 0x1000`.
  Only the first 0x100 bytes are described as fields (see `ec-register-map.md`).
* Indirect EC access (`EIDR`/`EIDW`, `FANG`/`FANW`): 16-bit index in `ERIB` (EC `0x5D`),
  commands `0xB0`/`0xB1`. No caller found in the ACPI tables.
* ACPI thermal tables (Intel DPTF, SSDT11/SSDT14) expose one fan: "CPU Fan #1".
