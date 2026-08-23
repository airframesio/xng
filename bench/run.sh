#!/usr/bin/env bash
# Off-air decode-count regression gate. Runs the vendored / released
# benchmark fixtures through `xng decode` and fails when a count drops
# below its committed baseline (see baselines.json; thresholds already
# include a small noise margin — decode counts are deterministic for a
# fixed binary, the margin only absorbs intentional-tradeoff slack).
#
# Usage: bench/run.sh [path-to-xng-binary]
set -euo pipefail
cd "$(dirname "$0")/.."

XNG="${1:-target/release/xng}"
BASE="bench/baselines.json"

fail=0

count() { # file fmt mode rate center channels
  "$XNG" decode "$1" -f "$2" -m "$3" -r "$4" -c "$5" --channels "$6" 2>&1 |
    grep "session complete" | grep -o '[0-9]* frame' | grep -o '[0-9]*'
}

count_crc() { # file fmt mode rate center channels — sums CRC-OK frames across channels
  # ACARS emits bad-CRC frames too (noise-dependent), so its gate counts only
  # frames with valid CRC, summed over the per-channel "N with valid CRC" lines.
  "$XNG" decode "$1" -f "$2" -m "$3" -r "$4" -c "$5" --channels "$6" 2>&1 |
    grep -oE '[0-9]+ with valid CRC' | grep -oE '^[0-9]+' | awk '{s+=$1} END{print s+0}'
}

check() { # name actual
  local min
  min=$(python3 -c "import json;print(json.load(open('$BASE'))['$1'])")
  if [ "$2" -lt "$min" ]; then
    echo "REGRESSION: $1 decoded $2 frames (baseline >= $min)"
    fail=1
  else
    echo "ok: $1 = $2 frames (baseline >= $min)"
  fi
}

check_max() { # name actual — ceiling gate for false-positive fixtures
  local max
  max=$(python3 -c "import json;print(json.load(open('$BASE'))['$1'])")
  if [ "$2" -gt "$max" ]; then
    echo "REGRESSION: $1 decoded $2 frames (ceiling <= $max — false positives)"
    fail=1
  else
    echo "ok: $1 = $2 frames (ceiling <= $max)"
  fi
}

# ADS-B: the canonical dump1090 test capture (vendored).
adsb=$(count bench/data/modes1.cu8 cu8 adsb 2000000 1090000000 1090)
check adsb_modes1 "$adsb"

# ADS-B false-positive gate: 20 s of quiet live 1090 RF (release
# asset). Near-floor candidate gates + CRC trials produce ~70 phantom
# frames/min on noise unless the two-sighting ICAO confirmation holds
# them back; this asserts the phantom rate stays ~zero.
if [ -f bench/data/adsb_quiet_24m.cu8 ]; then
  quiet=$(count bench/data/adsb_quiet_24m.cu8 cu8 adsb 2400000 1090000000 1090)
  check_max adsb_quiet_max "$quiet"
else
  echo "skip: bench/data/adsb_quiet_24m.cu8 not present (release asset)"
fi

# AIS: 5-minute off-air capture (release asset; fetched by CI or
# manually: gh release download bench-fixtures-v1 -p ais_96k.cs16 -D bench/data/).
if [ -f bench/data/ais_96k.cs16 ]; then
  ais=$(count bench/data/ais_96k.cs16 cs16 ais 96000 162000000 161.975,162.025)
  check ais_offair "$ais"
else
  echo "skip: bench/data/ais_96k.cs16 not present (release asset)"
fi

# HFDL: the 21931 kHz off-air capture (release asset), 48 kS/s s16.
if [ -f bench/data/hfdl_48k.cs16 ]; then
  hfdl=$(count bench/data/hfdl_48k.cs16 cs16 hfdl 48000 21931000 21931k)
  check hfdl_offair "$hfdl"
else
  echo "skip: bench/data/hfdl_48k.cs16 not present (release asset)"
fi

# VDL2: the sigidwiki off-air capture (release asset), 105 kS/s s16.
if [ -f bench/data/vdl2_105k_conj.s16 ]; then
  vdl2=$(count bench/data/vdl2_105k_conj.s16 cs16 vdl2 105000 136975000 136.975)
  check vdl2_offair "$vdl2"
else
  echo "skip: bench/data/vdl2_105k_conj.s16 not present (release asset)"
fi

# VDL2 (second path): Opflasher off-air capture (release asset), 2.5 MS/s
# downconverted to 105 kS/s single channel. Head-to-head: xng 13 vs dumpvdl2
# 2.6.0 12 on the same fixture (real Air NZ ZK-NNB ADS-C-over-VDL2). Proves the
# ~98%+ parity generalizes across a second antenna/RF path.
if [ -f bench/data/vdl2_opflasher_105k.cs16 ]; then
  vdl2b=$(count bench/data/vdl2_opflasher_105k.cs16 cs16 vdl2 105000 136800000 136.800)
  check vdl2_offair2 "$vdl2b"
else
  echo "skip: bench/data/vdl2_opflasher_105k.cs16 not present (release asset)"
fi

# Radiosonde: the projecthorus/radiosonde_auto_rx RS41 performance sample
# (release asset), 96 kS/s cf32 complex float. Oracle-anchored 119/119 vs rs41mod.
if [ -f bench/data/sonde_96k.cf32 ]; then
  sonde=$(count bench/data/sonde_96k.cf32 cf32 sonde 96000 404000000 404M)
  check sonde_offair "$sonde"
else
  echo "skip: bench/data/sonde_96k.cf32 not present (release asset)"
fi

# NAVTEX: SDRplay official navtex.zip IQ demo (release asset), 62.5 kS/s cs16,
# center 516 kHz, NAVTEX channel at 518 kHz — exercises the narrow-passband DDC.
if [ -f bench/data/navtex_62500.cs16 ]; then
  navtex=$(count bench/data/navtex_62500.cs16 cs16 navtex 62500 516000 518000)
  check navtex_offair "$navtex"
else
  echo "skip: bench/data/navtex_62500.cs16 not present (release asset)"
fi

# ACARS (POA): Opflasher off-air capture (release asset), the single active POA
# channel downconverted 3.0 MS/s -> 100 kS/s cs16. CRC-OK gate. Fair head-to-head
# on the same capture (acarsdec fed a channel-limited 12.5 kHz AM WAV): xng 16
# CRC-OK vs acarsdec 3.7 17 clean — comparable (real Korean Air HL8537 traffic).
if [ -f bench/data/acars_100k.cs16 ]; then
  acars=$(count_crc bench/data/acars_100k.cs16 cs16 acars 100000 131500000 131.500)
  check acars_offair "$acars"
else
  echo "skip: bench/data/acars_100k.cs16 not present (release asset)"
fi

exit $fail
