# VDL Mode 2 — implementation notes

Native VDL Mode 2 demod/decode for xng-mode-vdl2 (v0.20.0). D8PSK at
10 500 sym/s, Annex 10 Vol III. Clean-room — see PROVENANCE.md; dumpvdl2
is read for facts and used as an off-air oracle only.

Result: 44 AVLC frames on the sigidwiki off-air capture, against
dumpvdl2 2.6.0's 41, identical at 50 / 100 / 105 kS/s. CI bench floor
`vdl2_offair >= 42` (bench/baselines.json) runs the full capture at
105 kS/s; the vendored 6 s fixture asserts the chain end-to-end.

## Pipeline

Per channel: wideband IQ → DDC (or selectivity FIR on the no-DDC path) →
channel IQ → `demod::Vdl2Demod` (acquisition, header FEC, deinterleave +
RS) → `avlc::scan` → ACARS-over-AVLC or ATN (X.25 / CLNP). Source:
`crates/xng-mode-vdl2/src/`.

## Channel rate

Auto-selected from the capture rate (`lib.rs::Vdl2ChannelDecoder::new`):

- 105 kS/s when the input divides into it — an exact 10 samples/symbol.
  At 100 kS/s every symbol center lands at a fractional sample and the
  linear interpolator's error becomes decision noise; integer sps
  removes it.
- 100 kS/s when the input divides into it. Every real SDR rate
  (2.4M / 3M / 6M) does, so this is the usual operating point.
- 50 kS/s floor (≈4.76 sps). Also the vendored-fixture path.

Symbol instants are linearly interpolated, so no integer relationship
between channel rate and symbol rate is required. The preamble-fit search
grid is symbol-denominated, not sample-denominated — denominating it in
samples silently shrinks the search window as the rate rises (it once
dropped decodes 16 → 9 when the rate was raised). Grid is ±0.63 symbols,
floored at ±3 samples (the original 50 kS/s width).

## Selectivity FIR (no-DDC path)

When input rate == channel rate and offset is zero (fixture and off-air
harness runs), no DDC runs, so the demod would otherwise see the full
input Nyquist band of noise. A flat-in-band lowpass fills that path
(`lib.rs`): 101 taps, -6 dB point at the symbol rate (Rs = 10.5 kHz) so
it is flat through the RC band edge (±8.4 kHz) and the windowed-sinc
transition sits entirely in the noise-only region. ~2.5-3 dB sensitivity
(`examples/sensitivity.rs`). When a DDC runs its decimation filter
already provides this.

This is NOT a matched filter — see the matched-filter trap below.

## Acquisition: coherent preamble phase-pattern fit

Two stages (`demod.rs`).

Coarse trigger is differential: correlate the 16-symbol unique word as
Δφ products (`uw_correlate`). Differential is CFO-immune, so it fires
regardless of carrier offset. A weak per-symbol energy-consistency gate
kills locks straddling the burst edge (near-zero products against
silence) while tolerating the legitimate phase-transition dips that real
preambles show at low sps. Trigger threshold 0.6.

Fine sync is a coherent fit over the whole preamble (`preamble_fit`).
Over a fine timing grid around the trigger, compare the unwrapped
per-symbol phase trajectory of the 16 UW symbols against the known
cumulative UW phase ramp and fit residual ≈ a + b·k by weighted least
squares (weights = sample energy). The minimum-cost grid point jointly
yields the sync point, the carrier phase (a, absorbed by the differential
decisions) and the per-sample CFO (b, the `dphi` derotation). This uses
all 16 symbols coherently; the differential correlation argument uses
only 15 transitions non-coherently and is far noisier.

The fit cost gates acceptance: `FIT_COST_MAX = 0.25` rad². True preambles
on the off-air capture fit below ~0.11; random data sits above ~0.5. The
low trigger threshold is only safe because of buffer retention (below).

## Symbol decisions

Per-symbol differential D8PSK (`collect`): Δφ of consecutive symbol
centers, derotated by the fitted `theta`, to the nearest π/4 multiple →
inverse Gray triplet, descrambled on the fly. Decision-directed residual
tracking adapts `theta` each symbol (`PHASE_GAIN = 0.1`). The |residual|
at the π/4 grid is kept per symbol as a decision confidence for RS
erasure marking.

Differential beats coherent/absolute detection on real captures: the
sigidwiki signal has oscillator phase wander that differential
cancels and absolute tracking does not (a UW-trained LMS equalizer with
absolute D8PSK decoded 1 frame vs 17). Decision-directed differential
is the only in-burst adaptation; an equalizer's 16-symbol training leaves
the taps part-converged and injects more ISI than it removes at these sps.

## Gated noise-floor estimator

The energy gate's EMA (`hunt`) learns the floor only from samples below
the gate, with a tiny up-creep for re-convergence. Learning from burst
power would inflate the floor for ~0.1 s and shadow rapid back-to-back
transmissions — exactly the XID/ack exchanges in the capture. Gating it
took 17 → 19+ frames and made the count flat across `ENERGY_FACTOR` 8-20
and trigger threshold 0.4-0.6, where the symmetric estimator wobbled.

## Buffer retention / rewind (makes a low trigger safe)

A false UW lock can pass the 25-bit header FEC with a bogus length and
"collect" through a real burst. The buffer is retained back to the
collecting burst's UW start; on RS failure the hunt rewinds to
`uw_start + 1`, so any real burst inside the consumed span is still
buffered and gets retried. This is what makes lowering the trigger
threshold safe: a false header decode that fails RS rewinds without
swallowing a real burst. Worst case (max TL) this holds ~150 KB at
50 kS/s.

Two guards in demod acceptance:

- Bogus-length cap: header lengths above 16 000 bits are rejected
  outright (false locks have passed FEC with absurd lengths — up to
  3.4 s of "collection").
- A burst that already failed RS is deterministic; a re-detection that
  refines to the same UW position is skipped past, not retried (it would
  livelock until the noise floor rises).

## FEC: RS(255,249) and the octet convention

`interleave.rs`. Data octets fill a c-row × 255-column table row-major
(c = ⌈TL/1992⌉; short final row virtually zero-filled to 249 data
octets). RS(255,249) checks occupy columns 250-255 with shortening: rows
of ≤2 octets transmit no checks, 3-30 transmit 2, 31-67 transmit 4, ≥68
all 6. Transmission reads column-by-column, skipping virtual fill and
untransmitted checks.

The decisive fix: RS is computed over octets assembled **LSB-first (HDLC
wire order)**. MSB-first packing hands the RS stage bit-reversed symbols
and it rejects perfect codewords. This single-line convention was the
entire 19 → 44 gap.

## Soft-decision RS erasures

`deinterleave_soft`. Each RS row is tried as-is; on failure, retry once
erasing the two least-confident transmitted octets (per-symbol |residual|
mapped to per-octet worst-bit confidence; flagged octets must have
residual > 0.20 rad). RS trades one error of budget for two erasures, so
2·errors + erasures ≤ 6 with untransmitted checks already consuming part
of the budget — the one rung of two erasures keeps a two-error margin.

Erasure-assisted decodes rewind the cursor like a failure (do NOT advance
the hunt past the burst) so a miscorrection cannot swallow a later burst.
This guard was earned: an unbounded erasure ladder "decoded" every burst
(rs_fail 43 → 0) while real frames DROPPED 17 → 10 from cursor skips over
hallucinated codewords. The machinery is free on the happy path and
regression-proof by construction; on the sigidwiki capture (24-30 dB SNR)
it yields nothing the FCS accepts — that capture's losses were never FEC
headroom.

## Standing lessons

**The self-consistent-loopback trap.** The 19 → 44 gap was a single
MSB-vs-LSB-first RS symbol-assembly bug. Every synthetic loopback passed
it because encode and decode shared the same wrong convention. It was
caught only by octet-level ground truth from dumpvdl2's own
`--debug burst_detail` output (post-deinterleave Data+FEC octets), which
showed zero differing octets — the demodulator had been bit-perfect on
the "failing" bursts all along. Rule: demand oracle ground truth at the
octet level, never frame counts or derived files; and validate any
claimed truth file with `avlc::scan` before trusting it (an earlier
round drew conclusions from `/tmp` `.bits` files that were our own failed
post-RS output, containing zero FCS-valid frames).

**The matched-filter trap.** A naive RRC(α=0.6) receive filter passes
every synthetic loopback and collapses off-air decode to ~1 frame (with
RS-passing bursts full of AVLC-invalid bytes). The Annex 10 TX pulse is
full raised-cosine — Nyquist by itself — so the noise-optimal zero-ISI RX
filter is flat in-band and zero outside (a plain lowpass past the band
edge), NOT an RRC; an RRC creates RC^1.5 ISI at the sampling instants.
Critically the lowpass -6 dB point must sit beyond the RC band edge —
cutting at 8.5 kHz eats the outer rolloff and breaks the Nyquist
property. Synthetic loopback is blind to this because the original test
modulator did not shape pulses; it now does, via `burst_iq_shaped`
(RC α=0.6, linear modulation), so loopback covers the realistic waveform.

**RS pass is weak evidence on short rows.** Rows of 3-30 data octets
carry 2 check octets (correct one error) — an RS pass there is ~0.4%
likely on random data. The AVLC FCS is load-bearing: never emit a frame
or change control flow on an RS pass alone. Symbol-offset and clock-skew
re-walks "rescued" bursts that were all RS-passing garbage with zero
0x7E flags; every one was rejected by the FCS.

**Buffer retention is what makes a low trigger safe.** Rewinding to the
collecting burst's UW start means a false header decode that fails RS
cannot consume a real burst. Lower the trigger threshold only with this
in place.

## Architecture vs dumpvdl2

dumpvdl2 runs at 10 sps, does coherent preamble phase-pattern sync
(`pr_phase[]` cumulative expected phases; picks the sample where the
error vector is most constant, whose constant value is the carrier
phase), and carries an explicit per-sample CFO (`dphi`) across bursts.
xng's `preamble_fit` is the least-squares form of the same idea — joint
(sync, carrier phase, CFO) over the full preamble — and the same approach
(coherent preamble fit + no-DDC selectivity FIR) applies to HFDL's A1
acquisition. dumpvdl2 does NOT use a matched filter either; its symbol
decisions are single-sample `atan2` phases, like xng's.
