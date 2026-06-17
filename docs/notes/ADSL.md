# ADS-L (EASA SRD860 i-Conspicuity) — implementation notes

Native ADS-L message/frame decoder for `xng-mode-adsl`. ADS-L is the open,
low-power, direct-broadcast electronic-conspicuity standard published by
EASA (ED Decision 2022/024/R, "Technical Specification for ADS-L
transmissions using SRD860", Issue 1) — the FLARM/OGN-adjacent format
carried on the 868 MHz SRD860 band at 100 kbps 2-FSK. The crate takes a
received ADS-L packet (the on-wire bytes **after** Manchester de-whitening
and sync detection) and decodes the iConspicuity payload — address,
position, altitude, velocity, track, aircraft category, and the
integrity/source fields — into structured JSON.

This is a **DECODE-CORE** crate: bytes → fields → JSON. It is verified
clean-room against the EASA spec field layout and the OGN/SoftRF reference
codec, but the 868 MHz IQ → bits demodulator is a documented TODO, and the
crate is **not yet wired into the main binary** (no `--mode` selector, no
`Mode` enum variant — see "Status / wiring"). Clean-room: protocol facts
were taken from the EASA spec and the published reference *layout* only; no
decoder code was copied (SoftRF is GPL-3, this crate is MIT/Apache-2.0).
See `crates/xng-mode-adsl/PROVENANCE.md`.

> Roadmap note: in the xng mode list this is the item tracked as **"ADS-K"**,
> interpreted as **ADS-L**. There is no separate published "ADS-K" radio
> standard; ADS-L is the EASA i-Conspicuity format matching the description
> (`TODO.md` NEW-P2-1).

## Status / wiring

| Aspect | State |
|---|---|
| Frame deframe + CRC-24 + XXTEA descramble | implemented, verified |
| iConspicuity (Type 0x02) field decode → JSON | implemented, verified |
| 868 MHz IQ → bits demodulator (2-FSK / Manchester / sync) | **not implemented** (documented TODO) |
| Spec-faithful encoder/modulator | **not implemented** (deferred — would be loopback) |
| FANET / OGNTP payload types | **not implemented** (follow-up) |
| Workspace member (builds + `cargo test`) | yes (via `crates/*`) |
| `--mode` selector / `Mode` enum variant | **none** — not a runtime mode |
| Depended on by the main `xng` binary | **no** |

The crate compiles and its tests run, but nothing in the application
consumes it: it is not listed as a `*.workspace` dependency in the root
`Cargo.toml`, there is no `Mode::AdsL` (or similar) in `xng-types`, and no
CLI `--mode` value reaches it. It is a standalone, externally-verified
decode library awaiting the demodulator front-end and mode plumbing.

## Pipeline

```text
packet bytes → Frame::parse  (length-byte strip, CRC-24 check, XXTEA descramble)
             → IConspicuity::decode  (bit/field decode)
             → serde_json::Value
```

`Frame::parse` accepts the de-whitened on-wire content after the sync word:
the Version byte, the (scrambled) 20-byte payload, then 3 CRC bytes
(`FRAME_LEN = 24`). Some framings (OGN/SoftRF) prepend a Length byte; if the
input begins with the fixed `LENGTH_FIELD` (`0x18` = 24) and is one byte
longer than expected, that leading Length byte is skipped automatically.
Both framings (with and without the Length byte) are exercised by tests.

## Frame layer (`lib.rs::Frame`, `crc.rs`, `xxtea.rs`)

- **Layout**: Version[1] + scrambled payload[20] + CRC[3]. The 20 payload
  bytes are five little-endian 32-bit words (`words_from_le` /
  `words_to_le`, OGN `get4bytes`/`set4bytes`).
- **CRC-24** (`crc.rs`): 32-bit polynomial register fed MSB-first,
  polynomial `0xFFFA0480` (OGN `PolyPass`, the Mode-S-style checksum SoftRF
  selects for ADS-L). The 24-bit residue is the top three bytes
  (`crc >> 8`); residue == 0 ⇒ intact packet. CRC covers Version + payload +
  the 3 CRC bytes. `BadCrc` on non-zero residue, `TooShort` if fewer than
  24 body bytes.
- **XXTEA descramble** (`xxtea.rs`): Corrected Block TEA over the five
  payload words with an **all-zero 128-bit key** and **6 rounds**
  (`XXTEA_LOOPS`). This is obfuscation, not security — the key is public
  (zero), which collapses the mix function's key term (`mx_key0`). Matches
  OGN `ognconv.cpp` `XXTEA_*_Key0` (`ADSL_Packet::Descramble`). Both
  `decrypt_key0` (decode path) and `encrypt_key0` (used by tests) are
  present; round-trip is a unit test.
- **Header accessors** (§F.2): `payload_type()` = byte 0 (bit 7 set marks a
  unicast payload, masked off when matching); `address_table()` = low 6
  bits of the 30-bit Sender Address (the AMT); `address()` = the 24-bit
  Address (`word >> 6`); `relay()` = bit 39 (forwarded on behalf of the
  sender).
- `iconspicuity()` returns `Some(IConspicuity)` only when
  `payload_type() & 0x7F == 0x02` (`TYPE_ICONSPICUITY`); other payload
  types yield `None`.

## iConspicuity payload (`lib.rs::IConspicuity`, §G.1)

Byte offsets follow the OGN/SoftRF `ADSL_Packet` packed struct (payload
byte 0 = Type, 1..4 = Address, 5..6 = Meta, 7..17 = Position, 18..19 =
Integrity). Decoded fields and their sourcing:

| Field (JSON) | Source bits (§) | Decode |
|---|---|---|
| `address` / `address_table` / `address_type` | §F.2.2 | 24-bit addr, 6-bit AMT, AMT → name |
| `relay` | §F.2 bit 39 | forwarded flag |
| `timestamp_q` / `timestamp_s` | §G.1.1 | quarter-seconds since the hour mod 60 s; `_s` = ×0.25, `None` if ≥60 (invalid) |
| `flight_state` (+name) | §G.1.2 | 0 undefined / 1 on_ground / 2 airborne |
| `aircraft_category` (+name) | §G.1.3 | 0..13 (light fixed-wing … UAS open/specific/certified) |
| `emergency` (+name) | §G.1.4 | 0..6 (no_emergency … downed_aircraft) |
| `latitude_deg` / `longitude_deg` | §G.1.5 | signed-24 × LSB; lat 1°/93206, lon 1°/46603; `None` on `0xFFFFFF` no-fix sentinel |
| `ground_speed_mps` | §G.1.8 | unsigned VR-decode (N=6) × 0.25 m/s |
| `altitude_hae_m` | §G.1.7 | 14-bit unsigned VR-decode (N=12) − 320 m offset; geometric (WGS-84 HAE) |
| `vertical_rate_mps` | §G.1.9 | signed VR-decode (N=6) × 0.125 m/s; `None` when field == `0x100` (declared absent) |
| `ground_track_deg` | §G.1.10 | 9-bit × (360/512)° clockwise from north |
| `source_integrity` (SIL) | §G.1.11 | 0..3 |
| `design_assurance` (SDA) | §G.1.12 | 0..3 |
| `navigation_integrity` (NIC) | §G.1.13 | 0..12 |
| `horizontal_accuracy` (NACp) | §G.1.14 | 0..7 |
| `vertical_accuracy` (GVA) | §G.1.15 | 0..3 |
| `velocity_accuracy` (NACv) | §G.1.16 | 0..3 |

The altitude, climb and track fields straddle byte boundaries; the field
splits (alt = `(pos[8]&0x3F)<<8 | pos[7]`, climb = `(pos[9]&0x7F)<<2 |
pos[8]>>6`, track = `pos[10]<<1 | pos[9]>>7`) follow the OGN struct's
bit-packing exactly. Optional fields (`timestamp_s`, `latitude_deg`,
`longitude_deg`, `vertical_rate_mps`) are omitted from the JSON when absent
(`skip_serializing_if`).

### Address Mapping Table (AMT) names (§F.2.2)

`address_type_name`: 0 random/privacy, 5 icao, 6 flarm, 7 ogn, 8 fanet,
9..63 manufacturer, else reserved.

### Variable-resolution (exponential) codec (`vr.rs`, §G.1.6)

Ground speed, altitude and vertical rate use the spec's exponential
encoding: the two leading bits are a scaling exponent `e ∈ {0,1,2,3}`, the
remaining `N` bits are the base, and

```text
value = 2^e · (2^N + base) − 2^N
```

Signed fields (vertical rate) prepend a sign bit above the exponent
(`sign_decode`). The crate implements the **spec form** (`uns_decode` /
`sign_decode`), which reproduces every §G.1.7–G.1.9 worked example exactly.
This is a deliberate, documented divergence from SoftRF's `UnsVRdecode`
template, which adds small rounding-midpoint biases (+1/+2/+4) in the upper
exponent ranges and so decodes e.g. ground-speed field `0xFF` to 239 m/s
and altitude `0x3FFF` to 61116 m, where the spec says 238 m/s / 61112 m.
The spec is the authoritative oracle for decoded physical values; `vr.rs`
tests pin the spec worked examples.

## Output

`IConspicuity::to_json()` serializes the struct via serde to a
`serde_json::Value` with the field names above. The crate emits this JSON
directly; there is no `xng_types::Message` mapping yet (no mode wiring), so
no `Mode`/`crc_ok`/RSSI envelope is produced — that integration is part of
the deferred plumbing.

## Validation / oracles (no loopback)

This crate verifies against **external** references, never self-loopback:

- **EASA ADS-L 4 SRD860 spec** (ED Decision 2022/024/R, Issue 1) is the
  authoritative field layout and the oracle for decoded physical values.
  Every offset, width, scaling factor, enumeration and worked example in
  `lib.rs`/`vr.rs` comes from it: §F.2 header, §F.2.2 AMT, §G.1 payload bit
  offsets, §G.1.5 lat/lon LSBs and no-fix sentinel, §G.1.6 exponential
  encoding, §G.1.7–G.1.9 worked examples, §G.1.1–G.1.4 / §G.1.11–G.1.16
  enumerations.
- **lyusupov/SoftRF** ADS-L reference encoder/decoder (read via `gh api`
  for layout/algorithm facts only, not ported): `ADSL.{h,cpp}` (framing
  parameters — 2-FSK 100 kbps, IEEE-Manchester whitening, 8-byte sync word,
  21-byte payload, 3-byte CRC, payload inverted; `adsl_decode` entry point);
  OGN `ads-l.h` (the `ADSL_Packet` packed struct byte layout, LE
  accessors, address/AMT/relay arithmetic, lat/lon LSBs, alt/climb/track
  splits, CRC-24 `PolyPass`/`0xFFFA0480`); OGN `ognconv.{h,cpp}`
  (`XXTEA_*_Key0` scrambler and the `UnsVRdecode`/`SignVRdecode`
  templates). Where SoftRF's VR template and the spec disagree, the crate
  follows the spec (documented above).
- **Independent end-to-end vector** (`tests/decode_vectors.rs`): a complete
  ADS-L iConspicuity frame is decoded and every field asserted. The frame
  bytes are produced by `examples/gen_vector.py` — a **separate
  language/codebase** that mirrors the OGN/SoftRF struct + codec (XXTEA-key0
  scramble, `0xFFFA0480` CRC-24). The crate only *decodes* the pinned
  bytes; it never encodes them, so this is not an encode→decode loop. The
  asserted physical values are the EASA spec encodings, including the
  published worked examples (ground-speed field `0xC4` = 120 m/s §G.1.8,
  altitude field `0x0528` = 1000 m §G.1.7, vertical-rate field `0x048` =
  +10 m/s §G.1.9) and the spec lat/lon LSBs. The vector decodes to ICAO
  address `0x3C5EE2`, 47.5°N / 8.5°E, airborne glider, SIL 3 / NIC 11.
- **Unit tests**: `vr.rs` pins the spec worked examples; `crc.rs` anchors
  the CRC's residue-is-zero property and bit-flip detection; `xxtea.rs`
  exercises the scrambler round-trip (its external anchor is the
  independent vector above); `decode_vectors.rs` also covers the no-length
  framing, CRC rejection on a flipped bit, the `0xFFFFFF` no-fix sentinel,
  and the JSON shape.

## Known limitations / deferred

- **No IQ → bits demodulator.** The 868 MHz 2-FSK at 100 kbps,
  IEEE-Manchester de-whitening, 8-byte sync-word correlation and payload
  bit inversion are recorded from `ADSL.cpp` in PROVENANCE for a future
  implementation but are not coded. The crate starts from de-whitened
  post-sync bytes.
- **No encoder/modulator.** A spec-faithful encoder tested against this
  decoder would be a loopback, which the verification policy forbids; it is
  deferred until an external bit/IQ vector is available to anchor it.
  (`encrypt_key0`/`crc::calc` exist as test helpers, not a public encode
  path.)
- **Only the iConspicuity payload type (0x02) is decoded.** FANET and
  OGNTP payload types are a documented follow-up (`TODO.md` NEW-P2-1) and
  are not parsed; unicast (bit 7 of Type) is recognized for routing the
  iConspicuity match but its distinct semantics are not separately handled.
- **Not wired into the application** (see Status / wiring): no `--mode`,
  no `Mode` variant, no `xng_types::Message` mapping, not a dependency of
  the main binary. DECODE-CORE only.
- **No off-air capture.** Verification is against the spec + the
  independent generated vector; there is no real-RF ADS-L recording in CI
  (the demodulator that would consume one does not exist yet).

## Key references

- **EASA ADS-L 4 SRD860 technical specification** — ED Decision 2022/024/R,
  Issue 1 (the authoritative field layout and physical-value oracle).
- **lyusupov/SoftRF** (GPL-3) — ADS-L reference framing/codec, read for
  layout/algorithm facts only (`ADSL.{h,cpp}`, OGN `ads-l.h`,
  `ognconv.{h,cpp}`).
- **OGN** (Open Glider Network) — the `ADSL_Packet` struct, XXTEA-key0
  scrambler, `PolyPass` CRC-24, and `UnsVRdecode`/`SignVRdecode`.
- `examples/gen_vector.py` — the independent test-vector generator.
- `crates/xng-mode-adsl/PROVENANCE.md` — clean-room sourcing policy and the
  spec-vs-SoftRF VR divergence note.
