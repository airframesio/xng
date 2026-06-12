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

**Update (2026-06-11): two levers shipped, 116 → 145 unique (~90 %
of FA).** Ablation-attributed: (1) candidate gate relaxation
(PULSE_QUIET_RATIO 2.0 → 1.0, noise gate 3σ → 2σ — the CRC layer
arbitrates, so strict pre-gates only cost frames): +15 unique;
(2) a half-sample-shifted timing grid (midpoint complex
interpolation) scanned independently and merged by bytes+position:
+14 unique. Bursts landing between samples split pulse energy across
half-µs slots — the second phase grid is the 2 MS/s equivalent of
dump1090-fa's sub-phase handling. Trap documented: REPLACING the
on-grid stream with an interpolated one loses frames (−35; the
midpoint samples blur pulse/quiet contrast) — the phases must be
scanned independently and unioned. xng now also decodes 7 frames FA
misses (all from the dominant aircraft, 3 CRC-clean DF17). Remaining
FA-only clean DF17s: 20.

The funnel is in place — rerun:

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

**Update (same day): coherent burst path shipped.** A parallel
weak-signal demodulator (power gate → preamble+flag template anchor →
fine CFO from the template phase slope → 8-state MSK phase-trellis
Viterbi with decision-directed phase tracking) raised the synthetic
sensitivity by **+11–12 dB** (discriminator dies at ~12 dB SNR;
coherent path runs 40/40 at 6.2 dB and 30/40 at 0.9 dB) and the
off-air capture from 6 to **24 unique payloads** (45 % of
AIS-catcher, still a clean subset). **Fractional-timing refinement** (sub-sample template offsets,
window resampled at the winner) followed: 24 → 26 unique payloads
(49 % of AIS-catcher), bench fixture 36 → 39 frames.

**GMSK-exact MLSE (same day): 26 → 36 unique (68 %), fixture
39 → 51 frames.** The trellis is now 16-state
(phase-quadrant × two in-flight levels) with branch waveforms
synthesized from the true BT=0.4 Gaussian phase pulse; the anchor
template uses the same synthesis; both GMSK and MSK pulse hypotheses
run per anchored burst with the FCS arbitrating (real transmitters
vary). Synthetic: 40/40 at 3.7 dB SNR (vs 39/40 for the MSK trellis).
Boundary lesson: the trellis must start on the *known* last template
bit (anchoring quadrant and both in-flight levels) and drop its
emission — seeding at payload bit 0 leaves the first decision
unanchored. Process lesson re-learned at cost: `cargo build -p <crate>`
does NOT rebuild the workspace binary — three off-air "regressions"
were a stale `target/release/xng`.

**Collision decode (same day)**: AIS successive interference
cancellation shipped — a confirmed FCS-valid burst is reconstructed
exactly (the bits are known; the synthesis is the modulator's),
least-squares scaled with its own CFO estimate, subtracted, and the
residual re-hunted for a colliding burst. **No gain on this fixture**
(the missing 17 never anchor at any threshold — they are the genuinely
weakest tail, not collisions), but the machinery is in place for dense
traffic. ADS-B in-frame collision scanning was tried and **falsified**
on modes1 (−7 unique: mid-frame false DF11 candidates pollute the ICAO
cache; the cache clock was moved from attempts to sightings as a
lasting fix). The remaining gaps — AIS 17, ADS-B 8 — are deep-weak
sensitivity and need either better front-end SNR or per-burst
iterative refinement, not collision handling.

Hard-won implementation notes: the template-correlation anchor MUST
reject peaks near the search-window edge (the rising shoulder of a
burst still entering the window anchors mistimed and then skips the
real peak), the hunt cursor must never advance past a region whose
template window isn't fully buffered (chunk-boundary bursts are
otherwise silently skipped — same hazard class as the VDL2
stream-end swallow), and a coarse CFO grid alone is fatal (±75 Hz
residual ≈ 2 rad of drift across one burst; the fine estimate +
decision-directed trim are both load-bearing).

Reproduce:

```
xng decode ais_6m.cs16 -f cs16 -m ais -r 6000000 -c 162000000 --channels 161.975,162.025
AIS-catcher -r CS16 ais_6m.cs16 -s 6000000 -n
```

## Standing results elsewhere

- VDL2: **44 frames vs dumpvdl2's 41** on the sigidwiki capture —
  the long-standing 19/41 gap was a single bit-order bug in RS symbol
  assembly (MSB vs HDLC's LSB-first), found via octet-level ground
  truth from dumpvdl2's debug output; full story in
  docs/notes/VDL2-DEMOD-V2.md round 6. Bench fixture + floor (42)
  added.
- HFDL (forensic round 2026-06-12, VDL2 methodology): **33 events vs
  dumphfdl's 37** on the 21931 kHz capture — frame-exact diff shows
  the 13 data LPDUs match the oracle one-for-one; the missing 4 are
  the weakest bursts (4.0–5.0 dB SNR at 300 bps), a genuine
  sensitivity tail, NOT a convention bug. Parser-policy fixes from the
  round: no CRC-valid LPDU is ever silently dropped (unparsable
  HFNPDUs emit an envelope event) and 0x4F is correctly labeled
  logon-resume. Fixture + floor (31) added to the bench gate.
  Earlier: +4.5–5 dB synthetic sensitivity from the selectivity
  filter (PR #77).
- Iridium/STD-C/Aero: oracle-validated field-exact; no count-style
  sensitivity comparison run yet

