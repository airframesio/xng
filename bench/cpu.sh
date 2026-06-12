#!/usr/bin/env bash
# Decode CPU benchmark: wall-time vs capture duration (×-realtime)
# per mode on the bench fixtures. Higher is better; anything < 1.0
# cannot keep up live on this machine.
set -euo pipefail
cd "$(dirname "$0")/.."
XNG="${1:-target/release/xng}"

run() { # name file fmt mode rate center channels duration_s
  local t0 t1 wall
  t0=$(python3 -c 'import time; print(time.time())')
  "$XNG" decode "$2" -f "$3" -m "$4" -r "$5" -c "$6" --channels "$7" >/dev/null 2>&1
  t1=$(python3 -c 'import time; print(time.time())')
  wall=$(python3 -c "print(f'{$t1-$t0:.2f}')")
  python3 -c "print(f'{\"$1\":12} {$8:7.1f}s capture  {$wall:>7}s wall  {$8/$wall:6.1f}x realtime')"
}

echo "mode         capture        decode        speed"
# modes1 is 0.18 s — loop it 20x so process startup doesn't dominate.
if [ ! -f /tmp/bench_modes1_x20.cu8 ]; then
  for i in $(seq 20); do cat bench/data/modes1.cu8; done > /tmp/bench_modes1_x20.cu8
fi
run adsb /tmp/bench_modes1_x20.cu8 cu8 adsb 2000000 1090000000 1090 3.57
[ -f bench/data/ais_96k.cs16 ] && run ais bench/data/ais_96k.cs16 cs16 ais 96000 162000000 161.975,162.025 300
[ -f bench/data/vdl2_105k_conj.s16 ] && run vdl2 bench/data/vdl2_105k_conj.s16 cs16 vdl2 105000 136975000 136.975 46.9
[ -f bench/data/hfdl_48k.cs16 ] && run hfdl bench/data/hfdl_48k.cs16 cs16 hfdl 48000 21931000 21931k 127.3
