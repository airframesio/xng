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

## Trigger sensitivity v3 (resolved, 2026-06)

The missing piece was never the discriminator — it was making false
acceptances harmless. The buffer retention policy now keeps samples
back to the collecting burst's UW start, so when a false header decode
with a bogus length fails RS, the rewind to uw_start+1 still has every
sample of any real burst inside the consumed span. With that, the
trigger threshold drops 0.88 → 0.6 with the fit-cost gate (< 0.25 rad²)
arbitrating, and weak bursts get attempted safely: **16 frames** on the
capture (13 before; plateau holds down to thr 0.4). The remaining gap
to dumpvdl2 is genuine SNR reach at 4.76 samples/symbol.

## Channel-rate study (2026-06)

The 4.76 samples/symbol hypothesis tested directly: the sigidwiki
capture (48 kS/s native) resampled to 100 kS/s with a matched ±13 kHz
channel filter and fed to the demod at 9.52 sps. Two findings:

1. The preamble-fit search grid was sample-denominated (±3 samples =
   only ±0.32 symbols at the higher rate, while the differential
   trigger's peak width in samples scales with rate). Naively raising
   the rate DROPPED decodes 16 → 9. Symbol-denominating the grid
   (±0.63 symbols, floor ±3 samples — exactly the old width at
   4.76 sps) restored and then beat the baseline.

2. With the grid fixed: 50 kS/s → 16 frames (unchanged), 100 kS/s →
   17 frames. The decoder now auto-selects 100 kS/s whenever the
   capture rate divides into it (every real SDR rate: 2.4M/3M/6M);
   50 kS/s remains the floor and the vendored-fixture path.

The remaining gap to dumpvdl2 (41 on this capture) is not
sample-rate-bound: next step is demod v3 proper — matched filter +
decision-feedback equalization, the same arc that took HFDL from
19 to 33.

## Demod v3 attempt: UW-trained LMS equalizer (2026-06, reverted)

Instrumented the failure funnel on the off-air capture at 100 kS/s:
fit_pass=125, hdr_fail=26, **rs_fail=43**, burst_ok=25 → decision
quality on accepted bursts is the bottleneck, not acquisition.

Tried HFDL's recipe (7-tap T-spaced LMS trained on the 16-symbol UW,
2nd-order DD carrier loop). Findings, all measured:

1. **Coherent/absolute D8PSK detection loses outright** on this signal:
   per-symbol |phase err| 0.07–0.39 rad against π/8 decision regions —
   the capture has oscillator phase wander that differential detection
   cancels and absolute tracking does not. (Decoded 1 frame vs 17.)
2. Differential-on-equalized works in clean loopback (training residual
   0.001–0.004 rad) but off-air decodes 10–12 vs 17 plain: 16 training
   symbols leave the taps part-converged, and the residual tap noise
   injects ISI that outweighs the equalization gain at 9.5 sps.
   Decision-directed adaptation anchored on the previous output
   (drift-free for DPSK) recovers some (10→12), not enough.
3. Watch the lookahead-at-stream-end edge (k+3 window for the last
   symbols) — it silently stalls collection in loopback tests.

v3 direction that survives these findings: **two-pass decode** — first
pass with the plain demod, then retrain the equalizer on the *entire*
decoded burst (hundreds of known symbols instead of 16) and re-decode;
apply only when pass 1 fails RS. That spends CPU exclusively on the 43
RS failures and cannot regress the 25 already-good bursts. The funnel
counters (demod::STAT_*) are kept for that work.

## Two-pass decode attempt (2026-06, reverted)

Implemented the two-pass blueprint and measured both reference
strategies on the off-air capture:

1. **Pass-1 decisions as training refs**: 3 of 43 RS failures
   "decoded" on the second pass — all three were RS miscorrections
   producing zero FCS-valid AVLC frames. Lesson: with RS at capacity
   on noisy decisions, an RS pass alone is NOT acceptance — the AVLC
   FCS gate is load-bearing, and training on partly-wrong references
   mostly teaches the equalizer the errors.
2. **Row-corrected refs (partial deinterleave)**: engages never —
   the capture's failing bursts are single-RS-row (short AVLC), so
   there are no individually-decoded rows to train from when the
   burst fails. The strategy only helps multi-row (long) bursts.

Conclusion: the remaining 17-vs-41 gap is soft-decision territory —
per-symbol confidence into RS erasure marking (the decoder corrects
2t erasures vs t errors: flagging the 3-4 least-confident octets per
row doubles the correction budget exactly where it is needed), or
confidence-driven re-decision of boundary symbols. That is the v4
direction; the funnel counters remain in place for it.

## v4: soft-decision RS erasures (2026-06, shipped with caveats)

Implemented: per-symbol |residual| confidence → on RS row failure,
retry once with the two least-confident transmitted octets erased
(2e+f ≤ 6 keeps a two-error margin; flagged octets must have residual
> 0.20 rad). Erasure-assisted decodes never advance the hunt cursor
past the burst (rewind, like a failure) so a miscorrection can
never swallow a later burst — that guard was earned the hard way: an
unbounded erasure ladder "decoded" every burst (rs_fail 43 → 0) while
real frames DROPPED 17 → 10 from cursor skips over hallucinated
codewords.

Off-air result: 13 of 43 RS failures now pass RS, **all 13 rejected by
the AVLC FCS** — zero verified gain on this capture. Third independent
confirmation (after the equalizer and two-pass studies) that the
capture's failing bursts are pervasively weak rather than marginal:
the 17-vs-41 gap to dumpvdl2 lives in raw demodulation quality
(filtering/sync ahead of the slicer), not in FEC headroom. The
machinery ships because it is free on the happy path, regression-proof
by construction, observable live (soft_ok counter), and exactly
targets the 4-bad-octet bursts that real reception produces.

## The gate, not the slicer (2026-06): 16-17 → 19 frames

The dumpvdl2 oracle audit flipped the weak-burst theory: 40 of its 41
frames sit at 24-30 dB SNR — the missing frames were STRONG. Two real
culprits found by instrumenting per-burst lengths and SNRs:

1. **False-lock monsters**: bogus headers passing the 25-bit FEC with
   absurd lengths (up to 112k bits = 3.4 s of "collection"). Now capped
   at 16k bits in the demod acceptance.
2. **Noise-floor inflation**: the energy gate's EMA learned from burst
   power during post-rewind rescans, inflating the floor for ~0.1 s and
   shadowing rapid back-to-back transmissions (XID/ack exchanges — the
   exact pattern in the capture). The estimator is now gated: it only
   learns from samples below the gate, with a tiny up-creep for
   re-convergence. Result: 19 frames at every capture rate (50 kS/s:
   16 → 19, 100 kS/s: 17 → 19), and the frame count is flat across
   ENERGY_FACTOR 8-20 and trigger threshold 0.4-0.6 where the old
   estimator wobbled 17-20.

Remaining gap (19 vs 41): the oracle decodes multiple frames from
burst sequences we still partially miss; next instrumentation step is
time-aligning our decoded burst list against dumpvdl2's per-burst
timestamps to see exactly which transmissions remain.

## Failure forensics round 2 (2026-06, all rescue ladders falsified)

With the gated estimator in (19 frames), the remaining oracle gap was
chased through three more measured hypotheses:

- **Content diff vs dumpvdl2**: we decode the short frames (11-octet
  RRs) of conversations whose long I-frames we miss, plus zero of the
  six GSIF broadcast XIDs. Pattern: long bursts fail, short ones pass.
- **Clock-skew re-walk** (±25..100 ppm): zero rescues. Falsified.
- **Symbol-offset re-walk** (±1, ±2 symbols): six "rescues" — every
  one RS-passing garbage with zero FCS-valid frames and no 0x7E flags
  in the bytes. Cause: low-redundancy RS rows (2 check octets ≈ 0.4%
  random-pass odds) pass deterministically once lucky. Falsified, and
  a hazard quantified: **on short/low-k rows an RS pass is weak
  evidence — never emit or change control flow on it without FCS.**
- **Residual trajectories on real failures**: flat head-to-tail (no
  drift), and several failures show GOOD residuals (0.10-0.12 rad
  mean) — the constellation locks while the bits are wrong.

Locked-constellation-with-wrong-bits and a clean residual leaves one
suspect standing: the 25-bit header FEC accepting a near-codeword with
a wrong transmission length (collection length and interleaver layout
both wrong → clean symbols, garbage deinterleave). Next experiment:
on RS failure, ladder over the header codewords within FEC distance
of the received header bits and retry the deinterleave per candidate
length (data is already collected for the longest). FCS-arbitrated as
always.

## Forensics round 3 (2026-06): FEC thinness + the matched-filter trap

Two more hard-won facts:

1. **The FEC is thinnest exactly where the failures live**: rows of
   3-30 data octets carry 2 check octets (corrects ONE error), 31-67
   carry 4 (corrects two). The missing mid-size I-frames and GSIF
   broadcasts land in these bands, where 2-3 scattered symbol errors —
   entirely consistent with the observed 0.10-0.12 rad mean residuals —
   are fatal. dumpvdl2 wins these bursts by making fewer raw symbol
   errors, not by better FEC. (Header-candidate ladder and PLL
   lattice-clamp hypotheses: both falsified by measurement first.)

2. **A naive RRC matched filter is a trap**: inserting RRC(0.6) before
   the slicer passes every synthetic loopback test and collapses
   off-air decode 19 → 1, with the alarming signature of RS-passing
   bursts full of AVLC-invalid bytes. Do NOT retry this without first
   deriving the TX pulse/ISI interaction properly (the Annex 10 pulse
   is full raised-cosine: an RRC receive filter creates RC^1.5 ISI at
   the sampling instants) and validating decisions against a bit-level
   ground-truth burst. The synthetic loopback is blind to this failure
   class because the test modulator does not shape pulses.

Net position after rounds 1-3: 19/41 frames; every cheap hypothesis is
measured and dead; the remaining gap is raw symbol-decision quality on
mid-size bursts, and the next credible lever is a properly-derived
receive filter (matched to the actual RC pulse, ISI-compensated or
zero-forcing at symbol instants) — a half-day design task with the
harness and funnel already in place.

## Round 4 (2026-06): the receive filter, done properly

Protocol followed as planned: pulse-shaped test modulator first, then a
derived filter, bit-level ground truth, off-air last.

1. **Pulse-shaped modulator** (`burst_iq_shaped`): linear D8PSK with the
   Annex 10 full raised-cosine pulse (α=0.6, ±6T support). RC is
   Nyquist, so the existing symbol-center demod decodes it unchanged —
   loopback now covers the realistic waveform at both channel rates.

2. **The derived filter is not a matched filter.** For an RC-shaped TX
   pulse the zero-ISI receive filter family is F(ω) with H·F Nyquist;
   the noise-optimal member that adds no ISI is **flat across the
   signal band, zero outside**: a plain lowpass covering ±8.4 kHz.
   Critically its -6 dB point must sit *beyond* the RC band edge
   (we use Rs = 10.5 kHz, 101 taps) — an early experiment with the
   cutoff at 8.5 kHz ate the outer RC rolloff, broke the Nyquist
   property, and failed loopback exactly as theory predicts.

3. **Where it goes**: the DDC's decimation filter already provides
   channel selectivity (8.5 kHz passband), but the no-DDC path
   (input rate == channel rate, zero offset — all fixture and off-air
   harness runs) had NO filtering at all: the demod saw the full
   input Nyquist band of noise. The selectivity FIR now fills that
   path only.

4. **Measured sensitivity** (shaped burst, 40 trials/point, 50 kS/s):
   ~2.5-3 dB. At 13.5 dB SNR: 32/40 plain vs 40/40 filtered; at
   11.4 dB: 12 vs 34; at 9.7 dB: 1 vs 19. Matches the expected
   noise-bandwidth reduction (25 kHz → ~10.5 kHz equivalent).
   `examples/sensitivity.rs` reproduces the table.

5. **Off-air: 19 frames before, 19 after** (funnel slightly cleaner:
   rs_fail 17→16, hdr_fail 204→199). The sigidwiki capture is
   24-30 dB SNR — out-of-band noise was never its binding constraint.
   **The receive-filter hypothesis for the 19→41 gap is falsified.**
   The remaining gap stays where round 3 left it: in-band raw
   symbol-decision quality on mid-size bursts (good residuals, wrong
   bits), where the falsified-equalizer list already covers the cheap
   in-band levers.

6. **Harness hazard found**: a phantom UW lock in lead-in noise whose
   garbage header passes the thin 25-bit FEC starves collection when
   the test stream simply ends (no hdr_fail, no rs_fail, zero output —
   a silent swallow). Live streams always provide the trailing samples
   that let it fail RS and rewind. Loopback tests now pad 30k trailing
   samples; remember this when a synthetic test inexplicably returns
   zero bursts.

HFDL's no-DDC path has the same missing-selectivity structure and its
own falsified-sensitivity backlog — worth the same experiment there.

## Round 5 (2026-06-11): forensics reset — the pseudo-truth retraction

Two corrections and a sharpened target list:

1. **The `/tmp/vdl2_bits/burst_tl*.bits` files were never ground
   truth.** They contain zero FCS-valid AVLC frames — they are our own
   failed post-RS bits from an earlier round. Any conclusion drawn by
   comparing against them (including this round's first "check-octet
   convention" hypothesis) is invalid. Verification rule going
   forward: a claimed truth file must pass `avlc::scan` before it is
   used as a reference.
2. **105 kS/s native channel support added** (exact 10 samples/symbol)
   to falsify the fractional-interpolation hypothesis: 19 frames at
   105 k, identical to 100 k. Linear-interpolation error is NOT the
   gap. (Kept anyway: integer sps for free when the input divides.)

Real oracle diff (dumpvdl2 built from source, JSON output, same
capture): oracle 41 = 12 x25 + 10 acars + 9 RR + 8 xid + 2 other; we
decode 19, including RRs from every conversation. Missing entirely:
**all six GSIF broadcast XIDs** (2D4917/2D4918 → FFFFFF), 8 of 10
ACARS, and roughly half of the 47806D→10981A x25 exchange. The
stations are audible (their RRs decode); the mid-size frames fail.

RS-failure positions are now dumpable (`VDL2_DUMP_BITS=<dir>`, files
carry the burst sample offset); three GSIF burst IQ segments are
extracted to /tmp/vdl2_lab/ for single-burst lab work. Next: bit-true
single-burst study against dumpvdl2's decode of the same burst —
obtained from dumpvdl2 itself, not from stale files.
