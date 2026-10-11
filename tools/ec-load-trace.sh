#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Read-only EC trace under controlled load, to find which EC bytes follow the
# fans. Phases: idle -> CPU load -> cooldown -> GPU load -> cooldown.
#
# Safety: NEVER writes to the EC (ec_sys has no write_support). Loads run as the
# invoking user, and are killed immediately if a temperature limit is crossed.
#
# Usage: sudo tools/ec-load-trace.sh [outfile]
set -euo pipefail

[[ $EUID -eq 0 ]] || { echo "run with sudo" >&2; exit 1; }
[[ -n "${SUDO_USER:-}" ]] || { echo "must be run through sudo (need SUDO_USER)" >&2; exit 1; }

here="$(cd "$(dirname "$0")" && pwd)"
out="${1:-$here/../discovery/trace_$(date +%Y%m%d_%H%M%S).tsv}"
ecio=/sys/kernel/debug/ec/ec0/io

CPU_LIMIT_MC=92000   # abort CPU load above this package temp (milli-C)
GPU_LIMIT_C=85       # abort GPU load above this temp

modprobe ec_sys
[[ -r $ecio ]] || mount -t debugfs none /sys/kernel/debug 2>/dev/null || true
[[ -r $ecio ]] || { echo "$ecio unavailable" >&2; exit 2; }

cpu_hwmon=""
for h in /sys/class/hwmon/hwmon*; do
    [[ "$(cat "$h/name")" == coretemp ]] && cpu_hwmon="$h"
done
[[ -n $cpu_hwmon ]] || { echo "coretemp hwmon not found" >&2; exit 2; }

load_pids=()
stop_loads() {
    for p in "${load_pids[@]:-}"; do
        [[ -n $p ]] && pkill -TERM -P "$p" 2>/dev/null || true
        [[ -n $p ]] && kill -TERM "$p" 2>/dev/null || true
    done
    load_pids=()
}
trap 'stop_loads' EXIT INT TERM

start_cpu_load() {
    for _ in $(seq "$(nproc)"); do
        runuser -u "$SUDO_USER" -- yes >/dev/null &
        load_pids+=("$!")
    done
}
start_gpu_load() {
    runuser -u "$SUDO_USER" -- python3 -I "$here/load-gpu.py" "$1" &
    load_pids+=("$!")
}

gpu_temp() { nvidia-smi --query-gpu=temperature.gpu --format=csv,noheader,nounits 2>/dev/null || echo 0; }

sample() { # sample <phase>
    local cpu gpu hex
    cpu=$(<"$cpu_hwmon/temp1_input")
    gpu=$(gpu_temp)
    hex=$(dd if="$ecio" bs=256 count=1 status=none | od -An -tx1 -v -w256 | tr -d ' \n')
    printf '%s\t%s\t%s\t%s\t%s\n' "$(date +%s.%N)" "$1" "$cpu" "$gpu" "$hex" >>"$out"
    if (( cpu >= CPU_LIMIT_MC || gpu >= GPU_LIMIT_C )); then
        echo "[!] thermal limit hit (cpu=${cpu} gpu=${gpu}) - stopping loads" >&2
        stop_loads
        ABORTED=1
    fi
}

run_phase() { # run_phase <name> <seconds>
    local name="$1" secs="$2" end
    echo "[*] phase: $name (${secs}s)"
    end=$(( $(date +%s) + secs ))
    while (( $(date +%s) < end )); do
        sample "$name"
        sleep 0.5
    done
}

ABORTED=0
mkdir -p "$(dirname "$out")"
: >"$out"

run_phase idle 10
start_cpu_load;  run_phase cpu_load 45;  stop_loads
run_phase cooldown1 25
start_gpu_load 50; run_phase gpu_load 45; stop_loads
run_phase cooldown2 25

chown "${SUDO_UID}:${SUDO_GID}" "$out"
echo "[+] done (aborted=$ABORTED): $out  (nothing was written to the EC)"
