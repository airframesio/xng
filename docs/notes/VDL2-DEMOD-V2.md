# VDL2 demod v2 — design notes for the sensitivity upgrade

Status 2026-06: the native demod decodes 14/18-ish bursts on the
sigidwiki off-air capture (10 AVLC frames; dumpvdl2 gets 41 frames from
the same file, counting multi-frame bursts per frame). Every burst that
passes the header passes RS; the remaining losses are four
ground-station XID bursts whose symbol decisions carry enough errors
that RS at capacity (3 corrections on k=6 rows) miscorrects into a
nearby codeword — dumpvdl2's exact destuffing algorithm fails
identically on our post-RS bits, so the gap is entirely in the
demodulator front end. Phase-gain (0.05–0.1 plateau) and sampling-offset
(0 optimal) sweeps are exhausted.

## What dumpvdl2 actually does differently (facts from src/demod.c)

It does NOT use a matched filter. Its symbol decisions are single-sample
`atan2` phases, like ours. The edges are architectural:

1. **10 samples/symbol** (105 kHz channel rate vs our 50 kHz / 4.76 sps).
   Finer timing grid and more samples to average over the preamble.
2. **Preamble phase-pattern sync**: a buffer of per-sample phases spans
   the whole 16-symbol preamble; sync compares the phase *trajectory*
   against the known UW phase ramp (`pr_phase[]` = cumulative expected
   phases) and picks the sample where the error vector is most constant.
   The constant value of that error vector simultaneously yields the
   carrier phase. This uses all 16 symbols coherently — our differential
   correlation uses 15 transitions non-coherently.
3. **Explicit per-sample CFO (`dphi`)**: estimated from the preamble
   (slope of the error vector), applied as `phi - prev_phi - dphi` in
   every symbol decision, carried across bursts (`prev_dphi`) and
   reported as a ppm error. Our `theta` is per-symbol rotation from the
   differential correlation argument — same idea, noisier estimate, and
   our decision-directed `PHASE_GAIN` residual tracking is the only
   in-burst adaptation.

## v2 plan

- Raise `CHANNEL_RATE` to 105_000 (Ddc from wideband input; the
  sigidwiki capture resamples to 105 kHz losslessly for the test path).
- Replace the differential UW hunt's refinement with the phase-pattern
  sync over the full preamble: coarse trigger can stay differential
  (CFO-immune), then fit phase trajectory for (sync point, carrier
  phase, dphi) jointly.
- Per-symbol decisions as today, but derotated by the fitted dphi;
  keep the decision-directed residual tracking on top.
- Regression assets: `examples/offair.rs` against the conjugated
  sigidwiki capture (expect ≥14 bursts / 10 frames before, target the
  four XID bursts: tl_bits 372, 489, 572, 573 — XID heads
  `F6/F2 FE FE FE 94 AC 48 …` must pass AVLC FCS after the upgrade);
  the vendored 6 s fixture; the full synthetic suite.

Capture regeneration:

```
curl -L https://www.sigidwiki.com/images/d/df/VDL-M2_IQ.zip -o vdl2.zip && unzip vdl2.zip
ffmpeg -i "VDL2 IQ.wav" -f f32le -ac 2 -ar 105000 vdl2_105k.f32
# NOTE: capture has inverted I/Q — negate Q before decoding.
```

The same investigation applies to HFDL's weak-burst gap (31/37 vs
dumphfdl): its A1 hunt is also differential-then-refine; a coherent
preamble fit would sharpen acquisition there too.

## Progress (same day)

The coherent preamble fit is implemented (weighted least-squares over
the 16-symbol phase trajectory, ±3 samples in 0.25 steps) at the
existing 50 kHz channel rate — the rate bump turned out unnecessary for
this step. Result on the capture: **13 frames (from 10)**, including
the GS XID bursts (`XID len=75` from 2D4918) that previously RS-
miscorrected. The remaining gap to dumpvdl2 (41) is burst *detection* —
hunt trigger sensitivity on the weakest bursts — not decode quality.

## Trigger sensitivity investigation (negative results, 2026-06)

Attempted lowering the trigger threshold (0.88 → 0.5) with the fit
residual as the UW arbiter. The cost separation is real and promising —
true preambles fit below ~0.11 rad², random data above ~0.5, and at low
thresholds the fit surfaces real weak bursts the 0.88 trigger never
attempts (e.g. the old RS-failure at sample 406887, cost 0.010). But
making it safe needs an edge/straddle discriminator that none of the
quick heuristics provide:

- Global-noise-floor sample gates reject true preambles (D8PSK
  transition dips legitimately cross any global threshold, and the
  symmetric noise EMA rides up during bursts).
- Front/back half-energy ratio rejects true preambles too: the UW sits
  at burst start, inside the TX power ramp, so the ratio is naturally
  skewed.
- Trigger peak-picking alone doesn't help; false triggers sit farther
  from true UWs than the pick window.

The v3 direction: model the TX ramp (weight the fit by expected ramp
shape, or fit ramp amplitude jointly), or gate on the fit cost of the
SECOND half of the UW only (past the ramp). Each false acceptance is
expensive — a bogus header length drains the buffer past real bursts —
so the discriminator must be strong, not just statistical.
