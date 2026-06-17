# COSPAS-SARSAT 406 MHz beacons — implementation notes

First-Generation Beacon (FGB) decoder for `xng-mode-sarsat`, per C/S
T.001: the 112-bit short / 144-bit long 406 MHz distress message carried
by ELTs, EPIRBs and PLBs. The crate ships **two layers**: a **message/frame
decoder** (`decode_hex`: hex/bits → structured fields — message type and
format, country code, protocol classification, the protocol-specific
beacon identification, the encoded position, and both BCH error-correcting
codes), and an **IQ demodulator** (`SarsatChannelDecoder`: a DDC +
biphase-L (Manchester) PSK demod that recovers a beacon from channelized
capture IQ and feeds it to `decode_hex`). The field layout, the two BCH
generator polynomials, the bit offsets and the position arithmetic are
re-derived from the externally published reference decoder
`amsa-code/fgb-decoder` (Apache-2.0); **no code was copied** (the Java
was read to recover protocol facts). Every decode is asserted against
that project's compliance-kit oracle vectors and C/S T.001 worked
examples (`tests/oracle.rs`). Source: `crates/xng-mode-sarsat/src/`.

**Status: decode-core is oracle-anchored and oracle-validated; the demod
is synthetic-loopback-validated only.** The crate is wired into the `xng`
binary as `Mode::Sarsat` — `--mode sarsat` (also `cospas`, `406`), runtime
`ModeChannel::Sarsat`, the `scan` channel plan (406.025 / 406.028 /
406.037 MHz), console output, the Airframes uplink (`SARSAT`), and the
dashboard "beacons" map+table layer (🆘 glyph). The decode core is
verified against the AMSA compliance vectors; the **IQ demod path is
validated only by a self-generated modulate→demod loopback** (no public
SARSAT IQ oracle). A real off-air EPIRB capture is vendored as a bench
fixture but does **not** yet decode (0 frames; see Limitations / Real IQ).
Second-generation beacons (C/S T.018, SGB) are out of scope. See
PROVENANCE.md and the Limitations section below.

## Message/frame decode pipeline (`decode_hex`)

`decode_hex(hex)` (`lib.rs`) is the message-layer entry point. It accepts
**15 hex** (60 bits, short beacon ID) or **30 hex** (120 transmitted
bits, full long message — the frame-sync prefix already removed) and
returns a `SarsatBeacon`. There is no signal/PHY stage in this function;
the demod front-end (below) feeds it.

hex → `bits::hex_to_bits` (T.001-indexed bit string) → `message_format`
(bit 25/26) → `classify` (protocol code) → `compute_hex_id` →
`fill_identification` (protocol-specific) → `fill_position` (location /
RLS / user) → `expected_bch1` / `expected_bch2` (BCH verify) →
`SarsatBeacon`.

### Bit indexing

`hex_to_bits` builds a `'0'/'1'` string indexed **exactly like C/S T.001
numbers the bits** — bit *N* of the standard lives at index *N* of the
string. To make that line up it prepends 25 placeholder bits for the
bit-sync / frame-sync field (T.001 bits 1-24 sync, bit 25 format flag),
so the first hex character lands on bit 26.

- **15-hex (short):** bit 25 (format flag) is unknown from the hex form,
  so it is a `?` placeholder; the 60 protocol/ID bits begin at index 26.
  Format is reported as `Unknown`, and **no BCH parity is present** in a
  15-hex string (`bch1.ok` is false).
- **30-hex (long):** the 120 transmitted bits begin at index 25 (the
  format flag). Any other length → `DecodeError::BadLength`.

The decode is **lenient**: BCH failures are reported in `BchField::ok`,
not rejected — matching how real beacon receivers surface miscoded
beacons.

## Message format and protocol classification (`lib.rs`)

| Field | Bits (T.001 index) | Notes |
|---|---|---|
| Format flag | 25 | `0`=short, `1`=long; `?` for 15-hex → `Unknown` |
| Protocol flag | 26 | `1`=user family, `0`=location/standard family |
| Country code (MID) | 27-36 | 10-bit maritime identification digits |
| Protocol code (location) | 37-40 | 4-bit, location/standard family |
| Protocol code (user) | 37-39 | 3-bit, user family |

`message_format` maps bits 25-26 `{00,01}`→short, `{10,11}`→long; a
15-hex `?` matches neither → `Unknown`. `classify` then splits on the
protocol flag:

**Location / standard family** (4-bit code, `is_location()` set):

| Code | Protocol | Label |
|---|---|---|
| 0100 | ELT serial | `ELT - Serial` |
| 0110 | EPIRB serial | `EPIRB - Serial` |
| 0111 | PLB serial | `PLB - Serial` |
| 0010 | Ship MMSI | `Maritime MMSI` |
| 0011 | Aircraft address | `Aircraft Address` |
| 0101 | Aircraft operator | `Aircraft Operator` |
| 1101 | Return Link Service | `Return Link Service` |
| 1001 | ELT-DT | `ELT(DT) Location` |
| 1000 | Ship security | `Ship Security` (structural) |
| 1110 | National ELT | `National ELT` (structural) |
| 1111 | Standard test | `Standard Test Location` (structural) |
| 0000 | Orbitography reservation | `Reserved (orbitography)` |
| other | — | `Location` (generic) |

**User family** (3-bit code): `011`→Serial, `001`→Aviation,
`010`→Maritime, `110`→Radio Call Sign, `000`→Orbitography,
`111`→National, `100`→Test, else→`User`.

`message_type_label` composes the human string (e.g. `Standard Location
(Long)`, `User (Short)`, `Return Link Service Location`, `ELT(DT)
Location`).

## Beacon identification (`compute_hex_id`, `fill_identification`)

The C/S **15-hex beacon ID** is built to be position-independent. For
location protocols the on-air position bits are replaced by the
default-location pattern before hexing, so the ID is stable regardless of
the encoded fix (`hexIdWithDefaultLocation` in the oracle):

- **Standard Location:** bits 26-64 (39 bits) + a 10-bit default-lat
  pattern (`0111111111`) + an 11-bit default-lon pattern
  (`01111111111`) → 15 hex.
- **Return Link Service:** bits 26-66 (41 bits) + 9-bit default-lat
  (`011111111`) + 10-bit default-lon (`0111111111`) → 15 hex.
- **User / non-location:** bits 26-85 verbatim → 15 hex.

Protocol-specific identification fields (`fill_identification`):

| Protocol | Field | Bits | Encoding |
|---|---|---|---|
| ELT/EPIRB/PLB serial | C/S type approval | 41-50 | unsigned |
| ELT/EPIRB/PLB serial | beacon serial number | 51-64 | unsigned |
| Aircraft address | 24-bit ICAO address | 41-64 | hex + octal |
| Aircraft operator | operator designator (3 char) | 41-55 | 5-bit modified-Baudot |
| Aircraft operator | aircraft serial number | 56-64 | unsigned |
| Return Link Service | TAC number | 41-52 | 2-bit prefix + 10-bit value |
| Return Link Service | RLS id | 53-66 | 14-bit unsigned |

The **5-bit modified-Baudot** decode (`baudot5_decode` /
`baudot6_letter`) prefixes each 5-bit symbol with a leading `1` to form
the 6-bit table key, exactly as the oracle's
`mBaudotBits2mBaudotStr(..., 5)`; the table covers letters, space,
hyphen, slash and the figures-shift digits, verbatim from the oracle's
`mbaudotToAsciiMap`. **RLS TAC** prefix maps `00→2, 01→1, 10→3, else
T`, then a 3-digit zero-padded decimal value (e.g. `2153`).

The User-family and the structurally-recognised location families
(Ship MMSI, ELT-DT, orbitography, Ship Security / National ELT /
Standard Test) surface their identity through `hex_id` + `country_code`
+ `protocol_type` + BCH only; their inner sub-fields are deliberately
**not** modelled (see Limitations).

## Position decode (`fill_position`)

Position is only decoded for 30-hex (long) input. Three layouts, each
guarded by a "no-position" default pattern that suppresses the field:

- **Standard Location** (`is_location()`): coarse lat/lon from bits
  65-85, default-suppressed when `011111111101111111111`. Latitude
  (`std_lat_seconds`): sign bit 65, 9-bit code bits 66-74 → degrees =
  code/4, minutes = (code mod 4)·15. Longitude (`std_lon_seconds`):
  sign bit 75, 10-bit code bits 76-85, same arithmetic. On a long
  message the **offset field** bits 113-132 (`offset_position`, default
  `10000011111000001111`) refines coarse → `position`: per axis sign +
  5-bit minutes + 4-bit (×4) seconds added to |coarse|.
- **Return Link Service**: coarse from bits 67-85 in 30-minute units
  (`rls_coarse_seconds`, 9-bit lat + 10-bit lon split,
  `Common.position(...,67,19,1800)`), default `0111111110111111111`;
  **fine** position from the offset bits 115-132 (`rls_fine_seconds`,
  default `100001111`) adds minutes + (×4) seconds per axis → `position`.
- **User Location** (user family, long): absolute lat bits 108-119 / lon
  bits 120-132 (`user_location`), default `011111110000` /
  `0111111110000`. Latitude = 7-bit deg + 4-bit (×4) min; longitude =
  8-bit deg + 4-bit (×4) min, with the leading sign bit.

All positions are returned in decimal degrees (north/east positive) via
`Position { latitude, longitude }`. `long_carries_position` first checks
the trailing 8 hex aren't the all-default `FFFFFFFF` / `00000000`
pattern (those carry no position and no PDF-2 field).

## Error correction — the two BCH codes (`bits.rs`)

C/S T.001 protects the message with two shortened BCH codes; both are
implemented as polynomial long-division remainders, ported faithfully
from the oracle's `BeaconProtocol.calcBCHCODE`.

| Code | Protected data | Parity (transmitted) | Generator g(x) |
|---|---|---|---|
| **PDF-1** BCH(21,15) | bits 25-85 (61 bits) | bits 86-106 (21 bits) | `1001101101100111100011` |
| **PDF-2** BCH(12,7) | bits 107-132 (26 bits) | bits 133-144 (12 bits) | `1010100111001` |

- **GEN1** = x²¹+x¹⁷+x¹⁶+x¹⁵+x¹⁴+x¹¹+x¹⁰+x⁸+x⁷+x⁶+x⁵+x+1.
- **GEN2** = x¹²+x¹⁰+x⁸+x⁵+x⁴+x³+1.

`expected_bchN` pads the protected data, appends the parity zeros, and
runs `calc_bch`; `transmitted_bchN` slices the parity as carried.
`BchField { transmitted, computed, ok }` sets `ok` iff they match
(error **detection**, not correction — see Limitations). **PDF-1 is
always present** on a 30-hex decode; **PDF-2 is added only on a long
message that carries position** (tail not `FFFFFFFF` / `00000000`).

## IQ demodulator (`SarsatChannelDecoder`, `demod.rs`, `modulate.rs`)

`SarsatChannelDecoder` is the IQ → beacon front-end the `xng` runtime
instantiates. FGB modulation (C/S T.001 §2) is **biphase-L (Manchester)
phase modulation of the carrier at ±1.1 rad, 400 bps**, preceded by an
unmodulated carrier, a 15-bit bit-sync `1` run and a 9-bit frame sync.
The chain:

1. **DDC.** Owns an optional `xng_dsp::Ddc` that mixes the capture by
   `freq_offset_hz` and decimates to `CHANNEL_RATE = 8 kHz` (20 samples
   per 400 bps data bit, 10 per half-symbol) with a `CHANNEL_PASSBAND_HZ
   = 1.5 kHz` one-sided passband. At zero offset and an 8 kHz input the
   DDC is skipped.
2. **Carrier recovery (`BiphaseDemod`).** Because the deviation is ±1.1
   rad (not ±π/2) the modulated carrier keeps a non-zero mean component;
   a one-pole complex average (`CARRIER_ALPHA`) tracks that residual
   carrier (frequency offset + phase) — the role the 160 ms unmodulated
   carrier preamble plays in a real receiver. Each sample is derotated
   and `arg(s·conj(carrier))` gives `±1.1·level`.
3. **Half-symbol + timing recovery.** A zero-crossing timing loop
   (`TIMING_GAIN`) locks to the mid-bit transition biphase-L guarantees;
   each half-bit window integrates the residual phase and emits one
   half-symbol (`1` = +1.1 rad, `0` = −1.1 rad).
4. **Sync + assembly (`find_frame` / `assemble`, `lib.rs`).** The
   accumulated half-symbol stream is correlated against the preamble
   (15 `1`s + 9-bit frame sync `000101111`, allowing ≤6 half-symbol
   errors). The first data bit (format flag) selects the long (30-hex,
   120 data bits) vs short (15-hex, 60 ID bits) form; `pair_to_bit`
   un-Manchesters each half-pair, and the assembled hex goes to
   `decode_hex`. Decoded frames are deduped against a rolling 64-entry
   `recent` list.

**Biphase-L polarity is ambiguous** — it depends on the sign of the
recovered carrier phase, so a real beacon can arrive with every
half-symbol inverted (the vendored off-air EPIRB locked only inverted).
`find_frame` correlates **both** polarities across the whole window and
picks the global best-matching preamble (lowest-error, canonical wins
ties), flipping the data half-symbols to canonical before assembly.
`find_frame_recovers_both_polarities` asserts a canonical stream and its
full inversion decode to the identical beacon.

`modulate.rs` is a **self-generated test-signal source only** (biphase-L
±1.1 rad / 400 bps; `burst_iq` prepends ~50 bit-periods of unmodulated
carrier so the one-pole settles). It is **not** a spec-compliance encoder
and is not used by the decode core.

## Output / normalized message

`decode_hex` returns `SarsatBeacon` (serde-serializable; field names
mirror the `amsa-code/fgb-decoder` JSON so a decode can be asserted
against the published vectors): `message_type`, `format`, `hex_id`,
`country_code`, `protocol_type`, the optional protocol-specific
identification fields, `coarse_position` / `position`, `bch1` /
optional `bch2`, and the optional `raw_bits` (the full T.001-indexed bit
string, for debugging).

`to_message(frame, frequency_hz, level_dbfs, source)` maps a recovered
`SarsatFrame` to the normalized `xng_types::Message`: `mode =
Mode::Sarsat`, `body = MessageBody::Sarsat { kind: protocol_type, details:
<SarsatBeacon JSON> }`, `signal.rssi_db = level_dbfs`, `decode.crc_ok =
bch1.ok && bch2.ok` (PDF-2 absent counts as ok), and `raw` = the beacon
hex packed to wire bytes. The dashboard plots positions on the "beacons"
map+table layer (alongside radiosonde / ADS-L / DSC), keyed by `serial` /
`address` / `hex_id` / `beacon_id`.

## Validation / oracles

**Decode core: oracle-anchored, not loopback.** The message layer verifies
against an independent external implementation, never a self-consistency
round-trip. **Demod path: self-generated loopback** (no public SARSAT IQ
oracle exists), kept honestly distinct from the core's oracle anchoring.

- **Reference decoder oracle:** `amsa-code/fgb-decoder` (Apache-2.0, the
  Australian Maritime Safety Authority's open-source FGB decoder).
  `tests/oracle.rs` asserts each decode against real entries from that
  project's **compliance kit**
  (`src/test/resources/compliance-kit/<HEX>.json`, filename = input hex,
  body = the reference decoder's output). Pinned per vector: the exact
  15-/22-hex beacon ID, country code, protocol type, the
  protocol-specific identification, coarse + refined positions, and the
  BCH(21,15) / BCH(12,7) flags — all copied from the reference JSON.
- **Spec oracle:** C/S T.001 (freely published) for the bit layout, the
  two BCH generator polynomials, and the worked examples.

Vectors covered (compliance-kit filenames):

| Hex | Coverage |
|---|---|
| `8DA41A02C17FDFF83B4235FFFFFFFF` | Std Location ELT serial, France (218), no position |
| `8E8628D187874181D738F700000000` | Std Location EPIRB serial, coarse position, S. hemisphere, Italy (232) |
| `A3E7B10016150D364D8B3689C09437` | Std Location PLB serial, coarse + offset + PDF-2, Vietnam (574) |
| `ADA5B61C8C7FDFFBE89AF7FFFFFFFF` | Std Location Aircraft Operator (Baudot `FAC` + serial), Colombia (730) |
| `1C66738928FFBFF`, `3EE6F80D1AFFBFF` | 15-hex Aircraft Address (24-bit ICAO hex+octal) |
| `1D0E4E9142FFBFF` | 15-hex PLB serial, Italy (232) |
| `8E0D0990014710021963C85C7009F5` | RLS Location, TAC 2153 / id 5, coarse + fine + PDF-2 (E. hemisphere) |
| `96ED09900149D4D467EE0851A3B2E8` | RLS Location, USA (366), W.-hemisphere fine position |
| `4CB31E0C02A82608F011BE00000000` | User Aviation (short), Tunisia (203) |
| `4E86A265C600146DBC407600000000` | User Serial (Maritime Float-Free, short), Italy (232) |

`bch1_detects_corrupted_parity` additionally corrupts a parity nibble of
a known-good vector and asserts the BCH flag catches the mismatch — this
is error *detection*, not an encode→decode loopback. `serializes_to_json`
checks the serde field names against the oracle JSON; `rejects_bad_length`
and `hex_to_bits_lengths` cover the length guards.

**Demod loopback (`tests/demod_synth.rs`, `*_synth_iq`).** Because no
public SARSAT IQ reference vector exists, the demod is validated
self-consistently: a known-good compliance-kit hex is modulated at the
biphase-L ±1.1 rad / 400 bps waveform, run through the real
`SarsatChannelDecoder::process`, and the recovered fields asserted equal
to the oracle-known values. `decodes_known_long_beacon_synth_iq` (PLB,
Vietnam, at the channel rate, no DDC) and `decodes_with_ddc_and_cfo_synth_iq`
(ELT, France, out of a 48 kS/s capture with a 3.5 kHz carrier offset —
exercises the DDC mix+decimate and the carrier loop) cover the path;
`to_message_emits_sarsat_variant_synth_iq` checks the normalized-message
mapping. The modulate→demod path is therefore self-generated; the decode
core stays oracle-anchored.

**Real off-air IQ.** `bench/data/sarsat_37500.cs16` is a real 406 EPIRB
burst (sigidwiki `Epirbsignal.zip`, 37.5 kS/s cs16, ~0.5 s, vendored,
80 KB). It is weak/drifting and currently decodes **0 frames**: the
inverted biphase-L polarity now syncs (handled by the dual-polarity
`find_frame`), but a clean decode of this capture also needs a
decision-directed carrier PLL (a 2-attempt PLL was tried and reverted as
data-limited). It is therefore vendored but **not count-gated**.

There is **no count-style head-to-head benchmark** (no peer decoder run
on bulk captures), and SARSAT is **not** in CI's count gates — the decode
core is fenced by the exact-result oracle fixtures above, and the only
real IQ fixture does not yet decode.

## Known limitations / intentional gaps

- **No real off-air decode yet.** The IQ demod (`SarsatChannelDecoder`)
  ships and is wired into the runtime, but it is validated only by a
  self-generated modulate→demod loopback. The one vendored real EPIRB
  capture (`bench/data/sarsat_37500.cs16`) decodes **0 frames** — the
  inverted biphase-L polarity syncs, but this weak/drifting capture also
  needs a decision-directed carrier PLL (follow-up; a 2-attempt PLL was
  reverted as data-limited). So demod correctness on real captures is
  **not yet established** — treat the demod as synthetic-validated only.
- **Modulator is a test-signal source, not a spec encoder.**
  `src/modulate.rs` exists only to feed the demod loopback (biphase-L
  ±1.1 rad / 400 bps); it is not a C/S-compliance modulator.
- **BCH is detect-only.** `BchField::ok` flags whether the transmitted
  parity matches the recomputed parity; the codes are not used to
  *correct* bit errors (no syndrome/error-locator step).
- **Sub-protocols not modelled field-by-field.** Of the ~35
  sub-protocols in the reference decoder, the crate fully models the
  serial ELT/EPIRB/PLB, aircraft address, aircraft operator, RLS, and
  the location/user position layouts. Ship MMSI digit packing, radio
  call-sign, ELT-DT inner fields, the national/test variants, and the
  nature-of-distress / emergency-code flags are **not** decoded — those
  families still return the verified common fields (hex ID, country
  code, protocol type, BCH) rather than shipping unverified sub-fields.
- **Second-generation beacons (C/S T.018, SGB)** — the 250-bit /
  spread-spectrum beacon — are not decoded (no public oracle vectors
  were used).

## Gotchas

1. Bit indexing is **1-based per T.001**: 25 placeholder bits are
   prepended so index *N* == T.001 bit *N* (first hex char at bit 26).
2. 15-hex input has an unknown format flag (`?`) → `Format::Unknown` and
   no BCH parity (`bch1.ok` is false) — that is expected, not a failure.
3. PDF-2 is present only on a long message that carries position; a
   `FFFFFFFF` / `00000000` tail means no position and no PDF-2.
4. The 15-hex beacon ID for location/RLS protocols substitutes the
   default-location pattern — the ID is position-independent by design.
5. BCH is detection-only; failures are surfaced, not rejected (lenient
   decode, like a real beacon receiver).
6. **Biphase-L polarity is ambiguous.** A real beacon can arrive fully
   inverted (the vendored EPIRB locked only inverted); `find_frame`
   correlates both polarities and flips data to canonical before assembly.
   Don't assume a single orientation.

## Key references

- **C/S T.001** (COSPAS-SARSAT Specification for First-Generation 406 MHz
  Distress Beacons, freely published) — bit layout, BCH polynomials,
  worked examples.
- **`amsa-code/fgb-decoder`** (Apache-2.0) — field-layout + arithmetic +
  verification oracle (offsets, BCH generators, default-location
  substitution, position arithmetic, RLS TAC/ID, modified-Baudot table).
  Facts only; no code copied. Compliance-kit JSON used as test vectors.
- PROVENANCE.md — sourcing policy and per-field oracle notes.
