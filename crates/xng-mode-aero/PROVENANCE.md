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

Divergence from JAERO (documented intentionally):

- Demodulator: JAERO uses a coherent OQPSK-decomposition MSK demod with
  FFT square-law coarse AFC; xng v1 uses a frequency-discriminator MSK
  demod with offset tracking (simpler, ~2 dB less sensitive; the
  differential encoding makes discriminator output the data bits
  directly). Coherent upgrade is a planned improvement.
- Per-frame Viterbi with overlap instead of JAERO's streaming
  libcorrect decode (equivalent output, simpler state).

Conformance anchors: JAERO ships real off-air samples
(`samples/600bps_sample.ogg` etc.) usable for cross-validation; loopback
tests here exercise the full chain bit-exactly.
