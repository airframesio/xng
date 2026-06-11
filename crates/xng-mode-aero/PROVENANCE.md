# Provenance — xng-mode-aero

Ported from **JAERO** (https://github.com/jontio/JAERO), MIT license,
Copyright (c) Jonathan Olds — porting permitted with attribution, which
this file and the crate documentation provide. JAERO is the only open
implementation of Inmarsat Classic Aero.

Ported facts/structures (from `aerol.cpp/.h`, `mskdemodulator.cpp`,
`jconvolutionalcodec.cpp`):

- P-channel frame: UW 0xE15AE893 (32 bits, MSB-first) + 16-bit header +
  1152 coded bits = 1200-bit frames.
- Interleaver: 64 rows × N columns (6 at 600 bps, 9 at 1200 bps), row
  visit order (27·i) mod 64, column-major readout.
- Convolutional code: K=7 rate 1/2, polynomials 0o171/0o133 (JAERO passes
  the bit-reversed libcorrect forms 109/79), continuous across frames
  (decoded here with a 62-coded-bit overlap carry).
- Scrambler: the same 15-stage LFSR as VDL2 (x^15+x+1, shared in
  xng-dsp::scramble), applied to the *decoded* bits, reset at each UW.
- Signal Units: 12 bytes, CRC-16/X-25 over the first 10 (little-endian
  trailer; the all-zero SU is accepted); SU type table; ISU 0x71 + SSU
  0xC0 reassembly keyed by AESID/GESID/QNO/REFNO with SEQNO countdown and
  NOOCTLESTINLASTSSU tail handling.
- ACARS carriage: reassembled user data = FF FF + standard SOH-prefixed
  ACARS block (parity-bearing characters, BCS, DEL) — parsed by
  xng-acars::block; multi-block defragmentation matches on
  registration/label/mode/AES/GES with alphabetically incrementing block
  ids.

10.5 kbps OQPSK demodulator (`oqpsk.rs::OqpskDemod`) ported from
`oqpskdemodulator.cpp` + `coarsefreqestimate.cpp`:

- RRC(β=1) matched filter at 48 kHz; AGC with 2.84 clip.
- Non-data-aided square-law symbol timing: 1-sample power differentiator,
  T/4+T/4 delay-difference detector, narrow 10 500 Hz IIR resonator
  (JAERO's 48 kHz coefficients), quadrature phase detector against a
  strobed timing oscillator (±0.1 Hz pull) — the clock acquires
  independently of the carrier.
- Strobes at 10 500/s alternate rails; consecutive strobes pair into
  de-offset QPSK points; carrier tracked by JAERO's "BPSK 2x" tanh
  cross-product discriminator `tanh(I_d)·Q_d − tanh(Q)·I` through the
  2nd-order loop filter (48 kHz coefficients), with the slow
  moving-average bias rotation.
- Coarse CFO: squaring OQPSK yields spectral lines at 2f0 ± 5250 Hz; a
  two-tone matched search over the smoothed 2^14 spectrum of the squared
  signal locates 2f0 (JAERO folds the same spectrum against
  `expectedpeakbin = fb/2`). Applied only while unlocked; lock = low
  constellation MSE *and* a stationary 4th-power statistic (a spinning
  constellation has deceptively low MSE).
- Sign note: the discriminator slope w.r.t. constellation rotation is
  negative once the off-rail (transitional) component statistics are
  taken into account; the correction signs in this port reflect that and
  are verified by the locks_and_demodulates_with_cfo test (BER 0 at
  CFO 0/±120/−250 Hz).

Divergence from JAERO (documented intentionally):

- 600/1200 bps demodulator: JAERO uses a coherent OQPSK-decomposition MSK
  demod with FFT square-law coarse AFC; xng v1 uses a
  frequency-discriminator MSK demod with offset tracking (simpler, ~2 dB
  less sensitive; the differential encoding makes discriminator output
  the data bits directly). Coherent upgrade is a planned improvement.
- No AFC of the channel center / DCD interplay (JAERO's
  FreqOffsetEstimateSlot state machine); xng channels are DDC-tuned and
  the unlocked-only coarse correction covers reacquisition.
- Per-frame Viterbi with overlap instead of JAERO's streaming
  libcorrect decode (equivalent output, simpler state).

Off-air conventions (established against JAERO's real recordings,
2026-06; these are invisible to synthetic loopback because a matched
modulator/demodulator pair cancels them):

- A-BPSK data maps **directly** onto the deviation sign (bit 1 = +90°
  phase advance); there is no differential layer. The UW appears in true
  polarity at 1200-bit spacing in the discriminator's bit stream.
- The coded pair order on air is **(0o133 output, 0o171 output)** per
  data bit — libcorrect's 109/79 polynomial order in JAERO. With this
  order the off-air frames decode with zero Viterbi residual and all SU
  CRCs pass; with 171-first they are pseudorandom.
- Frame layout, 64×6 per-384-bit-block deinterleaving, the shared LFSR15
  scrambler reset per frame, LSB-first packing, and the X-25 SU CRC are
  all confirmed exactly as implemented.

Off-air validation results (JAERO samples):

- `600bps_sample.ogg` (78 s, carrier ~1066 Hz): 11 CRC-valid ACARS from
  real traffic (B-16333 METAR uplink, HL8217 ADS, B-HNF CPA509 PDC
  clearance, B-LIC, 37981S).
- `10.5k_sample.ogg` (240 s, carrier ~5761 Hz, resampled 44.1→48 kHz):
  188 events / 144 CRC-valid ACARS through the OQPSK demod (A7-AEE
  CPDLC AT1 among them).
- A 12 s slice of the 600 bps recording is vendored as a CI fixture
  (tests/data/, attributed) and guarded by tests/offair.rs.

Conformance anchors: JAERO ships real off-air samples
(`samples/600bps_sample.ogg` etc.) usable for cross-validation; loopback
tests here exercise the full chain bit-exactly.

10.5 kbps A-QPSK status: the framing layer (dual-rail UW with per-rail
inversion hypotheses, 16+178-bit header/dummy skip, 64x78 interleaver,
shared Viterbi/descrambler/SU path) is implemented and bit-level tested;
the coherent OQPSK demodulator does not yet achieve carrier lock and its
RF loopback tests are #[ignore]d pending a focused demod session
(JAERO's tanh cross-product loop is the port reference).

C-channel (8 400 bps OQPSK voice circuits) ported from
`aerol.cpp::DecodeC` + `oqpskdemodulator.cpp` (fb==8400 paths):

- Frame: 112-bit UW (two 52-bit rail patterns, JAERO `setPreamble`
  arguments 216866263330005 / 3012071630031408, each detector trying
  both patterns and complements for the OQPSK ambiguity) + 4096 coded
  bits per ~500 ms superframe.
- FEC: the P-channel K=7 rate-1/2 code punctured 3/4 (depuncture
  inserts a neutral bit after every 3rd, last source bit dropped);
  interleaving 16 × (64×4) blocks with the (27·i) mod 64 row permute;
  decoded 2730 → first 2714 kept; LFSR15 descramble.
- Payload: 25 sub-blocks of 1 + 96 + 12 bits — 96-bit AMBE voice
  frames (12 bytes, surfaced for external decoding; the codec itself
  is proprietary) and 12-bit slices accumulating into 12-byte sub-band
  signal units (CRC-16/X.25), types 0x01 fill / 0x30 call progress
  (AES, GES ids) / 0x60 telephony acknowledge.
- Demod: the ported OQPSK demodulator with RRC β=0.6 and JAERO's
  8 400-specific ~10 Hz timing-resonator coefficients.

Note: JAERO additionally delays decoded bits by 2714−6 before the
descrambler (`dl2`) for off-air scrambler alignment; our loopback is
self-consistent without it, and the alignment question is flagged for
when an off-air C-channel capture is available.
