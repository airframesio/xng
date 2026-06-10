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
- Off-air validation pending an L-band capture of the ring-alert
  channel (1626.270833 MHz; bursts every few seconds worldwide).
