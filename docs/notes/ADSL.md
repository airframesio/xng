# ADS-L (EASA SRD860 i-Conspicuity) — implementation notes

Native ADS-L decoder for `xng-mode-adsl`, from 868 MHz IQ to structured JSON.
ADS-L is the open, low-power, direct-broadcast electronic-conspicuity standard
published by EASA (ED Decision 2022/024/R, "Technical Specification for ADS-L
transmissions using SRD860", Issue 1) — the FLARM/OGN-adjacent format carried
on the 868 MHz SRD860 band at 100 kbit/s 2-FSK. The crate now provides the
full chain: a wideband-capture channelizer (`AdslChannelDecoder` = DDC +
2-FSK/Manchester/sync front-end), the frame decode core (CRC-24 + XXTEA
descramble), and the iConspicuity field decode (address, position, altitude,
velocity, track, aircraft category, integrity/source fields) into JSON and the
normalized `xng_types::Message` envelope.

This is a **CHANNELIZED-DECODER** crate: capture IQ → channel → wire bytes →
fields → `Message`. The decode core is verified clean-room against the EASA
spec field layout and the OGN/SoftRF reference codec; the 868 MHz IQ → bits
demodulator is validated by a **self-generated modulate→demod loopback**
(there is no public ADS-L reference IQ). The crate is fully wired into the
main binary as a runtime `--mode ads-l`. Clean-room: protocol facts were taken
from the EASA spec and the published reference *layout* only; no decoder code
was copied (SoftRF is GPL-3, this crate is MIT/Apache-2.0). See
`crates/xng-mode-adsl/PROVENANCE.md`.

> Roadmap note: in the xng mode list this is the item tracked as **"ADS-K"**,
> interpreted as **ADS-L**. There is no separate published "ADS-K" radio
> standard; ADS-L is the EASA i-Conspicuity format matching the description.
> `Mode::from_str` accepts `ads-l`, `adsl`, and `ads-k`.

## Status / wiring

| Aspect | State |
|---|---|
| 868 MHz IQ → bits demodulator (2-FSK / Manchester / sync) | implemented (`demod.rs`); **synthetic-validated** |
| Channelizer (`AdslChannelDecoder`: DDC mix+decimate + front-end) | implemented |
| Frame deframe + CRC-24 + XXTEA descramble | implemented, verified |
| iConspicuity (Type 0x02) field decode → JSON | implemented, verified |
| `to_message` → `xng_types::Message` (Mode::AdsL) | implemented |
| `Mode::AdsL` enum variant + `MessageBody::AdsL` | yes |
| Runtime `--mode ads-l` (CLI/TUI/scan) | yes (`runtime.rs`, `scan.rs`, `main.rs`) |
| Dashboard plotting | yes — "beacons" map+table layer (glyph `🛩`) |
| Self-generated modulator (loopback aid, `modulate.rs`) | yes (validation only, not a TX-grade encoder) |
| Spec-faithful TX encoder | **not implemented** (deferred) |
| FANET / OGNTP payload types | **not implemented** (follow-up) |
| Real off-air IQ capture / CI fixture | **none** (no usable ADS-L recording exists) |

## Pipeline

```text
capture IQ → AdslChannelDecoder
   → [Ddc]      (NCO mix by freq_offset_hz + decimate to CHANNEL_RATE; bypassed
                 at exactly CHANNEL_RATE / zero offset)
   → [demod::FskDemod]  (2-FSK discriminator, carrier-DC tracker, chip-rate
                 integrate-and-dump w/ zero-crossing timing, 8-byte sync
                 correlation both polarities, Manchester chip-pair, payload
                 invert → MSB-first wire bytes)
   → Frame::parse       (optional length-byte strip, CRC-24 check, XXTEA descramble)
   → IConspicuity::decode  (bit/field decode)
   → AdslFrame { wire_bytes, iconspicuity }
   → to_message  (Mode::AdsL, crc_ok=true, RSSI=channel dBFS, raw=wire bytes)
```

`CHANNEL_RATE = 1.0 MS/s` (5 samples per 200 kchip/s Manchester chip);
`CHANNEL_PASSBAND_HZ = 150 kHz` (preserves the ±50 kHz FSK sidebands).
`AdslChannelDecoder::new(input_rate, freq_offset_hz)` accepts any capture rate
≥ CHANNEL_RATE and resamples via the DDC; an exact-rate/zero-offset call takes
the DDC-bypass fast path.

## Physical layer (`demod.rs`, `modulate.rs`)

Parameters from the SoftRF `adsl_proto_desc`:

- **2-FSK, 100 kbit/s** (`RF_BITRATE_100KBPS`), **±50 kHz** deviation
  (`RF_FREQUENCY_DEVIATION_50KHZ`).
- **IEEE Manchester** line code (`RF_WHITENING_MANCHESTER`): data `0` → chips
  `(1,0)`, data `1` → chips `(0,1)`. Chip rate = 200 kchip/s.
- **8-byte sync word** `55 99 95 A6 9A 65 A9 6A`, which is the Manchester chip
  pattern for the 4 data bytes `F5 72 4B 18` (pinned by the
  `sync_chip_pattern_decodes_to_f5724b18` unit test). The demod correlates the
  chip pattern in both polarities (≤4 chip errors tolerated) to absorb the
  carrier-sign ambiguity.
- **Payload inverted** (`RF_PAYLOAD_INVERTED`): the Version+payload+CRC chip
  stream is FSK-inverted on air; the demod re-inverts (plus any sync-detected
  carrier flip) before Manchester decode.

`FskDemod` is a per-sample frequency discriminator → slow DC tracker (carrier
offset) → chip-rate integrate-and-dump with zero-crossing timing recovery →
chip stream → sync correlation → Manchester chip-pair → data-bit decode →
MSB-first wire bytes. An illegal Manchester chip pair (no mid-chip transition)
is resolved by majority so a single glitch still decodes; the frame CRC catches
genuine corruption. `modulate.rs` is the matching transmitter used only as a
loopback aid for the demod tests (preamble + sync + Manchester+inverted
payload, FM-modulated at ±50 kHz) — not a spec-grade TX encoder.

## Frame layer (`lib.rs::Frame`, `crc.rs`, `xxtea.rs`)

- **Layout**: Version[1] + scrambled payload[20] + CRC[3] (`FRAME_LEN = 24`).
  The 20 payload bytes are five little-endian 32-bit words (`words_from_le` /
  `words_to_le`, OGN `get4bytes`/`set4bytes`). Some framings (OGN/SoftRF)
  prepend a Length byte; if the input begins with `LENGTH_FIELD` (`0x18` = 24)
  and is one byte longer than expected, that leading byte is skipped. Both
  framings are exercised by tests.
- **CRC-24** (`crc.rs`): 32-bit polynomial register fed MSB-first, polynomial
  `0xFFFA0480` (OGN `PolyPass`, the Mode-S-style checksum SoftRF selects for
  ADS-L via `RF_CHECKSUM_TYPE_CRC_MODES`). The 24-bit residue is the top three
  bytes (`crc >> 8`); residue == 0 ⇒ intact packet. CRC covers Version +
  payload + the 3 CRC bytes. `BadCrc` on non-zero residue, `TooShort` if fewer
  than 24 body bytes. `calc()` exists as a test/encode helper.
- **XXTEA descramble** (`xxtea.rs`): Corrected Block TEA over the five payload
  words with an **all-zero 128-bit key** and **6 rounds** (`XXTEA_LOOPS`).
  This is obfuscation, not security — the public (zero) key collapses the mix
  function's key term. Matches OGN `ognconv.cpp` `XXTEA_*_Key0`
  (`ADSL_Packet::Descramble`). `decrypt_key0` (decode) and `encrypt_key0`
  (tests) are present; round-trip is a unit test.
- **Header accessors** (§F.2): `payload_type()` = byte 0 (bit 7 set marks a
  unicast payload, masked off when matching); `address_table()` = low 6 bits of
  the 30-bit Sender Address (the AMT); `address()` = the 24-bit Address
  (`word >> 6`); `relay()` = bit 39 (forwarded on behalf of the sender).
- `iconspicuity()` returns `Some(IConspicuity)` only when
  `payload_type() & 0x7F == 0x02` (`TYPE_ICONSPICUITY`); other payload types
  yield `None`.

## iConspicuity payload (`lib.rs::IConspicuity`, §G.1)

Byte offsets follow the OGN/SoftRF `ADSL_Packet` packed struct (payload byte
0 = Type, 1..4 = Address, 5..6 = Meta, 7..17 = Position, 18..19 = Integrity).
Decoded fields and their sourcing:

| Field (JSON) | Source bits (§) | Decode |
|---|---|---|
| `address` / `address_table` / `address_type` | §F.2.2 | 24-bit addr, 6-bit AMT, AMT → name |
| `relay` | §F.2 bit 39 | forwarded flag |
| `timestamp_q` / `timestamp_s` | §G.1.1 | quarter-seconds since the hour mod 60 s; `_s` = ×0.25, `None` if ≥60 (invalid) |
| `flight_state` (+name) | §G.1.2 | 0 undefined / 1 on_ground / 2 airborne |
| `aircraft_category` (+name) | §G.1.3 | 0..13 (none / light fixed-wing … UAS open/specific/certified) |
| `emergency` (+name) | §G.1.4 | 0..6 (undefined … downed_aircraft) |
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

Ground speed, altitude and vertical rate use the spec's exponential encoding:
the two leading bits are a scaling exponent `e ∈ {0,1,2,3}`, the remaining `N`
bits are the base, and

```text
value = 2^e · (2^N + base) − 2^N
```

Signed fields (vertical rate) prepend a sign bit above the exponent
(`sign_decode`). The crate implements the **spec form** (`uns_decode` /
`sign_decode`), which reproduces every §G.1.7–G.1.9 worked example exactly.
This is a deliberate, documented divergence from SoftRF's `UnsVRdecode`
template, which adds small rounding-midpoint biases (+1/+2/+4) in the upper
exponent ranges and so decodes e.g. ground-speed field `0xFF` to 239 m/s and
altitude `0x3FFF` to 61116 m, where the spec says 238 m/s / 61112 m. The spec
is the authoritative oracle for decoded physical values; `vr.rs` tests pin the
spec worked examples.

## Output / integration

`IConspicuity::to_json()` serializes the struct via serde. `to_message` wraps
it into the normalized `xng_types::Message`: `mode = Mode::AdsL`, body
`MessageBody::AdsL { kind: "iconspicuity", details }`, `decode.crc_ok = true`
(only CRC-passing frames are emitted), `signal.rssi_db` = the smoothed channel
level in dBFS, `raw` = the wire bytes (Version + 20 payload + 3 CRC) faithful
to the air interface, and the caller-supplied `frequency_hz` / `Provenance`.

Runtime wiring (`src/runtime.rs`) gives `Mode::AdsL` a `Decoder::Adsl`
(`AdslChannelDecoder`) variant; scan presets (`src/commands/scan.rs`) place it
on the EASA SRD860 868.2 MHz channel at a 2 MS/s capture rate. CLI/TUI accept
`--mode ads-l`. On the web dashboard, ADS-L positions plot in the shared
**"beacons"** map+table layer (alongside radiosonde / SARSAT / DSC), with the
`🛩` glyph (`dashboard.html` `BEACON_EMOJI`).

## Validation / oracles

The crate verifies against **external** references for the decode core, and
uses a clearly-labelled self-generated loopback for the IQ front-end (no public
ADS-L reference IQ exists):

- **EASA ADS-L 4 SRD860 spec** (ED Decision 2022/024/R, Issue 1) is the
  authoritative field layout and the oracle for decoded physical values. Every
  offset, width, scaling factor, enumeration and worked example in
  `lib.rs`/`vr.rs` comes from it: §F.2 header, §F.2.2 AMT, §G.1 payload bit
  offsets, §G.1.5 lat/lon LSBs and no-fix sentinel, §G.1.6 exponential
  encoding, §G.1.7–G.1.9 worked examples, §G.1.1–G.1.4 / §G.1.11–G.1.16
  enumerations.
- **lyusupov/SoftRF** ADS-L reference encoder/decoder (read via `gh api` for
  layout/algorithm facts only, not ported): `ADSL.{h,cpp}` (framing
  parameters — 2-FSK 100 kbit/s, IEEE-Manchester whitening, 8-byte sync word,
  21-byte payload, 3-byte CRC, payload inverted; `adsl_decode` entry point);
  OGN `ads-l.h` (the `ADSL_Packet` packed struct byte layout, LE accessors,
  address/AMT/relay arithmetic, lat/lon LSBs, alt/climb/track splits, CRC-24
  `PolyPass`/`0xFFFA0480`); OGN `ognconv.{h,cpp}` (`XXTEA_*_Key0` scrambler and
  the `UnsVRdecode`/`SignVRdecode` templates). Where SoftRF's VR template and
  the spec disagree, the crate follows the spec (documented above).
- **Independent end-to-end decode vector** (`tests/decode_vectors.rs`): a
  complete ADS-L iConspicuity frame is decoded and every field asserted. The
  frame bytes are produced by `examples/gen_vector.py` — a **separate
  language/codebase** that mirrors the OGN/SoftRF struct + codec (XXTEA-key0
  scramble, `0xFFFA0480` CRC-24). The crate only *decodes* the pinned bytes; it
  never encodes them, so this is not an encode→decode loop. The asserted
  physical values are the EASA spec encodings, including the published worked
  examples (ground-speed field `0xC4` = 120 m/s §G.1.8, altitude field `0x0528`
  = 1000 m §G.1.7, vertical-rate field `0x048` = +10 m/s §G.1.9) and the spec
  lat/lon LSBs. The vector decodes to ICAO address `0x3C5EE2`, 47.5°N / 8.5°E,
  airborne glider, SIL 3 / NIC 11.
- **Self-generated IQ loopback** (`tests/demod_synth_iq.rs`, tests suffixed
  `*_synth_iq`): the IQ front-end is exercised by modulating the **same**
  independently-generated frame the decode vector pins, then demodulating it
  through `AdslChannelDecoder`. Because the modulated bytes are externally
  anchored, only the modulate↔demod transform is self-consistent — the decode
  core stays externally anchored. Coverage: the DDC-bypass baseband path, the
  DDC mix+decimate channelized path (2 MS/s, +250 kHz offset), inverted carrier
  polarity (both sync polarities latch), and the full `to_message` envelope.
  This is the user-approved synthetic validation for a mode with no oracle IQ.
- **Unit tests**: `vr.rs` pins the spec worked examples; `crc.rs` anchors the
  CRC's residue-is-zero property and bit-flip detection; `xxtea.rs` exercises
  the scrambler round-trip (its external anchor is the independent vector
  above); `demod.rs` pins the sync chip pattern → `F5 72 4B 18` and the
  samples-per-chip integer constraint; `decode_vectors.rs` also covers the
  no-length framing, CRC rejection on a flipped bit, the `0xFFFFFF` no-fix
  sentinel, and the JSON shape.

## Known limitations / deferred

- **No off-air capture; demod is synthetic-validated only.** There is no
  usable real-RF ADS-L recording (unlike SONDE/NAVTEX/UAT, which now have
  validated off-air IQ this cycle). The 868 MHz front-end is anchored solely by
  the modulate→demod loopback on clean synthetic IQ. Real captures (noise,
  multipath, frequency drift, partial bursts) will likely want a matched-filter
  chip detector, soft-decision Manchester, and a preamble-AGC warm-up tuned
  against recorded signals — recorded in PROVENANCE but not yet anchored.
- **No spec-faithful TX encoder.** `modulate.rs` is a loopback aid for the
  demod, not a transmitter; a TX-grade encoder would need an external bit/IQ
  vector to anchor it independently. (`encrypt_key0`/`crc::calc` exist as test
  helpers, not a public encode path.)
- **Only the iConspicuity payload type (0x02) is decoded.** FANET and OGNTP
  payload types are a documented follow-up and are not parsed; unicast (bit 7
  of Type) is recognized for routing the iConspicuity match but its distinct
  semantics are not separately handled.

## Key references

- **EASA ADS-L 4 SRD860 technical specification** — ED Decision 2022/024/R,
  Issue 1 (the authoritative field layout and physical-value oracle).
- **lyusupov/SoftRF** (GPL-3) — ADS-L reference framing/codec, read for
  layout/algorithm facts only (`ADSL.{h,cpp}`, OGN `ads-l.h`,
  `ognconv.{h,cpp}`).
- **OGN** (Open Glider Network) — the `ADSL_Packet` struct, XXTEA-key0
  scrambler, `PolyPass` CRC-24, and `UnsVRdecode`/`SignVRdecode`.
- `examples/gen_vector.py` — the independent test-vector generator.
- `crates/xng-mode-adsl/PROVENANCE.md` — clean-room sourcing policy, the
  spec-vs-SoftRF VR divergence note, and the demod loopback verification
  policy.
</content>
</invoke>
