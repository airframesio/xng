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
