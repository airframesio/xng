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

## Comm-B BDS 3,0 — ACAS active RA (2026-06)

BDS 3,0 (ACAS active Resolution Advisory) MB-field layout is ICAO Annex 10
Vol IV §4.3.8.4.2.4: BDS id (MB 1–8 = 0x30), ARA bits (9–15: issued /
corrective / downward-sense / increased-rate / sense-reversal / altitude-
crossing / positive), ARA-reserved-for-ACAS-III (16–22), RAC bits (23–26:
no-below / -above / -left / -right), RA-terminated (27), multiple-threat
(28), threat-type indicator (29–30) and threat-identity data (31–56:
TTI 1 = 24-bit ICAO; TTI 2 = AC13 altitude + 7-bit range ((n−1)/10 NM) +
6-bit bearing (6(n−1)+3°)). Validity gates (BDS id == 0x30, ARA-reserved
< 48, TTI ≠ reserved 0b11) and field formulas were cross-checked against
the pyModeS `bds30` decoder and its `test_bds_commb` TestBds30* synthetic
payloads — facts/positions only, no code ported. Those bit-exact pyModeS
payloads (every ARA/RAC/TTI shift constant) are vendored as the `decode.rs`
unit tests; AC13 altitude reuses the existing `altitude13` decoder (proven
identical to pyModeS `altcode_to_altitude`). Added to the `bds_infer`
exactly-one-validates candidate set and emitted under `comm_b`.

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

## Target state and status (2026-06)

TC 29 (Target State and Status — BDS 6,2) ME-field bit layout is the
published DO-260B §2.2.3.2.7.1 single-format Target State and Status
message: subtype (ME 5–6), selected-altitude source (8), selected
altitude (9–19, (raw−1)·32 ft), barometric pressure setting (20–28,
800+(raw−1)·0.8 mbar), heading status (29), selected heading (30–38,
raw·360/512°), NACp (39–42), NICbaro (43), SIL (44–45), mode status (46)
gating autopilot (47) / VNAV (48) / altitude-hold (49) / approach (51) /
LNAV (53), and TCAS-operational (52). Bit positions and the field
formulas were cross-checked against the pyModeS `bds62` decoder and its
`test_bds62` golden vector (`8DA05629EA21485CBF3F8CADAEEB` → selected
altitude 16992 ft MCP/FCU, QNH 1012.8 mbar, heading 66.8°, AP/VNAV/LNAV
engaged) — facts/positions only, no code ported; that real vector and its
expected values are vendored as the `decode.rs` unit test. Emitted under
`adsb_status` with `subtype: "target_state"`.
