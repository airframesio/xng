# COSPAS-SARSAT 406 MHz beacons — implementation notes

First-Generation Beacon (FGB) **message decoder** for `xng-mode-sarsat`,
per C/S T.001: the 112-bit short / 144-bit long 406 MHz distress message
carried by ELTs, EPIRBs and PLBs. This crate decodes **hex/bits →
structured fields** — message type and format, country code, protocol
classification, the protocol-specific beacon identification, the encoded
position, and both BCH error-correcting codes. The field layout, the two
BCH generator polynomials, the bit offsets and the position arithmetic
are re-derived from the externally published reference decoder
`amsa-code/fgb-decoder` (Apache-2.0); **no code was copied** (the Java
was read to recover protocol facts). Every decode is asserted against
that project's compliance-kit oracle vectors and C/S T.001 worked
examples (`tests/oracle.rs`). Source: `crates/xng-mode-sarsat/src/`.

**Status: decode-core only.** This is a standalone decode library. There
is no IQ demodulator (IQ → bits), no spec-faithful modulator, and the
crate is intentionally **not** wired into the `xng` binary, the
`xng_types::Mode` enum, the runtime, or the CLI — there is no `--mode
sarsat`. Second-generation beacons (C/S T.018, SGB) are out of scope.
See PROVENANCE.md and the Limitations section below.

## Pipeline

`decode_hex(hex)` (`lib.rs`) is the single entry point. It accepts
**15 hex** (60 bits, short beacon ID) or **30 hex** (120 transmitted
bits, full long message — the frame-sync prefix already removed) and
returns a `SarsatBeacon`. There is no signal/PHY stage in this crate.

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

## Output

`decode_hex` returns `SarsatBeacon` (serde-serializable; field names
mirror the `amsa-code/fgb-decoder` JSON so a decode can be asserted
against the published vectors): `message_type`, `format`, `hex_id`,
`country_code`, `protocol_type`, the optional protocol-specific
identification fields, `coarse_position` / `position`, `bch1` /
optional `bch2`, and the optional `raw_bits` (the full T.001-indexed bit
string, for debugging). There is no `xng_types::Message` mapping — the
crate emits its own type, since it is not wired into the runtime.

## Validation / oracles

**Oracle-anchored, not loopback.** The crate verifies against an
independent external implementation, never a self-consistency round-trip.

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

There is **no count-style head-to-head benchmark** (no peer decoder run
on bulk captures), and SARSAT is **not** in CI's count gates — it is
fenced by the exact-result oracle fixtures above.

## Known limitations / intentional gaps

- **No IQ demodulator (IQ → bits).** FGB modulation is biphase-L
  (Manchester) PSK at 400 bps with ±1.1 rad phase modulation on the
  406.025 / 406.028 / 406.037 MHz carrier, preceded by a 160 ms
  unmodulated carrier, a 15-bit bit-sync `1` run and a 9-bit frame sync.
  That demod path is documented as a TODO (`src/lib.rs`) but not shipped.
- **No modulator (bits → IQ).** Out of scope; there is no encoder, so
  validation cannot use an encode→decode loopback (and deliberately
  doesn't — it uses the external oracle instead).
- **Not wired into the runtime.** No `xng_types::Mode` variant, no
  `--mode sarsat`, no `Message` mapping. Standalone decode library;
  runtime integration is a separate follow-up.
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

## Key references

- **C/S T.001** (COSPAS-SARSAT Specification for First-Generation 406 MHz
  Distress Beacons, freely published) — bit layout, BCH polynomials,
  worked examples.
- **`amsa-code/fgb-decoder`** (Apache-2.0) — field-layout + arithmetic +
  verification oracle (offsets, BCH generators, default-location
  substitution, position arithmetic, RLS TAC/ID, modified-Baudot table).
  Facts only; no code copied. Compliance-kit JSON used as test vectors.
- PROVENANCE.md — sourcing policy and per-field oracle notes.
