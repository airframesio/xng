# Provenance — xng-mode-iridium

Layer 2 ported from **iridium-toolkit** (https://github.com/muccc/iridium-toolkit,
BSD-2-Clause, muccc) — porting permitted with attribution, which this
file and the crate documentation provide. Ported structures (from
`bitsparser.py` / `bch.py`):

- 24-bit access codes (the differential decode of the 12-symbol UW),
  IMS messaging header, frame classification order (IMS header → IBC
  via BCH(7,3) poly 29 + 2-way deinterleave → IRA via 3-way
  deinterleave + triple ringalert BCH).
- Symbol-pair deinterleavers (bits swapped within each pair, symbols
  read from the end backwards): 2-way 64→2×32, 3-way 96→3×32.
- BCH blocks: 32 bits = 31 BCH (21 data + 10 check, polys
  ringalert 1207 / messaging 1897) + whole-block even parity; repair
  by 1–2 bit flip search.
- 64-bit FILL pattern removal (≤2 bitdiff per half).
- IRA field layout (sat/beam/x/y/z/interval/timeslot/EPI/sub-band +
  42-bit pages terminated by an all-ones page) and the geocentric
  position conversion.

PHY facts (no code) from **gr-iridium** and **iridium-sniffer**
(https://github.com/alphafox02/iridium-sniffer, both GPL-3): 25 000
sym/s DQPSK, 40 kHz channels, tone preamble (16/64 symbols) + UW,
`dqpsk_map = [0,2,3,1]` with MSB-first bit emission, frame length
limits, burst detector parameters. iridium-sniffer's ARCHITECTURE.md
is the single best pipeline reference (and documents the IDA/SBD→ACARS
chain for wave-2 follow-up).

The demodulator itself is original: power-boxcar burst gate, tone-DFT
coarse CFO, and the coherent UW fit (joint timing/phase/CFO weighted
least squares over the 12 known UW symbols) developed for the VDL2 and
HFDL demods in this codebase, plus a decision-directed phase trim
(α=0.2 as in gr-iridium).

Additional ports from iridium-toolkit (BSD-2) for the SBD chain:
the 46-bit LCW permutation + BCH components (polys 29/465/41, the
transmitted-bit-short lcw2), the DA frame block mapping (124-bit
chunks → 2-way deinterleave → BCH(31,20) poly 3545 blocks in
[b4,b2,b3,b1] order), DA field layout + CRC-CCITT placement, the IDA
fragment reassembly rules (counter continuity, expiry) and the SBD
transport framing (0x0600/0x76xx types, prehdr variants, 0x10
len/count header, multi-message merge) from
iridiumtk/reassembler/{ida,sbd}.py. The ACARS payload is a standard
SOH-prefixed parity ACARS block, parsed by xng-acars and emitted as a
first-class ACARS message.

Broadcast-time (`iri_time` / `tmsi_expiry`) conversion follows
iridium-toolkit `util.fmt_iritime` (90 ms ticks, the two ERA2-window
leap seconds 2015-06-30 / 2016-12-31) but extends it for the network's
periodic **re-epoch** (L-Band Frame Number reset), which the stock
toolkit does not handle: the counter restarts near zero at each re-epoch
(ERA1 2007-03-08, ERA2 2014-05-11, ERA3 2025-02-14, ERA4 2026-01-14
18:08 UTC per the MetOcean technical bulletin / 2026 security analysis),
so `ira::iri_time_unix` selects the era in force at the frame's
wall-clock receive time. Without this, every post-2025 frame decoded
~11 years into the past. The ERA2 path remains bit-identical to
`fmt_iritime` (pinned in `ira::time_tests` against toolkit-generated
values and in the off-air `tmsi_expiry` oracle).

## Validation

- **Oracle-validated against iridium-toolkit**: a generated ring-alert
  frame decodes bit-identically in `bitsparser.py` (the reference
  implementation) and in this crate — sat/beam/xyz/position/interval/
  flags/sub-band/TMSIs all agree. The exact vector is vendored in
  tests/e2e.rs. (Interop note: gr-iridium `RAW:` lines carry each
  symbol's two bits swapped; `RWA:` is the normalized form this crate
  uses internally.)
- RF loopback: tone+UW+payload burst with CFO and noise through the
  full chain (burst gate, tone CFO, UW fit, DQPSK, deinterleave, BCH,
  field parse).
- **PHY cross-validated against gr-iridium**: the reference
  implementation's own generated test burst
  (test-data/prbs15-2M-20dB.sigmf-data, a synthetic frame with PRBS15
  payload at 20 dB channel SNR ≈ 3 dB full-band) demodulates
  bit-perfectly — access code recognized, zero PRBS15 recurrence
  violations across the whole payload. The DDC'd burst is vendored as a
  CI fixture (tests/data/, 32 KB) guarded by tests/crossval.rs. With
  the toolkit oracle covering layer 2, both layers are validated
  against their reference implementations.
- **DA layer also oracle-validated**: a generated ft==2 burst decodes
  in bitsparser as `IDA: cont=0 ctr=000 len=20 [..] CRC:OK` with every
  byte identical to our decode (LCW encode, DA block mapping,
  BCH(31,20), CRC all agree); vector vendored in tests.
- Off-air validation pending an L-band capture of the ring-alert
  channel (1626.270833 MHz; bursts every few seconds worldwide).

## Wideband front end (2026-06)

`wideband::IridiumWideband` hunts bursts across a whole capture
(gr-iridium's architecture, facts from iridium-sniffer's
ARCHITECTURE.md): per-frame FFT (~1 kHz bins, Blackman window), per-bin
noise floor as a slow symmetric EMA (a 512-frame-average equivalent —
an asymmetric min-tracker sits far below the mean of exponentially
distributed noise bins and fires constantly), 16 dB threshold,
contiguous hot-bin grouping with small-gap bridging, multi-frame burst
tracking, duplicate suppression for leakage-split detections, and
per-burst downmix (mix + boxcar decimate) into the existing 250 kHz
single-channel demodulator. Reported burst frequency = detection
centroid + the demod's fitted CFO. The demod's coarse tone scan covers
±30 kHz so leakage-skewed detection centroids still pull in.

Validated: three synthetic IRA bursts at −700/+123/+651 kHz in a 2 MHz
band all decode with correct satellite ids and offsets; gr-iridium's
real reference burst, re-upconverted to an arbitrary offset in a 2 MHz
band, is found and demodulates bit-perfectly (PRBS15) through the
wideband path.

## IMS pager + duplex traffic classes (2026-06)

IMS messaging decode (header/group/trailer block structure, odd-bit
stream, LSB-first RIC, format 5 ASCII with the 1023-mod-1024 block
checksum and multi-part counters, format 3 BCD) ported from BSD-licensed
iridium-toolkit bitsparser.py (IridiumMSMessage family; ref. US
5,596,315), validated by oracle: a synthetic page generated by our TX
helpers parses field-identically in bitsparser.py
(IridiumMessagingAscii, ric/fmt/seq/text), vendored as a CI vector.
Multi-part page reassembly is original (keyed by RIC, ctr/ctr_max).
LCW frame-type mapping for the duplex traffic classes (0 voice / 1 IP
data / 7 sync) is from the same toolkit source; voice payloads are
extracted as AMBE candidate bytes only — the codec is proprietary and
decoding it is out of scope (iridium-toolkit likewise defers to an
external ir77_ambe decoder). ITL time/location frames remain undecoded:
the toolkit's itl.py is orbital-mechanics tooling rather than a frame
parser, deferred.
