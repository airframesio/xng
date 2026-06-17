# UAT (978 MHz, DO-282B) — implementation notes

Native UAT (Universal Access Transceiver, 978 MHz, RTCA DO-282B) mode for
`xng-mode-uat`: a wideband IQ front-end (2-ary CPFSK demod → 36-bit sync hunt
→ bit slice) feeding a decode core (bytes/bits → structured fields) for the
ADS-B downlink (state-vector / mode-status / target-state) and the FIS-B
uplink (APDU framing + DLAC text weather products), plus the Reed-Solomon FEC
that fronts both. Protocol facts are anchored to DO-282B / DO-358 /
FAA AC 00-63B, and every **decode-core** test asserts against a real reference
decoder's output (FlightAware **dump978**) rather than an encode→decode
loopback. Source: `crates/xng-mode-uat/src/`.

> **STATUS — runnable mode.** `--mode uat` is wired end-to-end: the
> `xng-types::Mode::Uat` variant, the `MessageBody::Uat { kind, details }`
> body, the `UatChannelDecoder` in `src/runtime.rs`, and CLI / TUI / scan /
> asf-2.0 output all exist. UAT is treated as **wideband like ADS-B** — it
> consumes the whole capture, so the runtime forces offset 0 and refuses a
> non-zero `-c` offset ("tune -c to 978.000M and pass --channels 978").
> **Validated on a real 50 s off-air capture** (~879 CRC-OK frames, live GA
> aircraft) reported this session; that capture is **not** a vendored CI
> fixture (see *Validation*). The standalone `decode_frame` entry (corrected
> payload bytes / raw with-parity frames) still exists and remains
> independent of how the bits were recovered.

## Pipeline

wideband IQ @ capture rate → `Ddc` to `CHANNEL_RATE` (≈2 samples/bit, bypassed
at an exact-rate capture) → `demod::FskDemod` (CPFSK discriminator + sync hunt
+ bit slice) → candidate with-parity block(s) → `fec` (Reed-Solomon correct;
uplink also deinterleave) → corrected payload → `UatDownlink::decode` /
`UatUplink::decode` → `UatMessage` → `to_message` → `xng_types::Message`.

`UatChannelDecoder::new(input_rate).process(&[Complex<f32>]) -> Vec<UatFrame>`
mirrors the ADS-B wideband interface (single 978 MHz signal, offset 0,
`level_dbfs()`). Each `UatFrame` carries the decoded `UatMessage`, the RS
symbols corrected, the with-parity wire bytes, and the channel level at
detection. `to_message(frame, freq, source)` maps it to a normalized
`Message` (`Mode::Uat`, `crc_ok = true` since the frame passed RS, the
corrected count surfaced as `fec_corrected`, `rssi_db` = the channel level,
and `MessageBody::Uat { kind: "adsb"|"fisb", details: <decoded JSON> }`).

Below the front-end, `decode_frame(raw)` dispatches purely by frame length:

| Raw length | `UatFrameKind` | FEC | Decoder |
|---|---|---|---|
| 30 B | `DownlinkShort` | RS(30,18) | `UatDownlink` (18-byte payload) |
| 48 B | `DownlinkLong` | RS(48,34) | `UatDownlink` (34-byte payload) |
| 552 B | `Uplink` | 6× RS(92,72) interleaved | `UatUplink` (432-byte MDB) |

Constants: `UAT_FREQUENCY_HZ` = 978 000 000 (single channel);
`UAT_BIT_RATE` = 1.041667 Mbit/s nominal; `CHANNEL_RATE` = 2 × bit rate
(≈2.083 MS/s, ~2 samples/bit); `CHANNEL_PASSBAND_HZ` = 625 000 (one-sided,
covers the h≈0.6 ±312.5 kHz CPFSK deviation). `UatMessage` boxes both variants
so the enum stays small.

## Front-end / demod (`demod.rs`)

UAT is binary continuous-phase FSK at 1.041667 Mbit/s, h ≈ 0.6 (deviation
≈ ±312.5 kHz): a `1` is the upper tone, `0` the lower. A burst is a 36-bit
sync word then the FEC-coded block (no further line coding — recovered bits
are the RS codeword octets, MSB-first). `FskDemod` runs in the
frequency-discriminator domain:

- **Discriminator** — per-sample `arg(x · conj(prev))` with a slow DC tracker
  (`FREQ_ALPHA = 0.002`) absorbing carrier offset; channel power smoothed
  (`LEVEL_ALPHA = 0.005`) for `level_dbfs`. (Reuses the AIS GFSK discriminator
  idea at UAT's rate; no shared FSK primitive, so the pattern is local.)
- **Sync hunt** — the buffered discriminator stream is searched at sample
  resolution over a half-sample timing grid for the two 36-bit sync words
  (downlink `0xEACDDA4E2`, uplink `0x153225B1D`), ≤ 4 bit errors tolerated
  (`SYNC_MAX_ERRORS`). The half-sample grid is what makes 2-samples/bit robust
  to arbitrary burst arrival phase; carry-over `disc` buffer recovers bursts
  that straddle a chunk boundary.
- **Slice + RS gate** — at a sync hit the symbol period is known, so message
  bits are integrate-and-dumped at the matched phase, packed MSB-first, and
  handed to `decode_frame`. A downlink burst is sliced at the long (48 B)
  length and *also* offered as its 30-byte short prefix; the RS gate validates
  whichever is correct. Hard-decision uplink deinterleave (via
  `fec::correct_uplink`) already recovers clean uplinks; soft-bit deinterleave
  is a possible refinement, not implemented.

`modulate.rs` is the inverse (CPFSK-modulate a known frame to IQ) used only to
build the synthetic-IQ self-tests; it is not on the receive path.

## FEC (`fec.rs`)

Reed-Solomon over GF(2⁸), primitive polynomial **p(x) = 0x187**
(x⁸+x⁷+x²+x+1), first consecutive generator root **α¹²⁰** — identical
field/root parameters to dump978's libfec call
`init_rs_char(8, 0x187, 120, 1, nroots, pad)`. Codec is the shared
`xng_dsp::rs::ReedSolomon` (Berlekamp–Massey + Forney).

All three UAT codes are **shortened** RS codes — the full 255-symbol code with
the leading high-degree symbols held at zero:

| Code | data | parity (nroots) | corrects | pad (255−n) |
|---|---|---|---|---|
| RS(30,18) downlink short | 18 | 12 | 6 sym | 225 |
| RS(48,34) downlink long | 34 | 14 | 7 sym | 207 |
| RS(92,72) uplink block | 72 | 20 | 10 sym | 163 |

- **Encode** (`encode_short`) feeds only the real data bytes — leading zeros
  never change the systematic remainder — and returns the parity octets in
  transmission order (highest-degree first).
- **Correct** (`correct_block`) virtual-zero-fills the front to 255, runs
  `rs.correct`, then strips the pad and parity. Returns the number of
  corrected symbols.
- **Uplink** (`correct_uplink`) deinterleaves the 552-byte frame into six
  RS(92,72) blocks where frame byte `i*6 + b` belongs to block `b`
  (DO-282B §2.4.4.2 / dump978 `FEC::CorrectUplink`), corrects each, and
  concatenates the six 72-byte data sections → 432 bytes. `interleave_uplink`
  is the inverse, used to build test frames.

## Downlink ADS-B (`downlink.rs`) — MDB, DO-282B §2.2.4.5

Field offsets, scaling, and the payload-type → element-set mapping follow
dump978's `uat_message.cc` (`AdsbMessage`); the structured `UatDownlink`
mirrors its **"emit only present fields"** JSON shape (every optional field is
`skip_serializing_if` so it appears only when the element is present and
carries data). Bit addressing is 1-based MSB-first `(byte, bit)` via
`BitReader`, matching DO-282B's field tables and dump978's `RawMessage::Bits`.

### HDR (§2.2.4.5.1)

`payload_type` = bits(1,1..1,5) (the MDB type); `address_qualifier` =
bits(1,6..1,8); `address` = bits(2,1..4,8) (24-bit, formatted 6 hex digits).

**Payload type → element set** (DO-282B Table 2-10, `match payload_type`):

| Type | Elements decoded |
|---|---|
| 0 | SV |
| 1 | SV + MS + AUX-SV |
| 2 | SV + AUX-SV |
| 3 | SV + MS + TS (TS @ byte 30) |
| 4 | SV + TS (TS @ byte 30) |
| 5 | SV + AUX-SV |
| 6 | SV + TS (TS @ byte 25) + AUX-SV |
| 7–10 | SV only |
| 11–31 | HDR only |

### State Vector (`decode_sv`)

- **Position**: raw lat bits(5,1..7,7), raw lon bits(7,8..10,7), each ×
  360/2²⁴; lat > 90 ⇒ −180, lon > 180 ⇒ −360; emitted (rounded to 5 dp) only
  when lat/lon/NIC are non-zero.
- **Altitude**: raw bits(11,1..12,4); when non-zero, (raw−41)×25 ft, routed by
  bit(10,8) to `geometric_altitude` or `pressure_altitude`.
- **NIC**: bits(12,5..12,8) (always emitted).
- **A/G state**: bits(13,1..13,2) → `airground_name` (airborne / supersonic /
  ground / reserved).
- **Airborne (0) / supersonic (1)**: N/S and E/W velocity (sign + magnitude,
  ×4 multiplier in supersonic), `ground_speed` = √(n²+e²), `true_track` =
  atan2(e,n); vertical velocity (raw−1)×64 ft/min routed by `vv_src`
  (geometric / barometric).
- **On ground (2)**: `ground_speed` (raw−1); track-or-heading-type
  bits(14,7..14,8) → `true_track` / `magnetic_heading` / `true_heading` from a
  ×360/512 angle; **aircraft size** (length/width) from DO-282B Table 2-35
  16-entry table; **GPS antenna offset** lateral/longitudinal (and the
  "offset applied" flag).
- **Address-qualifier tail**: for ADS-B/vehicle/fixed-beacon (0/1/4/5),
  `utc_coupled` + `uplink_feedback`; for TIS-B/ADS-R (2/3/6), `tisb_site_id`.

### Mode Status (`decode_ms`, type 1/3)

Base-40 packed **callsign / flightplan-id** (alphabet
`"0123456789…XYZ *??"`, trailing spaces and code-37 `*` trimmed); bit(27,7)
selects callsign vs. flightplan-id (the latter validated as four octal
digits — a squawk). `emitter_category` rendered as e.g. `A2`. Then
`emergency`, `mops_version`, `sil`, `transmit_mso`, `sda`, `nac_p`, `nac_v`,
`nic_baro`; `capability_codes` (uat_in / es_in / tcas_operational),
`operational_modes` (tcas_ra_active / ident_active / atc_services),
`sil_supplement`, `gva`, `single_antenna`, `nic_supplement`.

### Auxiliary State Vector (`decode_auxsv`, type 1/2/5/6)

Secondary altitude bits(30,1..31,4), (raw−41)×25 ft, routed to the *other*
altitude type vs. the SV (bit(10,8) inverted relative to the SV).

### Target State (`decode_ts`, type 3/4 @ byte 30, type 6 @ byte 25)

Selected altitude (MCP/FCU vs. FMS by the SAT bit, (raw−1)×32 ft); barometric
pressure setting 800 + (raw−1)×0.8 hPa; selected heading (signed, ×180/256);
`mode_indicators` (autopilot / vnav / altitude_hold / approach / lnav) when
the mode-bit-present flag is set.

`to_json()` serializes the struct directly (no transport/metadata wrapper).

## Uplink FIS-B (`uplink.rs`) — ground MDB, DO-282B §2.2.4.6 / DO-358

432-byte corrected MDB. Field offsets, the information-frame header, the
FIS-B APDU header, the time options, and segmentation flags follow dump978's
`legacy/uat_decode.c` (`uat_decode_uplink_mdb` / `uat_decode_info_frame`) —
the legacy decoder is the reference that actually decodes FIS-B contents.

### MDB header (`UatUplink`)

- **Site position**: 24-bit lat (mdb[0..2]) and lon (mdb[2..5]) each ×
  360/2²⁴ with the same >90/>180 wrap; decoded **regardless** of the
  `position_valid` flag (mdb[5]&1) — dump978's behaviour.
- `utc_coupled` (mdb[6]&0x80), `app_data_valid` (mdb[6]&0x20),
  `slot_id` (mdb[6]&0x1f), `tisb_site_id` (mdb[7]>>4).

### Information frames (`InfoFrame`)

When `app_data_valid`, walk the 424-byte application area: each frame is a
9-bit length (`(app[pos]<<1)|(app[pos+1]>>7)` & 0x1ff) + 4-bit `frame_type`
(app[pos+1]&0x0f) + `length` payload bytes. Stop on a zero-length type-0
frame, on overrun, or at 256 frames (`MAX_INFO_FRAMES`, dump978's cap).
`frame_type` 0 = FIS-B APDU, 15 = TIS-B/ADS-R Service Status; only type 0 is
parsed into a `FisbProduct`.

### FIS-B APDU (`parse_fisb`, `FisbProduct`)

Type-0 frames ≥ 4 bytes. Flags from data[0]: `a_flag` (Application
Method/AID present, 0x80), `g_flag` (geometric overlay, 0x40), `p_flag`
(position present, 0x20); `product_id` = `((data[0]&0x1f)<<6)|(data[1]>>2)`;
`s_flag` (segmentation, data[1]&0x02). **Time option** `t_opt` =
`((data[1]&1)<<1)|(data[2]>>7)` selects the `ProductTime` layout and payload
start:

| t_opt | Fields | Payload start |
|---|---|---|
| 0 | hours, minutes | byte 4 |
| 1 | hours, minutes, seconds | byte 5 |
| 2 | month, day, hours, minutes | byte 5 |
| 3 | month, day, hours, minutes, seconds | byte 6 |

`product_name` comes from the FAA AC 00-63B / DO-358 product table
(`dlac::product_name`). When the product is DLAC-text
(`is_dlac_text`: ids 20–27, 411, 412, 413), the payload is DLAC-decoded and
split into `reports`; otherwise `reports` is empty and the raw APDU payload is
kept (not serialized).

### DLAC text + product table (`dlac.rs`)

- **DLAC** ("Document Library and Application Codes") packs six bits per
  character, four chars per three bytes, via a 4-state step machine
  (`decode_dlac`) — a faithful port of dump978's `decode_dlac` including the
  quirk that **step 2 does not advance** the byte index. Code **28 is a TAB
  control**: the *following* code is the run-length of spaces to emit. The
  64-entry alphabet carries A–Z, 0–9, punctuation, and control codes
  (ETX 0x03, SUB 0x1a, RS 0x1e, LF). No fixed length is required — it decodes
  the whole payload.
- **`split_text_reports`** splits decoded text on RS (0x1e) / ETX (0x03) and
  peels up to three leading space-delimited tokens as type / location / time
  (DO-358 generic textual), the rest as body — mirroring dump978's
  product-413 handling.
- **`product_name`** is the full FAA/DO-358 product-id table (METAR/SPECI,
  TAF, SIGMET, AIRMET, PIREP, Winds & Temps Aloft, NOTAM, D-ATIS, NEXRAD
  regional/national/individual, echo tops, lightning, system/operational/
  ground-station status, the generic raster/text/vector/symbolic APDU formats,
  proprietary FISDL/WSI, …; unknown → `"unknown"`).

## Sourcing / oracles

dump978 is the bit-layout **and** test oracle. It is BSD-2 (modern) / GPL-2
(legacy); **no source was copied** — the layouts were re-expressed in Rust,
and the legacy `uat2text` and modern `uat_message.cc` were *built and run* on
this machine to generate the expected test values. These crates verify against
an external decoder, never a self-loopback.

| Fact | Oracle | How verified |
|---|---|---|
| RS poly/root, nroots, pads, uplink deinterleave | dump978 `uat_protocol.h` / `fec.cc` | parameters match `init_rs_char(8,0x187,120,1,…)` |
| RS parity octets (short + long) | libfec `encode_rs_char` (built/run here) | crate's 12/14 check octets equal libfec's for two real payloads |
| Downlink field offsets / scaling / element map / base-40 / JSON shape | dump978 `uat_message.cc` `AdsbMessage::ToJson` | two real `sample-data.txt` frames decode byte-for-field equal (incl. callsign `N5130E`) |
| Uplink MDB layout, info-frame header, APDU header, time options, DLAC, product ids | dump978 `legacy/uat_decode.c` (`uat2text`) | one real 432-byte MDB: site pos, slot/site id, NOTAM (prod 8) framing + time, three product-413 Winds-Aloft reports (RKS/BAM/PRC, 250000Z) |
| DLAC alphabet + TAB run-length + step machine | dump978 `decode_dlac` | a `METAR` word and a TAB-run sequence decode identically |
| Product names | FAA AC 00-63B / DO-358 via dump978 `get_fisb_product_name` | table values asserted |

Decode-core test vectors are real off-air UAT from dump978's published
`sample-data.txt` (GA traffic near KPAO). `tests/vectors.rs` has 14 tests:
9 oracle-anchored decode-core / FEC tests, plus 5 demod tests suffixed
`_synth_iq` / `to_message_emits_uat_adsb_variant` that exercise the new
front-end. Unit tests also live in `bits.rs` / `dlac.rs`.

## Validation

- **Decode core + FEC** — oracle-anchored against dump978 (table above);
  these are the strong guarantees.
- **Front-end (in-repo, synthetic IQ)** — `modulate.rs` CPFSK-modulates the
  two dump978-pinned known frames (short type-0, long type-1 N5130E) to IQ and
  `UatChannelDecoder` recovers the *exact* with-parity frame and the pinned
  decoded fields. Covered: clean at native `CHANNEL_RATE`, additive-noise
  (deterministic xorshift, not a clean loopback), and through-DDC at 8 MS/s.
  `to_message_emits_uat_adsb_variant` pins the `Message` mapping. There is
  **no public UAT IQ oracle vector**, so these are self-generated — the decode
  core stays oracle-anchored, the synthetic tests validate only the
  discriminator + sync correlation + bit slicing.
- **Front-end (real off-air, reported this session)** — a 50 s live 978 MHz
  capture yielded ~879 CRC-OK frames from real GA aircraft through
  `UatChannelDecoder`. This is a real-world result, **not** a vendored CI
  fixture: there is no UAT IQ file in `bench/`, no `bench/baselines.json` UAT
  entry, and no `tests/data/` directory (the CI-gated off-air fixtures are
  sonde / navtex / sarsat, not UAT). It is not reproducible from the repo.

## Limitations / deferred

- **Front-end validated only synthetically in-repo.** The CPFSK demod + sync
  hunt + bit slicing are pinned by self-generated modulate→demod tests; the
  one real off-air validation (~879 CRC-OK / 50 s) is not vendored or
  CI-gated, so the repo cannot reproduce a real-IQ pass.
- **Uplink is hard-decision only.** No soft-bit deinterleave; clean uplinks
  recover, but the marginal-SNR uplink path is untested off-air.
- **On-ground SV and Target State unpinned.** `sample-data.txt` carries only
  airborne GA downlinks, so the on-ground branch (ground speed,
  track-or-heading, aircraft size, GPS antenna offsets) and the Target-State
  element are **ported faithfully from `uat_message.cc` but have no pinned
  reference vector** — and no loopback test was fabricated for them.
- **FIS-B graphical products not parsed.** Only type-0 FIS-B APDUs are
  decoded, and only DLAC-text products are turned into `reports`. NEXRAD /
  graphical / raster / vector / TFR-graphic products are named and their raw
  APDU payload is preserved, but not interpreted (matching the scope of the
  legacy text reference). Info `frame_type` 15 (Service Status) is framed but
  not decoded.

## Dashboard / output

- UAT downlinks (`kind: "adsb"`) plot as **aircraft** on the HTTP dashboard
  and **merge with 1090 ADS-B by ICAO** (`src/outputs/http.rs`), so an
  aircraft seen on both links is one track with two sources (`adsb` + `uat`).
- The console renderer prints `UAT ADSB` / `UAT FISB` lines; asf-2.0 output
  carries the `MessageBody::Uat { kind, details }` body
  (`crates/xng-proto/src/lib.rs`).

## Gotchas

1. Length-dispatched FEC: 30/48/552 raw bytes pick short/long/uplink; the MDB
   type in the header would also disambiguate downlink, but `decode_frame`
   goes by length.
2. Shortened RS: encode feeds only the real data bytes; correct virtual-
   zero-fills the front to 255 and strips the pad.
3. Uplink interleave is byte-of-six: frame byte `i*6 + b` → block `b`.
4. DLAC step 2 does **not** advance the byte index, and TAB (code 28) makes
   the *next* code a space run-length — both ported quirks of dump978.
5. AUX-SV altitude is the *opposite* type (geo vs. pressure) to the SV's,
   keyed by the same bit(10,8).
6. dump978 decodes uplink site lat/lon even when `position_valid` is clear;
   the crate matches that.

## Key references

- **RTCA DO-282B** — UAT MOPS: short/long downlink payloads, HDR, SV / MS /
  AUX-SV / TS elements, 432-byte uplink MDB + FIS-B framing, RS FEC.
- **FAA AC 00-63B / RTCA DO-358** — FIS-B product list, DLAC 6-bit text
  products.
- **FlightAware dump978** (`github.com/flightaware/dump978`) — bit-layout +
  test oracle: `uat_protocol.h`, `fec.cc` (FEC); `uat_message.cc` /
  `uat_message.h` (downlink); `legacy/uat_decode.c` (uplink FIS-B / DLAC /
  product table). BSD-2 / GPL-2; built and run as an oracle, not vendored.
- Shared DSP: `xng_dsp::rs::ReedSolomon`, `xng_dsp::Ddc`.
- `crates/xng-mode-uat/PROVENANCE.md` — sourcing policy, FEC notes, the IQ
  front-end + synthetic-IQ validation, and the runtime-wiring scope.
