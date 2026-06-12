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

**readsb round (2026-06-12)**: readsb (wiedehopf, built from source,
`--no-fix`) is the strongest ADS-B oracle: **167 unique** on the
2.4 MS/s resample of modes1, vs dump1090-fa's 162 and our 161 (at
2 MS/s). Two improvements followed:

1. **Native 2.4 MS/s support** — the RTL-SDR's best rate, previously
   rejected (integer-samples/µs requirement). The fractional path
   integrates half-µs slots from prefix sums with fractional edges and
   decides bits at interpolated half-bit **centers** (the slot
   integral splits a boundary-straddling sample's energy across both
   halves and flips bits at adverse phases — measured), with four
   sub-sample phase passes merged by bytes+position.
2. Result: **157 unique at 2.4 MS/s** on readsb's own input file
   (94 % of readsb; was 0 — the rate didn't work at all). The 2 MS/s
   path is untouched (gate: 323).

**Round 2 (same day): 157 → 164 (98 % of readsb).** The winning lever
was simply a denser sub-sample pass grid (4 → 8 → 16 passes measured
157 → 163 → 164; asymptote at 16), now effort-gated (live = 2 passes
= 148 unique within a Pi budget; max = 16). Falsified along the way,
with numbers: trimmed-slot bit integrals (152 — edge energy hurts
more than extra energy helps) and per-candidate preamble-contrast
phase refinement (154 — overfits preamble noise). xng also decodes
**5 frames readsb misses**; the remaining 4-frame readsb edge is its
phase-classified bit templates.

**Round 4 falsifications (2026-06-12)**, recorded with numbers:
two-bit syndrome-pair repair for DF17 (known-ICAO-gated) gained zero
frames on either benchmark while halving max-effort throughput
(5.3× → 2.7×) — reverted; in-frame collision rescanning still loses
7 frames even with the sighting-based cache clock (the earlier
attribution was wrong — the harm is false mid-frame DF11/17-shaped
candidates filling the ICAO cache and evicting real aircraft, which
no clock policy fixes; a cache large enough to be pollution-proof
would weaken the overlay-DF trust model). The ADS-B demod stands at
its measured best: 161 @2 MS/s, 164 @2.4 MS/s.

**Round 5 (2026-06-12)**: disagreement-flagged suspect bits
(center-tap vs slot-integral statistics) feeding syndrome-targeted
1/2-bit repair — implemented, measured, **decoded set bit-identical
to baseline** (the suspect-pair mechanism never converted a frame on
this capture). Reverted per the discipline: unmeasurable benefit
doesn't ship. Conclusion of the ADS-B campaign on current evidence:
the demod is at its measured ceiling on these captures; the levers
that could move it further are (a) new bench captures with genuinely
weak/dense traffic (the current ones saturate), and (b) readsb-style
per-phase bit templates, parked at an estimated ≤4-frame payoff.

## Decode CPU (×-realtime, Apple M-series; `bench/cpu.sh`)

| mode | effort | speed | decode recall |
|---|---|---|---|
| adsb | `max` (default for files) | 5.3× | 161 unique (reference) |
| adsb | `live` (default for SDR) | **16.6×** | 156 unique (97 %) |
| ais | full | **8.6×** | 52 frames |
| vdl2 | full | 85× | 44 frames |
| hfdl | full | 283× | 33 frames |

Pi-class hardware runs ~5–8× slower than the bench machine: `live`
effort keeps ADS-B comfortably real-time there; AIS lands ~1.4×.
History: the first measurement caught AIS at 3.0× and ADS-B at an
unusable 2.2× — fixed by caching the GMSK waveform tables, replacing
the trellis' O(n²) path clones with a traceback matrix, stride-2
template hunting with low-metric span skipping (which also *gained* a
frame: 51 → 52), and the `--demod-effort` knob (per-command defaults:
file decode = max, SDR commands = live).

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

## Live-capture authenticity: phantom frames and ICAO confirmation

A 60 s off-air 1090 MHz capture (RTL-SDR, 2.4 MS/s, gain 48, quiet
minute) exposed a real defect the dense benchmark captures had hidden:
xng reported 70 "unique frames" where dump1090-fa and readsb both
reported ~0. The frames had the unmistakable phantom signature — 31
CRC-clean DF17s carrying 31 *distinct* ICAO addresses, each seen
exactly once, scattered across implausible allocation blocks.

The math says this must happen: near-floor candidate gates × 16
sub-sample phase passes over 60 s ≈ 2.3 × 10⁹ CRC trials, and a random
112-bit candidate passes the 24-bit parity with probability 2⁻²⁴ —
~140 expected false DF17s per minute of pure noise. The 0.18 s modes1
fixture expects ~0.4, which is why the benchmarks never showed it.
Worse, false DF11s (only 17 parity bits effectively checked) were
*learning* junk ICAOs into the cache, which then validated junk
address-overlaid DF0/4/5 frames.

Fix: two-sighting ICAO confirmation (the same policy readsb uses for
unreliable sources). A CRC-clean DF17/18/11 whose address has never
been seen is held, not emitted; a second clean frame with the same
address confirms the aircraft, releases the held frame at its original
position, and admits the ICAO to the cache. Random phantoms never
repeat an address (P ≈ 2⁻²⁴ per pair), so they die in the pending
table (capped at 64, age-evicted). Address-overlaid frames already
required a cached ICAO, so they now inherit confirmed-only trust.

Measured cost: zero. modes1 is a single heavily-repeated aircraft —
the gate still reads 323 at 2 MS/s. Measured benefit: the quiet live
capture drops from 70 phantoms to exactly 0, matching both oracles.
