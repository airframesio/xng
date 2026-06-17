# Provenance — xng-acars

Ported from **libacars** (https://github.com/szpajder/libacars), MIT
license, Copyright (c) 2018-2023 Tomasz Lemiech <szpajder@gmail.com> —
porting is permitted with attribution, which this file and the crate
documentation provide.

Ported pieces and their sources:

- ARINC 622 envelope (`arinc.c`): IMI table (.AT1/.CR1/.CC1/.DR1/.ADS/.DIS),
  7-or-4-char ground station address rules, IMI(3)+air_reg(7)+hex payload
  layout, CRC-16 (poly 0x1021, MSB-first, init 0xFFFF) over the IMI+air_reg
  ASCII plus all binary bytes, validated by residue 0x1D0F.
- ADS-C (`adsc.c`): downlink and uplink tag tables, bit-level field layouts,
  and the exact scaling formulas (coordinate (180−90/2^19)·r/0xFFFFF,
  altitude ×4 ft, timestamp ×0.125 s, heading (180−90/2^10)·r/0x7FF with
  +360 wrap, wind direction (180−90/2^7)·r/0xFF, temperature
  (512−256/2^10)·r/0x7FF, speed ÷2, vertical speed ×16, distance ÷8).
- Media advisory (`media-adv.c`): label SA text format and link codes.
- Sublabel/MFI extraction (`acars.c`): H1 `#xxB`/`- #xx` and `/yy ` rules.

Test fixtures: the four real off-air ADS-C messages embedded in libacars'
`examples/adsc_get_position.c` (MIT), with field values cross-verified by
independent reimplementation before porting.

Differences from libacars: Rust-native types with serde serialization;
CPDLC (AT1/CR1/CC1/DR1) payloads are currently carried as verified raw hex
pending the FANS-1/A ASN.1 PER decoder; MIAM/OHMA not yet ported.

## Reassembly, MIAM, OHMA (2026-06)

Ported from MIT-licensed libacars with attribution:

- `reasm.rs`: multi-block reassembly semantics from reassembly.c +
  acars.c — key (tail, label, msg_num), downlink sequencing via the 4th
  message-number character, uplink sequencing via block id with the
  A..W wrap, the empty-uplink-ACK skip, per-bearer timeouts.
- `miam.rs`: miam.c / miam-core.c — ACARS CF frame map (T/F/K/S/A/Y/X),
  base85 armor ('!' offset, 'z' zero-word), bpad/hpad + '|' framing,
  v1/v2 DATA header layouts, DEFLATE bodies inflated as raw streams
  (windowBits −15 equivalence via miniz_oxide). CRC fields parsed but
  not verified (matches libacars default behavior).
- `ohma.rs`: ohma.c — OHMA/RYKO marker with downlink/uplink routing
  prefixes, the duplicated-first-block quirk workaround, base64 → zlib
  → JSON.

Synthetic roundtrip vectors (compress → render → parse) stand in for
off-air samples until label-MA/OHMA traffic is captured at the live
station.

## FANS-1/A composite arguments (2026-06)

Additional element-argument readers: the two/three-component composites
(PositionAltitude, TimeAltitude, PositionTimeAltitude, ...) compose the
existing verified readers; new scalars take their PER constraints from
libacars's asn1c tables (MIT, as before): VerticalRateEnglish (0..60,
100 ft/min), VerticalRateMetric (0..200, 10 m/min), Degrees (1..360,
magnetic/true), Direction (ENUMERATED 0..10), FreeText (IA5 SIZE
1..256). Route clearances remain unsupported (deep optional-laden
sequence; staged separately).

## FANS-1/A route clearances (2026-06)

FANSRouteClearance (ten optional components: airports, runways,
SID/STAR/approach procedures, airway intercept, the route-information
sequence) decoded with constraints from the libacars asn1c tables (MIT,
as before): FANSProcedure SIZE(1..6), route sequence SIZE(1..128),
route legs as published-identifier (fixname + optional lat/lon),
lat/lon, place-bearing pairs, place-bearing-distance (NM 0.1 / KM), or
airway designators. trackDetail legs and the trailing
routeInformationAdditional stay undecoded (reported as present).

## Q-series classification (2026-06)

`qseries.rs`: classifies the ARINC 620 `Q`-series link-test / squitter /
OOOI-event downlink labels (`Q0`–`Q7`, `QA`–`QX`). Descriptions are taken
from airframes' own published references (not invented): the
acars-message-documentation repo (`Q0` "ACARS Link Test", `Q2` "ETA
Report", `QF` "OFF Destination Report", `QQ` "OFF Report") and the
acars-decoder-typescript plugin descriptions (`QP` "OUT Report", `QR` "ON
Report", `QS` "IN Report"). The remaining OOOI-bearing `Q` labels are named
from the gate/wheels event each carries per f00b4r0/acarsdec `label.c`
(`QA` gate-out, `QB` wheels-off, `QC` wheels-on, `QD` gate-in, ...).

## MIAM file-transfer reassembly (2026-06)

File transfers spanning multiple label-MA messages reassemble per the
libacars semantics (MIT, as before): the FileTransferRequest registers
file id and size, segments numbered from 1 carry CORE-PDU text
fragments, completion at the declared size parses the combined text as
a CORE PDU (attached to the closing segment's message as
miam_file_complete). Abort frames cancel; 10-minute timeout.
