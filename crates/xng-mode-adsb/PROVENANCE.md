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

## Comm-B BDS 1,0 / 1,7 — capability registers (2026-06)

BDS 1,0 (Data Link Capability Report, ICAO Doc 9871 Table A-2-16 / Annex
10 Vol IV §3.1.2.6.10.2: config flag, overlay-command, ACAS-operational,
Mode-S subnetwork version, transponder level 5, Mode-S specific services,
uplink/downlink ELM throughput, aircraft-ident / squitter / SIC / GICB
capability, ACAS hybrid / RA / RTCA version, DTE status) and BDS 1,7
(Common Usage GICB Capability Report: a 24-bit map, Table A-2-25, of the
registers the transponder will report) MB-field layouts, validity gates
(BDS id 0x10 + reserved-bits + OVC/subnet heuristic for 1,0; BDS-2,0-bit
mandatory + 32 trailing-zero bits for 1,7), and the capability-map ordering
were cross-checked against the pyModeS `bds10` / `bds17` decoders and their
`test_bds_commb` golden frames (`A800178D10010080F50000D5893C` full field
dict; `A0000638FA81C10000000081A92F` → [0,5 0,6 0,7 0,8 0,9 2,0 4,0 5,0 5,1
5,2 6,0]) — facts/positions only, no code ported; those real vectors are
vendored as `decode.rs` unit tests. `bds_infer` was restructured to the
phased precedence of pyModeS `_infer.py`: a format-ID fast path (BDS 1,0 /
1,7 / 2,0 / 3,0, mutually exclusive, first-match-wins) ahead of the EHS
exactly-one heuristic set (4,0 / 5,0 / 6,0), ahead of the meteo fallback —
which resolves the real BDS 1,7 vs 4,0 collision the old flat
exactly-one rule could not.

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

## Comm-B BDS 4,4 / 4,5 — meteorological registers (2026-06)

BDS 4,4 (Meteorological Routine Air Report, ICAO Doc 9871 Table A-2-33:
FOM, wind speed/direction, static air temperature, pressure, turbulence,
humidity) and BDS 4,5 (Meteorological Hazard Report, Table A-2-32:
turbulence / wind-shear / microburst / icing / wake-vortex levels +
temperature / pressure / radio height) MB-field layouts, the sign-magnitude
temperature convention, the status/value-consistency gates, and the BDS 1,7
disambiguation for 4,5 were cross-checked against the pyModeS `bds44` /
`bds45` decoders and their `test_bds_commb` TestBds44*/TestBds45* vectors
(golden frames `A0001692185BD5CF400000DFC696` → wind 22 kt / 344.5° /
−48.75 °C and `A00004190001FB80000000000000` → −4.5 °C, plus the multi-
field / multi-hazard synthetic payloads) — facts/positions only, no code
ported; those real vectors are vendored as `decode.rs` unit tests. Both are
heuristic registers that collide with the EHS layouts, so — mirroring
pyModeS's `include_meteo` separation — they are NOT in the strict
exactly-one-validates set: `bds_infer` tries them only as a fallback when
the ELS/EHS set is unambiguously empty, leaving existing decoding
unperturbed. Emitted under `comm_b`.

## Mode A/C reply decode (2026-06)

The Mode A/C information-word decode (`mode_ac.rs`) — the 16-bit Mode A
pulse word → 4-digit octal squawk (`word & 0x7777`) + SPI/Ident pulse
(`0x0080`), and the Mode A→Mode C Gillham altitude ladder — uses the
documented dump1090 / readsb pulse layout and `internalModeAToModeC`
algorithm (protocol facts only). For verification the upstream dump1090 C
function was compiled verbatim and run as an independent external oracle to
emit (mode_a → altitude) reference pairs (e.g. 0x0020 → −1000 ft, 0x0320 →
1000 ft, 0x4220 → 5000 ft, 0x5124 → 35000 ft, 0x5424 → 38000 ft, 0x6520 →
10000 ft; 0x1000 / 0x0050 invalid) and squawk/SPI pairs from
`decodeModeAMessage`; those values are vendored as the `mode_ac.rs` unit
tests — a separate authoritative decoder, not an encode→decode loopback.
Only the deterministic decode kernel is implemented; the RF framing-pulse
demodulation (a distinct magnitude-domain signal path) is deferred, so this
module is the decode side a future Mode A/C demod would feed.

## DF18 CF-field source classification (2026-06)

The DF18 Control Field (frame bits 5–7) classification — CF=0 ADS-B
non-transponder, CF=1 ADS-B anonymous/non-ICAO, CF=2 fine TIS-B, CF=3
coarse TIS-B, CF=5 fine TIS-B non-ICAO, CF=6 ADS-R rebroadcast, CF=4/7
unknown format — follows DO-260B §2.2.3.2.1.2 as implemented identically
by the de-facto reference decoders readsb (`wiedehopf/readsb` mode_s.c)
and dump1090-fa (`flightaware/dump1090` mode_s.c): the source/addrtype
mapping in their DF18 CF switch was used as the external reference (facts
only, no code ported). The mapping is pinned by a `decode.rs` unit test
asserting each CF's source/addr-type against that reference. Surfaced by
folding `cf` / `source` / `source_addr_type` / `source_detail` into the
frame's `adsb_status` (merged with any TC28/29/31 status already present),
which the crate serializes to JSON/asf-2.0.

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

## Accuracy / integrity — NUCp / NIC / NACv / SDA (2026-06)

The version-dependent ADS-B quality layer (`decode::nuc_p` / `nic_v1` /
`nic_v2` / `nac_v_hfom_mps` / `position_quality`, plus the new `Velocity`
NACv / VR-source / GNSS-minus-baro fields and the TC31 NIC-supplement-C /
SDA / HRD additions). Lookup-table values and the resolution procedure
are ICAO Annex 10 Vol IV / DO-260A/B as tabulated in pyModeS
`uncertainty.py` (`TC_NUCp_lookup`, `TC_NICv1_lookup`, `TC_NICv2_lookup`,
`NUCp`, `NACv`) and decoded by its `nuc_p` / `nic_v1` / `nic_v2` / `nac_v`
functions; the velocity trailer (NACv at ME 10–12, vertical-rate source
bit 35 = GNSS/baro, GNSS-minus-baro at ME 48–55 `(mag−1)·25 ft`, N/A at 0
or 127) is the pyModeS `bds09` layout; the TC31 operational-status NICb/c
supplement positions (NICa = ME 43, NICc = ME 19) follow pyModeS
`nic_a_c`, and SDA = the low two bits of the 16-bit operational-mode field
(ME 38–39) follows the rs1090 `bds65` `OperationalMode` layout — facts and
table values only, no code ported. Verification (external, not loopback):
the published pyModeS `test_adsb` NIC golden-vector set (twelve frames
`8D3C70A3…`→0 … `8D3C4ACF…`→11, two of them supplement-sensitive) is
vendored as the `nic_v1` unit test; the velocity NACv/VR-source/geo-baro
asserts are pinned to `pyModeS.decoder.bds.bds09.decode_bds09` outputs
(e.g. `8D485020…` → nac_v 0 / GNSS / geo_minus_baro 550 ft; `8d3461cf…`
→ nac_v 1 / baro / 350 ft); the TC31 v2 op-status field positions are
pinned to `bds65.decode_bds65` on a synthetic v2 payload. NUCp emits on
every airborne position frame under `adsb_status.nuc_p`; NACv/VR-source/
geo-baro fold into `adsb_status` on TC19; the version-aware NIC is exposed
by `position_quality` for a caller that pairs a position TC with the
aircraft's last operational-status supplement.
