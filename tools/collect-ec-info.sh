#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Read-only hardware discovery for the HP Pavilion Gaming 15-dk1xxx.
#
# Copies the ACPI tables, takes a few raw Embedded Controller snapshots and
# records related kernel state. It NEVER writes to the EC: the ec_sys module is
# loaded WITHOUT write_support, so debugfs exposes the EC read-only.
#
# Usage: sudo tools/collect-ec-info.sh [outdir]
set -euo pipefail

if [[ $EUID -ne 0 ]]; then
    echo "run me with sudo (read-only access to ACPI tables / EC debugfs needed)" >&2
    exit 1
fi

out="${1:-$(dirname "$0")/../discovery}"
mkdir -p "$out"
out="$(cd "$out" && pwd)"

echo "[*] output dir: $out"

# --- firmware tables (to be decompiled later, as a normal user) -------------
mkdir -p "$out/acpi"
cp /sys/firmware/acpi/tables/{DSDT,ECDT} "$out/acpi/"
for t in /sys/firmware/acpi/tables/SSDT*; do cp "$t" "$out/acpi/"; done

# --- kernel / platform state ------------------------------------------------
{
    echo "## uname";            uname -a
    echo "## dmi";              grep -H . /sys/class/dmi/id/{sys_vendor,product_name,board_name,board_vendor,bios_version,bios_date}
    echo "## ioports (EC)";     grep -Ei '0062|0066|embedded|ec' /proc/ioports || true
    echo "## hp hwmon";         for f in /sys/devices/platform/hp-wmi/hwmon/hwmon*/*; do [ -f "$f" ] && echo "$f: $(cat "$f" 2>&1)"; done
    echo "## dmesg hp/wmi/ec";  dmesg | grep -Ei 'hp[-_ ]?wmi|acpi.*ec|embedded|thermal' | tail -60 || true
    echo "## wmi guids";        ls /sys/bus/wmi/devices/
} >"$out/platform.txt" 2>&1

# --- EC snapshots (read-only) -----------------------------------------------
modprobe ec_sys   # no write_support=1 on purpose
ecio=/sys/kernel/debug/ec/ec0/io
if [[ ! -r $ecio ]]; then
    mount -t debugfs none /sys/kernel/debug 2>/dev/null || true
fi
if [[ ! -r $ecio ]]; then
    echo "[!] $ecio not available" >&2
    exit 2
fi

snap() { # snap <label>
    local label="$1"
    dd if="$ecio" bs=256 count=1 of="$out/ec_${label}.bin" status=none
    {
        echo "# $(date -Is) cpu_pkg_mC=$(cat /sys/class/hwmon/hwmon*/temp1_input 2>/dev/null | head -1)"
        grep -H . /sys/devices/platform/hp-wmi/hwmon/hwmon*/{fan1_input,fan2_input,pwm1_enable} 2>&1
        nvidia-smi --query-gpu=temperature.gpu,utilization.gpu,power.draw --format=csv,noheader 2>&1 || true
    } >"$out/ec_${label}.meta"
    echo "[*] snapshot '$label' saved"
}

for i in 1 2 3 4 5; do
    snap "idle$i"
    sleep 1
done

chown -R "${SUDO_UID:-0}:${SUDO_GID:-0}" "$out"
echo "[+] done. Nothing was written to the EC."
