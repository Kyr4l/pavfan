#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Hypothesis test: are EC bytes 0x16 (CPTV) and 0x17 (GPTV) per-component
# temperature inputs ("CPU temp value" / "GPU temp value", 0xFF = unset) that
# feed the CPU / GPU fan curves independently?
#
# The firmware is left in BIOS (auto) mode: 0x14/0x0F are never touched. For
# each step ONE mailbox byte is set to a "hot" value, held until you press
# ENTER, then restored to its original (0xFF) before the next step.
#
# Safety properties:
#   * only runs on the exact DMI product/board;
#   * writes whitelisted to 0x16 and 0x17 ONLY;
#   * values allowed: 60..90, or the captured original. Under either reading of
#     the byte (a temperature, or a speed in x100 RPM) that can only mean
#     "hotter / faster", never "fan off";
#   * refuses to start unless 0x0F/0x14 are in the pristine BIOS state and both
#     mailbox bytes still hold 0xFF;
#   * originals are restored on EXIT/INT/TERM/HUP, and write_support is
#     disabled again afterwards;
#   * aborts (and restores) if CPU package >= 85C or GPU >= 80C.
#
# Usage: sudo tools/ec-mailbox-test.sh [reg:value ...]   (default: 17:60 17:75 16:60 16:75)
#        reg is 16 or 17 (hex), value is decimal 60..90
set -euo pipefail

[[ $EUID -eq 0 ]] || { echo "run with sudo" >&2; exit 1; }

prod=$(</sys/class/dmi/id/product_name); board=$(</sys/class/dmi/id/board_name)
if [[ $prod != "HP Pavilion Gaming Laptop 15-dk1xxx" || $board != "8742" ]]; then
    echo "refusing: unsupported machine ($prod / $board)" >&2
    exit 1
fi

steps=("$@"); (( ${#steps[@]} )) || steps=(17:60 17:75 16:60 16:75)
for s in "${steps[@]}"; do
    [[ $s =~ ^(16|17):([0-9]+)$ ]] || { echo "bad step '$s' (want 16:NN or 17:NN)" >&2; exit 1; }
    (( BASH_REMATCH[2] >= 60 && BASH_REMATCH[2] <= 90 )) || { echo "value out of 60..90 in '$s'" >&2; exit 1; }
done

ecio=/sys/kernel/debug/ec/ec0/io
reload_ec_sys() {
    if [[ -d /sys/module/ec_sys ]]; then rmmod ec_sys; fi
    [[ ! -d /sys/module/ec_sys ]] || { echo "could not unload ec_sys" >&2; return 1; }
    modprobe ec_sys "$@"
}
reload_ec_sys write_support=1
[[ "$(</sys/module/ec_sys/parameters/write_support)" == Y ]] \
    || { echo "ec_sys write_support is not enabled; refusing" >&2; reload_ec_sys || true; exit 2; }
[[ -e $ecio ]] || { echo "$ecio missing" >&2; exit 2; }

ec_read() { dd if="$ecio" bs=1 skip=$(( $1 )) count=1 status=none | od -An -tu1 | tr -d ' '; }
ec_dump() { dd if="$ecio" bs=256 count=1 status=none | od -An -tu1 -v -w256; }
ec_write() { # whitelist enforced here
    local off=$(( $1 )) val=$(( $2 ))
    (( off == 0x16 || off == 0x17 )) || { echo "BUG: write to non-whitelisted 0x$(printf %02x $off)" >&2; exit 99; }
    (( val == orig_ff || (val >= 60 && val <= 90) )) || { echo "BUG: bad value $val" >&2; exit 99; }
    printf '%b' "$(printf '\\x%02x' "$val")" | dd of="$ecio" bs=1 seek=$off count=1 conv=notrunc status=none
}

cpu_hwmon=""
for h in /sys/class/hwmon/hwmon*; do [[ "$(<"$h/name")" == coretemp ]] && cpu_hwmon=$h; done
[[ -n $cpu_hwmon ]] || { echo "coretemp not found" >&2; exit 2; }
gpu_temp() { nvidia-smi --query-gpu=temperature.gpu --format=csv,noheader,nounits 2>/dev/null || echo 0; }

# --- preconditions: pristine BIOS state ---------------------------------------
orig_ff=$(ec_read 0x16); orig16=$orig_ff; orig17=$(ec_read 0x17)
flags=$(ec_read 0x0F); target=$(ec_read 0x14)
printf '[*] state: 0x0F=0x%02x 0x14=0x%02x 0x16=0x%02x 0x17=0x%02x\n' "$flags" "$target" "$orig16" "$orig17"
(( flags == 0x01 && target == 0xFF && orig16 == 0xFF && orig17 == 0xFF )) \
    || { echo "EC is not in the pristine BIOS state (expected 0x01/0xff/0xff/0xff); refusing" >&2; reload_ec_sys || true; exit 3; }

armed=0
restore() {
    trap - EXIT INT TERM HUP
    if (( armed )); then
        ec_write 0x16 "$orig16" || true
        ec_write 0x17 "$orig17" || true
        printf '[+] restored: 0x16=0x%02x 0x17=0x%02x (expected 0xff / 0xff)\n' "$(ec_read 0x16)" "$(ec_read 0x17)"
    fi
    reload_ec_sys || true
}
trap restore EXIT INT TERM HUP

status_line() {
    local cpu gpu
    cpu=$(<"$cpu_hwmon/temp1_input"); gpu=$(gpu_temp)
    printf '  %-9s 0x11=%3d(x100RPM) 0x14=0x%02x 0x16=0x%02x 0x17=0x%02x  cpu=%2dC gpu=%2sC\n' "$1" \
        "$(ec_read 0x11)" "$(ec_read 0x14)" "$(ec_read 0x16)" "$(ec_read 0x17)" $(( cpu/1000 )) "$gpu"
    if (( cpu >= 85000 || gpu >= 80 )); then echo "[!] temperature limit: aborting" >&2; exit 4; fi
}

# bytes that change on their own (temps, battery, counters): not interesting in a diff
noisy=" 6 167 176 177 178 182 183 198 219 227 228 229 230 231 232 241 242 "
diff_vs_base() { # print EC bytes that differ from the baseline dump (ignoring noisy ones)
    local -a cur; read -r -a cur <<<"$(ec_dump)"
    local i out=""
    for i in "${!cur[@]}"; do
        [[ $noisy == *" $i "* ]] && continue
        (( cur[i] != base[i] )) && out+=$(printf ' 0x%02X:%d->%d' "$i" "${base[i]}" "${cur[i]}")
    done
    echo "  EC changes vs baseline:${out:- none}"
}

wait_key() { # status every 2 s until ENTER (or 60 s)
    local end=$(( SECONDS + 60 ))
    while read -r -t 0.05 -n 1000 _; do :; done
    while (( SECONDS < end )); do
        status_line "$1"
        if read -r -t 2 _; then return 0; fi
    done
}

declare -a base; read -r -a base <<<"$(ec_dump)"
echo "[*] BIOS-controlled baseline. Press ENTER to start the steps."
wait_key baseline

for s in "${steps[@]}"; do
    reg=$(( 0x${s%%:*} )); val=${s##*:}
    printf '\n[*] >>> setting 0x%02x = %d   (all else BIOS-controlled)\n' "$reg" "$val"
    echo "    Which fan changes: LEFT (GPU)? RIGHT (CPU)? both? neither? Press ENTER for the next step."
    armed=1
    ec_write "$reg" "$val"
    sleep 3
    diff_vs_base
    wait_key "$s"
    ec_write "$reg" 255          # back to "unset" before the next step
    echo "    (restored to 0xff; fans return to BIOS behaviour)"
    sleep 4
done

echo; echo "[*] done"
restore
