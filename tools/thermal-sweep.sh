#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Objective thermal characterisation: how hot does the machine get under a fixed
# load at each fan setting? Replaces "listening" (one loud fan masks the other).
#
# For every case: cool down under BIOS control, then apply the fan setting via
# `pavfan run --mode fixed`, start the load, and log temperatures.
#   case "bios" = no override (BIOS curve), numeric = fixed level (x100 RPM).
#
# Safety: pavfan itself forces maximum fans at 88 C effective temperature; on top
# of that this script stops the load at CPU >= 92 C or GPU >= 87 C. The
# override is released when each case ends (pavfan exits), on Ctrl-C and on error.
#
# Usage: sudo tools/thermal-sweep.sh [cpu|gpu|both] [seconds] [case ...]
#   cpu|gpu|both  which component to load (default cpu; both = gaming-like)
#   seconds   load duration per case (default 90)
#   case      "bios", "auto" (pavfan AUTO mode) or a fixed level 22..42
#             (default: bios 22 32 36 42)
set -euo pipefail

[[ $EUID -eq 0 ]] || { echo "run as root (sudo)" >&2; exit 1; }

here="$(cd "$(dirname "$0")" && pwd)"
pavfan="$here/../target/release/pavfan"
[[ -x $pavfan ]] || { echo "build first: cargo build --release" >&2; exit 1; }

target=${1:-cpu}
secs=${2:-90}
if (( $# >= 2 )); then shift 2; else shift $#; fi
cases=("$@"); (( ${#cases[@]} )) || cases=(bios 22 32 36 42)

[[ $target == cpu || $target == gpu || $target == both ]] || { echo "target must be cpu, gpu or both" >&2; exit 1; }
if ! [[ $secs =~ ^[0-9]+$ ]] || (( secs < 30 || secs > 300 )); then
    echo "seconds must be 30..300" >&2; exit 1
fi
for c in "${cases[@]}"; do
    [[ $c == bios || $c == auto || $c =~ ^[0-9]+$ ]] || { echo "bad case '$c'" >&2; exit 1; }
done

cpu_hwmon=""
for h in /sys/class/hwmon/hwmon*; do [[ "$(<"$h/name")" == coretemp ]] && cpu_hwmon=$h; done
[[ -n $cpu_hwmon ]] || { echo "coretemp not found" >&2; exit 1; }
cpu_c()  { local m=0 v; for f in "$cpu_hwmon"/temp*_input; do v=$(<"$f"); (( v > m )) && m=$v; done; echo $(( m / 1000 )); }
gpu_c()  { nvidia-smi --query-gpu=temperature.gpu --format=csv,noheader,nounits 2>/dev/null || echo 0; }
# EC fan level (0x11, x100 RPM): what the EC is actually commanding right now (read-only).
ecio=/sys/kernel/debug/ec/ec0/io
ec_level() { dd if="$ecio" bs=1 skip=17 count=1 status=none 2>/dev/null | od -An -tu1 | tr -d ' ' || echo "?"; }

out="$here/../discovery/sweep_${target}_$(date +%Y%m%d_%H%M%S).csv"
mkdir -p "$(dirname "$out")"
echo "case,t_s,cpu_c,gpu_c,ec_level" >"$out"

pids=()
cleanup() {
    trap - EXIT INT TERM HUP
    for p in "${pids[@]:-}"; do [[ -n $p ]] && kill -TERM "$p" 2>/dev/null || true; done
    sleep 1
    # belt and braces: make sure the fans are back with the BIOS
    "$pavfan" release >/dev/null 2>&1 || true
}
trap cleanup EXIT INT TERM HUP

start_load() {
    if [[ $target == cpu || $target == both ]]; then
        for _ in $(seq "$(nproc)"); do yes >/dev/null & pids+=("$!"); done
    fi
    if [[ $target == gpu || $target == both ]]; then
        python3 -I "$here/load-gpu.py" "$secs" & pids+=("$!")
    fi
}
stop_load() {
    for p in "${pids[@]:-}"; do [[ -n $p ]] && kill -TERM "$p" 2>/dev/null || true; done
    pids=()
}

wait_cool() { # BIOS control, load stopped: wait until both are cool
    local limit=240 t=0
    echo -n "    cooling down"
    while (( t < limit )); do
        if (( $(cpu_c) <= 48 && $(gpu_c) <= 45 )); then break; fi
        echo -n "."; sleep 5; t=$(( t + 5 ))
    done
    echo " cpu=$(cpu_c)C gpu=$(gpu_c)C"
}

echo "[*] sweep target=$target load=${secs}s cases: ${cases[*]}"
echo "[*] raw data: $out"

for c in "${cases[@]}"; do
    echo; echo "[*] case: $c"
    wait_cool

    pf=""
    if [[ $c != bios ]]; then
        log="${out%.csv}_$c.log"
        if [[ $c == auto ]]; then
            "$pavfan" run --mode auto --duration $(( secs + 15 )) --quiet >"$log" 2>&1 &
        else
            "$pavfan" run --mode fixed --level "$c" --duration $(( secs + 15 )) --quiet >"$log" 2>&1 &
        fi
        pf=$!; pids+=("$pf")
        sleep 3     # let the override settle before the load starts
        if ! kill -0 "$pf" 2>/dev/null; then
            echo "[!] pavfan did not start (is another tool controlling the fans?); aborting" >&2
            exit 1
        fi
    fi

    start_load
    start=$SECONDS
    while (( SECONDS - start < secs )); do
        cpu=$(cpu_c); gpu=$(gpu_c)
        echo "$c,$(( SECONDS - start )),$cpu,$gpu,$(ec_level)" >>"$out"
        if (( cpu >= 92 || gpu >= 87 )); then
            echo "[!] temperature limit hit (cpu=$cpu gpu=$gpu): stopping this case" >&2
            break
        fi
        sleep 2
    done
    stop_load
    if [[ -n $pf ]]; then
        kill -TERM "$pf" 2>/dev/null || true
        wait "$pf" 2>/dev/null || true
        pids=()
    fi
    echo "    last: cpu=$(cpu_c)C gpu=$(gpu_c)C ec_level=$(ec_level)"
done

echo; echo "[*] summary (load: $target, ${secs}s per case; steady = mean of the last 30 s)"
printf '%-6s %8s %10s %8s %10s %9s\n' case cpu_max cpu_steady gpu_max gpu_steady ec_level
for c in "${cases[@]}"; do
    awk -F, -v c="$c" -v secs="$secs" '
        $1==c && $2>0 { n++; t[n]=$2; cp[n]=$3; gp[n]=$4; lv[n]=$5
                        if ($3>cm) cm=$3; if ($4>gm) gm=$4 }
        END { if (!n) { print c": no data"; exit }
              last=t[n]; s=0; k=0; g=0; lmin=999; lmax=0
              for (i=1;i<=n;i++) if (t[i] >= last-30) { s+=cp[i]; g+=gp[i]; k++
                                                        if (lv[i]+0<lmin) lmin=lv[i]+0; if (lv[i]+0>lmax) lmax=lv[i]+0 }
              lev = (lmin==lmax) ? lmin : lmin"-"lmax
              printf "%-6s %7dC %9.1fC %7dC %9.1fC %9s\n", c, cm, s/k, gm, g/k, lev }' "$out"
done
echo; echo "[+] done; fans handed back to the BIOS."
