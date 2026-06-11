# Decode-count benchmark fixtures

Regression gate for demodulator sensitivity: `run.sh` decodes the
fixtures and fails if any count drops below its baseline
(`baselines.json`). CI runs it on every PR (`.github/workflows/bench.yml`).

| fixture | what | where |
|---|---|---|
| `data/modes1.cu8` | ADS-B/Mode S: the canonical dump1090 test capture (antirez/dump1090 `testfiles/modes1.bin`, BSD), 2 MS/s UC8, ~0.18 s of dense 1090 MHz traffic | vendored |
| `data/ais_96k.cs16` | AIS: 5 min at 162.000 MHz (Airspy Mini, Sacramento, 2026-06-11), 6 MS/s → 96 kS/s cs16; ~53 mostly-weak base-station bursts | [release asset](https://github.com/airframesio/xng/releases/tag/bench-fixtures-v1) (115 MB) |

```bash
cargo build --release
gh release download bench-fixtures-v1 -p ais_96k.cs16 -D bench/data/
bench/run.sh
```

Oracle calibration (what the strongest open decoders get on the same
fixtures, methodology, and the history of xng's numbers) lives in
[docs/notes/BENCHMARKS.md](../docs/notes/BENCHMARKS.md). Baselines are
floors for *our* counts — raise them when a demod improvement lands.
