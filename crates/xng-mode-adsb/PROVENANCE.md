# Provenance — xng-mode-adsb

Clean-room implementation. Sources used (protocol facts and standards text
only; no code from any decoder was read or ported):

- ICAO Annex 10 Vol IV: Mode S downlink format — 1090 MHz PPM at 1 Mbps
  (each 1 µs bit cell: pulse in the first half = 1, second half = 0),
  8 µs preamble with pulses at 0, 1.0, 3.5, 4.5 µs, 56/112-bit frames
  (DF ≥ 16 → 112), CRC-24 generator 0xFFF409, parity field either clean
  (extended squitter PI with II=0) or overlaid with the interrogator /
  aircraft address.
- "The 1090 MHz Riddle" (junzis, open educational reference): worked
  field layouts for extended squitter (TC 1–4 identification charset,
  TC 9–18 12-bit altitude with Q-bit, N·25 − 1000 ft) and the published
  example frames used as test vectors (8D4840D6... → KLM1023 ident;
  8D40621D... → 38000 ft).
- Textbook DSP (magnitude-domain pulse detection).

## Position/velocity depth (2026-06)

CPR decode (global airborne, local airborne/surface, NL function), TC 19
velocity, 13-bit AC altitude (Q-bit + Gillham reorder) and ID-field
squawk follow the ICAO Annex 10 Vol IV procedures as published openly in
"The 1090 Megahertz Riddle" (Junzi Sun, CC BY-SA) — implemented from the
described algorithms, validated against the book's worked examples
(vendored as unit-test vectors: the 40621D CPR pair, ground-speed and
airspeed velocity frames). The per-aircraft tracker (even/odd pairing
within 10 s, local decode against a fix fresher than 180 s) mirrors the
standard surveillance practice; SBS-1 output line format follows the de
facto BaseStation convention as served by dump1090-family tools.

## Operational status / aircraft status (2026-06)

TC 31 (Aircraft Operational Status — BDS 6,5) and TC 28 (Aircraft Status)
ME-field bit layouts are the published DO-260B / "The 1090 MHz Riddle" §6
fields: TC31 subtype (ME 5–7), ADS-B version (40–42), NIC-supplement-A
(43), NACp (44–47), GVA (48–49), SIL (50–51), NICbaro (52), SIL-supplement
(54); TC28 subtype (5–7) with the subtype-1 emergency/priority state
(8–10). Bit positions were cross-checked against the pyModeS `bds65`
decoder and its `test_bds65` synthetic vector (facts/positions only — no
code ported); the synthetic-vector construction is reproduced in
`decode.rs` unit tests. Emitted under `adsb_status` on the Mode S message.
