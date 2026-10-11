#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Interactive EC *write* test: step the "CPU fan" target through a few speeds so
# a human can check which physical fan(s) follow it, then hand control back.
#
# Mechanism (same as the NBFC profile used): EC 0x0F bit3 = override enable,
# 0x14 = target speed in units of 100 RPM, 0x11 = measured speed.
#
# Safety properties:
#   * only runs on the exact DMI product/board it was reverse-engineered for;
#   * writes are limited to a whitelist: 0x14 (whole byte) and 0x0F bit 3 only;
#   * every target is validated against the EC's own [FNMN, FNMX] limits
#     (0x13/0x12), i.e. never below the BIOS idle speed, never above its max;
#   * ec_sys is reloaded with write_support only for this test, and the write
#     capability is VERIFIED before the first write;
#   * original values are captured first and restored on EXIT/INT/TERM/HUP;
#   * aborts (and restores) if CPU package >= 90C or GPU >= 85C.
#
# Usage: sudo tools/ec-fan-test.sh [level ...]   levels in x100 RPM, default: 32 42 22
#        each level is held until you press ENTER (auto-advances after 90 s)
set -euo pipefail

[[ $EUID -eq 0 ]] || { echo "run with sudo" >&2; exit 1; }

# --- identity check ---------------------------------------------------------
prod=$(</sys/class/dmi/id/product_name); board=$(</sys/class/dmi/id/board_name)
if [[ $prod != "HP Pavilion Gaming Laptop 15-dk1xxx" || $board != "8742" ]]; then
    echo "refusing: unsupported machine ($prod / $board)" >&2
    exit 1
fi

levels=("$@"); (( ${#levels[@]} )) || levels=(32 42 22)
for l in "${levels[@]}"; do [[ $l =~ ^[0-9]+$ ]] || { echo "bad level '$l'" >&2; exit 1; }; done

REG_FLAGS=0x0F; REG_RPM=0x11; REG_MAX=0x12; REG_MIN=0x13; REG_TARGET=0x14
FNSW_BIT=0x08
ecio=/sys/kernel/debug/ec/ec0/io

# --- EC module: reload with write support, and VERIFY it ---------------------
reload_ec_sys() { # reload_ec_sys [modprobe args...]
    if [[ -d /sys/module/ec_sys ]]; then rmmod ec_sys; fi
    [[ ! -d /sys/module/ec_sys ]] || { echo "could not unload ec_sys" >&2; return 1; }
    modprobe ec_sys "$@"
}
reload_ec_sys write_support=1
[[ "$(</sys/module/ec_sys/parameters/write_support)" == Y ]] \
    || { echo "ec_sys write_support is not enabled; refusing" >&2; reload_ec_sys || true; exit 2; }
[[ -e $ecio ]] || { echo "$ecio missing" >&2; exit 2; }

ec_read()  { dd if="$ecio" bs=1 skip=$(( $1 )) count=1 status=none | od -An -tu1 | tr -d ' '; }
ec_write() { # whitelist enforced here
    local off=$(( $1 )) val=$(( $2 ))
    (( off == REG_TARGET || off == REG_FLAGS )) || { echo "BUG: write to non-whitelisted 0x$(printf %02x $off)" >&2; exit 99; }
    (( val >= 0 && val <= 255 )) || { echo "BUG: bad value $val" >&2; exit 99; }
    printf '%b' "$(printf '\\x%02x' "$val")" | dd of="$ecio" bs=1 seek=$off count=1 conv=notrunc status=none
}

cpu_hwmon=""
for h in /sys/class/hwmon/hwmon*; do [[ "$(<"$h/name")" == coretemp ]] && cpu_hwmon=$h; done
[[ -n $cpu_hwmon ]] || { echo "coretemp not found" >&2; exit 2; }
gpu_temp() { nvidia-smi --query-gpu=temperature.gpu --format=csv,noheader,nounits 2>/dev/null || echo 0; }

# --- capture originals, arm restore ------------------------------------------
orig_flags=$(ec_read $REG_FLAGS); orig_target=$(ec_read $REG_TARGET)
ec_min=$(ec_read $REG_MIN);       ec_max=$(ec_read $REG_MAX)
armed=0
restore() {
    trap - EXIT INT TERM HUP
    if (( armed )); then
        # 1) give control back to the BIOS first, 2) then restore the target byte
        ec_write $REG_FLAGS "$orig_flags" || true
        ec_write $REG_TARGET "$orig_target" || true
        printf '[+] restored: 0x0F=0x%02x 0x14=0x%02x (expected 0x%02x / 0x%02x)\n' \
            "$(ec_read $REG_FLAGS)" "$(ec_read $REG_TARGET)" "$orig_flags" "$orig_target"
    fi
    reload_ec_sys || true      # back to the default read-only module
}
trap restore EXIT INT TERM HUP

printf '[*] original: 0x0F=0x%02x 0x14=0x%02x  EC limits: min=%d max=%d (x100 RPM)\n' \
    "$orig_flags" "$orig_target" "$ec_min" "$ec_max"

# sanity: limits must look like what we saw during discovery
(( ec_min >= 10 && ec_min <= 30 && ec_max >= 30 && ec_max <= 60 && ec_min < ec_max )) \
    || { echo "unexpected EC limits; refusing" >&2; exit 3; }
for l in "${levels[@]}"; do
    (( l >= ec_min && l <= ec_max )) || { echo "level $l outside EC limits $ec_min..$ec_max" >&2; exit 3; }
done

ec_dump() { dd if="$ecio" bs=256 count=1 status=none | od -An -tu1 -v -w256; }

# bytes that change on their own (temps, battery, counters): not interesting in a diff
noisy=" 6 167 176 177 178 182 183 198 219 227 228 229 230 231 232 241 242 "
declare -a base
diff_vs_base() { # print EC bytes that differ from the baseline dump (ignoring noisy ones)
    local -a cur; read -r -a cur <<<"$(ec_dump)"
    local i out=""
    for i in "${!cur[@]}"; do
        [[ $noisy == *" $i "* ]] && continue
        (( cur[i] != base[i] )) && out+=$(printf ' 0x%02X:%d->%d' "$i" "${base[i]}" "${cur[i]}")
    done
    echo "    EC bytes changed vs BIOS baseline:${out:- none}"
}

status_line() { # status_line <label>
    local cpu gpu
    cpu=$(<"$cpu_hwmon/temp1_input"); gpu=$(gpu_temp)
    printf '  %-10s fan_rpm=%4d  target=0x%02x flags=0x%02x  cpu=%2dC gpu=%2sC\n' "$1" \
        $(( $(ec_read $REG_RPM) * 100 )) "$(ec_read $REG_TARGET)" "$(ec_read $REG_FLAGS)" $(( cpu/1000 )) "$gpu"
    if (( cpu >= 90000 || gpu >= 85 )); then echo "[!] temperature limit: aborting" >&2; exit 4; fi
}

wait_key() { # wait_key <label>   -- status every 2 s until ENTER (or 90 s)
    # Only a full line (Enter) advances: multi-byte keys such as the arrows
    # (ESC [ A) must not be mistaken for "next step".
    local end=$(( SECONDS + 90 ))
    while read -r -t 0.05 -n 1000 _; do :; done      # drop anything typed earlier
    while (( SECONDS < end )); do
        status_line "$1"
        if read -r -t 2 _; then return 0; fi
    done
}

echo "[*] baseline (BIOS control). Note the sound of BOTH fans. Press ENTER to continue."
read -r -a base <<<"$(ec_dump)"
wait_key baseline
read -r -a base <<<"$(ec_dump)"

for l in "${levels[@]}"; do
    printf '\n[*] >>> target = %d  (= %d RPM)\n' "$l" $(( l * 100 ))
    echo "    Check LEFT (GPU) and RIGHT (CPU) fan: sound, airflow at each vent. Press ENTER for the next step."
    armed=1
    ec_write $REG_TARGET "$l"                          # value first, then enable override
    ec_write $REG_FLAGS $(( orig_flags | FNSW_BIT ))
    sleep 3
    diff_vs_base
    wait_key "tgt=$l"
done

echo
echo "[*] >>> releasing to BIOS"
restore
sleep 1
echo "[*] done."
