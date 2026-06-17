# Provenance — xng-mode-adsl

Clean-room implementation of an **ADS-L** (EASA SRD860 i-Conspicuity)
message decoder. ADS-L is the open, low-power, direct-broadcast electronic-
conspicuity standard on the 868 MHz SRD860 band — the FLARM/OGN-adjacent
format. Sources used were protocol/standards facts and the published
reference *layout* only; no decoder code was copied into this crate.

> Roadmap note: in the xng mode list this crate is the item tracked as
> **"ADS-K"**, interpreted as **ADS-L** (EASA SRD860 i-Conspicuity). There
> is no separate published "ADS-K" radio standard; ADS-L is the EASA
> i-Conspicuity format that matches the description.

## Oracles (external references)

1. **EASA ADS-L 4 SRD860 technical specification** — ED Decision 2022/024/R,
   "Technical Specification for ADS-L transmissions using SRD860", Issue 1:
   <https://www.easa.europa.eu/sites/default/files/dfu/ads-l_4_srd860_issue_1.pdf>
   This is the authoritative field layout. Every offset, width, scaling
   factor, enumeration, and worked example in `src/lib.rs` / `src/vr.rs`
   comes from this document:
   - §F.2 ADS-L Header: Payload Type Identifier[8], Sender Address[30]
     (= Address Mapping Table[6] + Address[24]), Reserved[1], Relay[1].
   - §F.2.2 Address Mapping Table (AMT): 0=Random/Privacy, 5=ICAO,
     6=FLARM/OEMs, 7=OGN-Tracker, 8=FANET/OEMs, 9..63=Manufacturer pages.
   - §G.1 iConspicuity payload bit offsets (relative to payload start):
     TimeStamp@0[6], FlightState@6[2], AcftCat@8[5], Emergency@13[3],
     Lat@16[24], Lon@40[24], GroundSpeed@64[8], Altitude@72[14],
     VertRate@86[9], GroundTrack@95[9], SIL@104[2], DesignAssurance@106[2],
     NavIntegrity@108[4], HorizAcc@112[3], VertAcc@115[2], VelAcc@117[2],
     Reserved@119[1].
   - §G.1.5 lat LSB = 1°/93206, lon LSB = 1°/46603; 0xFFFFFF = no fix.
   - §G.1.6 exponential ("variable resolution") encoding:
     `value = 2^exp·(2^N + base) − 2^N` (unsigned: 2-bit exp; signed:
     1-bit sign + 2-bit exp). The worked-example tables in §G.1.7–G.1.9 are
     asserted directly in `src/vr.rs::tests` (altitude 0x0140=0 m,
     0x0528=1000 m, 0x3FFF=61112 m; ground-speed 0x01=0.25 m/s, 0xC4=120 m/s,
     0xFF=238 m/s; vertical-rate 0x048=+10 m/s, 0x1FF=−119 m/s …).
   - §G.1.1–G.1.4 / §G.1.11–G.1.16 enumerations (flight state, aircraft
     category, emergency, SIL, design assurance, navigation integrity,
     horizontal/vertical/velocity accuracy).

2. **lyusupov/SoftRF — ADS-L reference encoder/decoder** (via `gh api`):
   - `software/firmware/source/SoftRF/src/protocol/radio/ADSL.{h,cpp}` —
     the framing parameters (2-FSK 100 kbps, IEEE-Manchester whitening,
     8-byte sync word, 21-byte payload, 3-byte CRC, payload inverted) and
     the decode entry point (`adsl_decode`).
   - `software/firmware/source/libraries/OGN/ads-l.h` — the authoritative
     `ADSL_Packet` packed struct giving the exact **byte** layout of the
     20-byte payload (Type, Address[4], Meta[2], Position[11], Integrity[2]),
     the little-endian `get3bytes`/`get4bytes` accessors, the address /
     AMT / relay bit arithmetic, the FANET-cordic lat/lon accessors (which
     reduce to the spec's 1°/93206 and 1°/46603 LSBs), the
     altitude/climb/track field splits, and the CRC-24 (`PolyPass`,
     `0xFFFA0480`).
   - `software/firmware/source/libraries/OGN/ognconv.{h,cpp}` — the
     `XXTEA_*_Key0` scrambler (XXTEA / Corrected Block TEA, all-zero key,
     6 rounds) used by `ADSL_Packet::Descramble`, and the `UnsVRdecode` /
     `SignVRdecode` variable-resolution templates.

   The C source was read for the **layout and algorithm facts** only; it was
   not ported verbatim, and SoftRF is GPL-3 (this crate is MIT/Apache-2.0).

## Spec vs SoftRF: exponential decode divergence (documented)

The SoftRF `UnsVRdecode<Type,N>` template adds small rounding-midpoint
biases (+1, +2, +4) in the upper exponent ranges, so it decodes e.g.
ground-speed field `0xFF` to **239** m/s and altitude `0x3FFF` to **61116**
m. The EASA spec formula `value = 2^exp·(2^N + base) − 2^N` decodes those to
**238** m/s and **61112** m respectively — matching the spec's own published
worked-example tables exactly. This crate follows the **spec formula** (the
authoritative oracle for decoded values); `src/vr.rs::tests` pins it to the
spec worked examples. Both references are real and external; the spec is the
one that defines the correct physical value.

## Verification (no loopback)

`tests/decode_vectors.rs` decodes a complete ADS-L iConspicuity frame and
asserts every field. The frame bytes (`FRAME`) are produced by an
**independent** generator, `gen_vector.py` — a separate language/codebase
that mirrors the OGN/SoftRF C struct layout and codec (XXTEA-key0 scramble,
`0xFFFA0480` CRC-24). This crate only *decodes* the pinned bytes; it never
encodes them, so the test is not an encode→decode self-consistency loop. The
asserted physical values are the EASA spec field encodings, including the
published worked examples (ground-speed field 0xC4 = 120 m/s, altitude field
0x0528 = 1000 m, vertical-rate field 0x048 = +10 m/s) and the spec lat/lon
LSBs. The `gen_vector.py` source is reproduced in this commit message / kept
alongside the development notes for reproducibility.

The `vr.rs` and `crc.rs` unit tests anchor the codecs to the spec worked
examples and to the CRC's defining residue-is-zero property respectively;
the `xxtea.rs` round-trip test exercises the scrambler whose external anchor
is the end-to-end decode of the independently-generated vector above.

## Scope / TODO

This crate ships the verified **message/frame decoder** (bytes → fields →
JSON). Two stretch goals are intentionally left as documented TODOs because
they cannot be externally verified within scope without real reference IQ:

- **IQ → bits demodulator** (868 MHz 2-FSK at 100 kbps, IEEE-Manchester
  de-whitening, 8-byte sync-word correlation, payload bit inversion). The
  framing parameters are recorded above from `ADSL.cpp` for a future
  implementation.
- **Spec-faithful modulator/encoder**. An encoder tested against its own
  decoder would be a loopback, which the verification policy forbids; it is
  therefore deferred until an external bit/IQ vector is available to anchor
  it.
