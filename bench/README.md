# Decode-count benchmark fixtures

Regression gate for demodulator sensitivity: `run.sh` decodes the
fixtures and fails if any count drops below its baseline
(`baselines.json`). CI runs it on every PR (`.github/workflows/bench.yml`).

| fixture | what | where |
|---|---|---|
| `data/modes1.cu8` | ADS-B/Mode S: the canonical dump1090 test capture (antirez/dump1090 `testfiles/modes1.bin`, BSD), 2 MS/s UC8, ~0.18 s of dense 1090 MHz traffic | vendored |
| `data/ais_96k.cs16` | AIS: 5 min at 162.000 MHz (Airspy Mini, Sacramento, 2026-06-11), 6 MS/s → 96 kS/s cs16; ~53 mostly-weak base-station bursts | [release asset](https://github.com/airframesio/xng/releases/tag/bench-fixtures-v1) (115 MB) |
| `data/adsb_quiet_24m.cu8` | ADS-B **false-positive ceiling**: 20 s of quiet live 1090 MHz RF (RTL-SDR, Sacramento, 2026-06-12), 2.4 MS/s UC8. Near-floor gates produce ~70 phantom CRC-clean frames/min on noise without the two-sighting ICAO confirmation; the gate fails if the count *exceeds* `adsb_quiet_max` | [release asset](https://github.com/airframesio/xng/releases/tag/bench-fixtures-v1) (96 MB) |
| `data/vdl2_opflasher_105k.cs16` | VDL2 (second RF path): Opflasher off-air capture, 2.5 MS/s downconverted to 105 kS/s cs16 single channel at 136.800 MHz (real Air NZ ZK-NNB ADS-C-over-VDL2); xng 13 vs dumpvdl2 2.6.0 12 on the same fixture | [release asset](https://github.com/airframesio/xng/releases/tag/bench-fixtures-v1) |
| `data/acars_100k.cs16` | ACARS (POA): Opflasher off-air capture, the single active POA channel at 131.500 MHz downconverted 3.0 MS/s → 100 kS/s cs16 (real Korean Air HL8537 traffic). Gated on **CRC-OK** frames only (`count_crc`) | [release asset](https://github.com/airframesio/xng/releases/tag/bench-fixtures-v1) |
| `data/sonde_96k.cf32` | Radiosonde (RS41): the projecthorus/radiosonde_auto_rx decoder-performance sample (serial N3920808, Adelaide AU, 2019-02-10), 96 kS/s cf32, 120 s. **Oracle-anchored: 119/119 frames vs rs1729/RS `rs41mod`** | [release asset](https://github.com/airframesio/xng/releases/tag/bench-fixtures-v1) (88 MB) |
| `data/navtex_62500.cs16` | NAVTEX (SITOR-B): SDRplay official `navtex.zip` IQ demo (USCG, 2020-09-04), 62.5 kS/s cs16, center 516 kHz / channel 518 kHz. Exercises the narrow-passband DDC; decodes the real USCG message (matches the fldigi/YaND ground truth) | [release asset](https://github.com/airframesio/xng/releases/tag/bench-fixtures-v1) (74 MB) |
| `data/sarsat_37500.cs16` | COSPAS-SARSAT 406 EPIRB: a real off-air beacon burst (sigidwiki `Epirbsignal.zip`), 37.5 kS/s cs16, ~0.5 s. Vendored real-IQ fixture; **0 frames today** — the inverted biphase-L polarity now syncs, but a clean decode of this weak/drifting capture also needs a decision-directed carrier PLL (follow-up), so it is not yet count-gated | vendored |

```bash
cargo build --release
gh release download bench-fixtures-v1 -p ais_96k.cs16 -p vdl2_105k_conj.s16 -p hfdl_48k.cs16 -p adsb_quiet_24m.cu8 -p acars_100k.cs16 -p vdl2_opflasher_105k.cs16 -p sonde_96k.cf32 -p navtex_62500.cs16 -D bench/data/
bench/run.sh
```

`cpu.sh` measures decode speed (×-realtime) per mode on the same
fixtures. Aero, STD-C, and Iridium are fenced separately: their
vendored `tests/data` fixtures assert exact decode results in
`cargo test` on every CI run.

Oracle calibration (what the strongest open decoders get on the same
fixtures, methodology, and the history of xng's numbers) lives in
[docs/notes/BENCHMARKS.md](../docs/notes/BENCHMARKS.md). Baselines are
floors for *our* counts — raise them when a demod improvement lands.
Keys ending in `_max` are ceilings (false-positive gates) instead.
