# Off-air decode benchmarks vs oracle decoders

Method: one capture, each decoder fed its native preferred rate/format
(resampled with scipy `resample_poly` where needed), unique frames
compared. Counts are unique raw frames unless noted.

## ADS-B / Mode S — vs dump1090-fa 11.0 (2026-06-11)

Capture: `modes1.bin` — the canonical dump1090 test capture
(antirez/dump1090 testfiles; 2.0 MS/s UC8, ~0.18 s, dense traffic).
xng decodes it natively at 2 MS/s; dump1090-fa got the same capture
resampled 2.0 → 2.4 MS/s (×6/5, SC16).

| decoder | unique frames | notes |
|---|---|---|
| dump1090-fa (`--no-fix`) | 162 | error correction disabled |
| xng v0.12.0 | 116 | 113 common with FA + 3 only-xng |

The gap is real demod sensitivity, not framing: **42 of the 49
FA-only frames are CRC-clean DF17** (the rest are address-overlaid
DF0/11/20/21 accepted via FA's ICAO cache). dump1090-fa's 2.4 MS/s
phase-correlation demodulator out-pulls our magnitude PPM demod on
weak/overlapped bursts. xng currently recovers ~72 % of FA's haul on
this capture.

Next lever (separate workstream): phase-aware PPM demod and/or
2.4 MS/s-style fractional sampling. The funnel is in place — rerun:

```
xng decode modes1.cu8 -f cu8 -m adsb -r 2000000 -c 1090000000 --channels 1090
dump1090 --ifile modes1_24m.sc16 --iformat SC16 --no-fix --raw
```

Local 1090 capture attempts (Airspy Mini, 90 s, gains 0/17): zero
frames from BOTH xng and dump1090-fa — the bench antenna is a VHF
whip, deaf at L-band. A 1090 antenna is needed for live-air ADS-B
benchmarks; until then modes1.bin is the reference.

## AIS — vs AIS-catcher (built from main, 2026-06-11)

Capture: 5 min at 162.000 MHz / 6 MS/s on the Airspy Mini + VHF whip
(Sacramento — inland; the deep-water channel is ~15 mi west). Live
traffic present: mostly type-4 base-station reports every ~10 s from
distant stations, i.e. predominantly weak signals — a sensitivity
test by construction.

| decoder | unique payloads |
|---|---|
| AIS-catcher (default model, same CS16 file at 6 MS/s) | 53 |
| xng v0.12.0 | 6 |

xng's 6 are a clean subset of AIS-catcher's 53 (nothing xng-only):
correctness is fine, sensitivity is the whole gap. AIS-catcher's
coherent demodulator (per-burst phase tracking) pulls weak bursts our
FM-discriminator GMSK path cannot. Recovering ~11 % of the oracle's
haul makes this the largest measured demod gap in xng — larger than
ADS-B (72 %) and VDL2 (~46 % off-air).

Reproduce:

```
xng decode ais_6m.cs16 -f cs16 -m ais -r 6000000 -c 162000000 --channels 161.975,162.025
AIS-catcher -r CS16 ais_6m.cs16 -s 6000000 -n
```

## Standing results elsewhere

- VDL2: 19/41 frames vs dumpvdl2 on the sigidwiki capture
  (docs/notes/VDL2-DEMOD-V2.md — receive-filter hypothesis falsified,
  remaining gap is in-band symbol quality)
- HFDL: 33 events at every rate, vs 37-ish for dumphfdl on the
  21931 kHz capture; +4.5–5 dB synthetic sensitivity from the
  selectivity filter (PR #77)
- Iridium/STD-C/Aero: oracle-validated field-exact; no count-style
  sensitivity comparison run yet

