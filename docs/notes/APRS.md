# APRS / AX.25 (Bell 202 AFSK1200) — implementation notes

Native APRS / AX.25 packet-radio decode core for `xng-mode-aprs`. APRS (the
Automatic Packet Reporting System) on VHF — **144.39 MHz** in North America,
**144.800 MHz** in Europe — is **Bell 202 AFSK**: a 1200 Hz "mark" tone and a
2200 Hz "space" tone keyed at **1200 baud**, frequency-modulated onto the RF
carrier (narrowband FM). Packets are framed as **AX.25 v2.2
Unnumbered-Information (UI)** frames whose information field carries an **APRS
Protocol Reference 1.0.1** payload. Clean-room: no decoder was copied or
ported — only protocol facts, the published specifications, and their worked
examples were used, each cited (see `PROVENANCE.md`). The **decode / framing /
payload layers** (AX.25 address rule, X.25 FCS, APRS payload formats) are
anchored to **external** spec-cited byte/bit vectors and published worked
examples, never an encode→decode self-loopback. The **demod** is validated
**only synthetically** (modulate→AWGN→demod); no real off-air IQ exists. On top
of the decode core sits a channelized **IQ front end** (DDC + FM discriminator
+ AFSK1200 correlator) that turns a wideband capture into the link-layer octets
the decode core consumes.

Status: the receive stack is implemented bottom-up — `demod` → `hdlc` →
`ax25` → `aprs` (+ `mice`) — with `AprsChannelDecoder` as the channelized IQ
entry point and `to_message` normalizing a decoded frame into the bus form
`MessageBody::Aprs { kind, details }`. The payload layer now covers the bulk of
real on-air traffic: position (uncompressed + Base-91 compressed, with Chapter 7
data extensions and the Chapter 9 cs/T sub-field), **Mic-E** (Chapter 10 — the
single most common encoding), weather, message, bulletin/announcement/group,
status (incl. Maidenhead grid), object, item, general query, and telemetry. The
DECODE/FRAMING/PAYLOAD tests are spec-anchored; the demod is exercised by a
self-generated modulate→AWGN→demod path and documented as synthetic.

## Pipeline

```
wideband capture IQ
  → Ddc                     mix by freq_offset_hz, decimate to CHANNEL_RATE (38400 S/s)
  → demod::AfskDemod        FM discriminator + Bell 202 dual-tone correlator
                            + transition-resync bit clock → 1 NRZI line symbol/bit
  → hdlc::HdlcDeframer      NRZI differential decode + bit de-stuffing + 0x7E flag framing
deframed AX.25 octet sequence (address…control PID info FCS)
  → ax25::parse_frame       address subfields (callsign<<1 + SSID), control 0x03,
                            PID 0xF0, X.25 FCS check
  → decode_frame            Mic-E? → mice::parse(dest_callsign, info)   [Ch.10, cross-field]
                            else    → aprs::parse(info)                 dispatch on the DTI
  → aprs::AprsPayload       { kind, fields (serde_json) }
  → to_message              → xng_types::Message bus form
```

Two entry points:

- `AprsChannelDecoder::new(input_rate, freq_offset_hz) -> Result<Self, String>`
  — channelized IQ entry (mirrors the NAVTEX `NavtexChannelDecoder` / AIS
  contract). It owns an internal `xng_dsp::Ddc` that mixes the capture by
  `freq_offset_hz` and decimates to `CHANNEL_RATE`, then runs the
  AFSK/HDLC/AX.25/APRS pipeline. `process(iq) -> Vec<AprsFrame>` returns the
  packets recovered from that buffer. When `input_rate == CHANNEL_RATE` **and**
  `freq_offset_hz == 0` the DDC is skipped (IQ is already channelized). By
  default only frames whose FCS validated are emitted; `set_require_fcs(false)`
  also surfaces CRC-failed candidates (with `ax25.fcs_ok == false`).
  `level_dbfs()` reports smoothed channel power.
- `decode_frame(raw: &[u8]) -> Option<AprsFrame>` — the deframed-octet decode
  core: parse the AX.25 UI frame, then parse the APRS payload (only when
  `pid == 0xF0`; any other UI PID parses the frame but keeps the info field as
  a `raw` payload). **Mic-E dispatch lives here**, not in `aprs::parse`: when
  the info field's first byte is a Mic-E data-type id (`` ` ``, `'`, 0x1c, 0x1d)
  it calls `mice::parse(&ax25.dest.callsign, &ax25.info)` — because Mic-E packs
  the latitude into the AX.25 **destination** address (Chapter 10), so the
  decoder needs both fields — and tags the payload `AprsKind::MicE`; a failed
  Mic-E parse falls back to the info-only `aprs::parse`. Returns `None` if it is
  not a parseable UI frame.

`AprsFrame` bundles the decoded `ax25: Ax25Frame`, the parsed
`payload: AprsPayload`, and `raw: Vec<u8>` (the deframed link-layer octets,
address…control PID info FCS).

`to_message(frame, frequency_hz, level_dbfs, source) -> Message` normalizes an
`AprsFrame` into the bus `Message`: `mode = Mode::Aprs`, body
`MessageBody::Aprs { kind, details }` where `kind` is the APRS data class
(`position` / `weather` / `message` / `status` / `object` / `item` /
`telemetry` / `mic-e` / `bulletin` / `query` / `raw`) and `details` is a JSON
object merging the AX.25 addressing (`source`,
`dest`, `via[]`, each in TNC-2 `CALL-SSID` display form) with the decoded APRS
fields (`lat`, `lon`, `symbol_table`, `symbol_code`, `comment`, …).
`decode.crc_ok = ax25.fcs_ok`, RSSI from the channel level, and the deframed
link-layer octets travel as `raw`.

`CHANNEL_RATE = 38_400.0` S/s — an integer multiple of 1200 Bd (**32
samples/bit**) that comfortably resolves the 2200 Hz space tone (Nyquist) and
the FM swing. `CHANNEL_PASSBAND_HZ = 7_000.0` (one-sided): a 2.2 kHz top AFSK
tone under narrowband FM (≈±3 kHz deviation, ≈±5 kHz Carson bandwidth) fits
well inside this, and it rejects the adjacent 25 kHz VHF channels.

## IQ front end (`demod.rs`)

The AFSK1200 demodulator, structured after the other modes' streaming demods
but for a Bell 202 AFSK signal carried in narrowband FM:

- **FM discriminator** — `arg(x · conj(x_prev))` per sample recovers the
  instantaneous audio (the AFSK tone) as a real signal; it also smooths channel
  power for `level_dbfs()` (`LEVEL_ALPHA = 0.002`).
- **Non-coherent dual-tone correlator** — two quadrature (sin/cos) reference
  tables, one tuned to the 1200 Hz mark and one to the 2200 Hz space, are summed
  over a sliding one-bit window (`win = round(samples_per_bit)`, min 4). The
  per-bit decision is `|mark| − |space|` (positive ⇒ mark/1200 Hz). This is
  polarity-unambiguous (no DC-bias tracking needed) and degrades gracefully
  under noise — more robust at low SNR than two raw Goertzel bins and needs no
  per-tone gain matching.
- **Transition-resync bit clock** — a phase accumulator (`timing`, 0..1 of a
  bit) advances `1/samples_per_bit` per sample; the symbol is sampled at its
  wrap. On each detected sign change of the tone decision the clock is
  hard-reset to `TRANSITION_RESET_PHASE = 0.6` (just past 0.5, because the
  trailing matched window has a ≈half-bit group delay, placing the next sampling
  wrap at the symbol center). HDLC's NRZI + bit-stuffing guarantee a transition
  at least every six bits, so this drains accumulated clock error at every
  transition and never slips, even over the longest AX.25 frame. Tolerates ±1%
  baud error and any start phase.
- The hard `|mark| > |space|` decisions are the **NRZI line symbols**, fed
  straight into `hdlc::HdlcDeframer::push_symbol` (NRZI decode + de-stuffing
  live there). A mark (1200 Hz) is NRZI line symbol 1.

## HDLC framing (`hdlc.rs`)

AX.25 v2.2 §3.6–§3.8 / ISO 3309 HDLC, operating bit-by-bit on the NRZI symbol
stream:

- **NRZI differential decode** (§3.6): a `1` data bit = *no change* of the line
  symbol, a `0` data bit = a *change*. `push_symbol` differentially decodes the
  incoming line symbols back to data bits (the first symbol has no predecessor
  and is treated as a `1`). `push_data_bit` feeds an already-decoded bit (tests
  that build a bit stream from spec octets).
- **Flag framing** (§3.8): frames are delimited by the flag octet `FLAG = 0x7E`
  (`01111110`). Flag detection is done on the bit level via a running 1-count —
  the flag is the only place six consecutive 1s appear.
- **Bit de-stuffing** (§3.7): a `0` following exactly five 1s is a stuffed bit
  and is dropped; a `0` following exactly six 1s is a flag boundary; seven+
  consecutive 1s is an abort/idle (drops any partial frame).
- Recovered data bits are assembled **LSB-first** into octets. A completed
  frame is emitted only when it has at least 3 octets (so stray flag pairs do
  not emit empties); the AX.25 parser + FCS reject the rest.

Encode helpers (`frame_bits`, `nrzi_encode`) exist for the modulator/tests
only and are the inverse of the deframer.

## AX.25 v2.2 UI framing (`ax25.rs`)

`parse_frame` operates on an already-deframed, bit-unstuffed octet sequence
(address…control PID info FCS) per AX.25 v2.2:

- **Address field** (§3.12 / §3.12.2): each callsign subfield is 7 octets — 6
  callsign chars as **ASCII shifted left one bit** (`C << 1`), space-padded, then
  an SSID octet `0x60 | (ssid << 1) | ext` with the C/H bit in bit 7. The HDLC
  address-extension bit (LSB) of every address octet is 0 **except the last
  octet of the whole address field, whose LSB is 1**; the parser walks subfields
  until that bit is set (dest + source + up to 8 digipeaters, max 10 subfields).
  `Address` records `callsign`, `ssid` (0..15), and `h_or_c` (the C/has-been-
  repeated bit). `Address::display()` renders TNC-2 form: `CALL` or `CALL-SSID`,
  with a trailing `*` when the has-been-repeated H-bit is set.
- **Control field** (§3.13): a UI frame uses control `0x03` (modulo-8). The
  parser accepts the P/F bit set too (mask `control & 0xEF == 0x03`, i.e.
  `0x03` or `0x13`); a non-UI control rejects the frame.
- **PID field** (§3.14): `0xF0` = no layer-3 protocol (what APRS uses).
- **Frame Check Sequence** (§3.9): the FCS is the 16-bit ISO 3309 / CCITT
  (X.25 / HDLC) CRC — poly 0x1021, reflected, init 0xFFFF, complemented,
  transmitted low-order byte first — checked via `xng_dsp::checksum::hdlc_frame_ok`
  (CRC-16/X-25, reused; no new CRC implementation). A bad FCS sets
  `fcs_ok = false` but does not by itself reject the frame; the caller
  (`AprsChannelDecoder`, `require_fcs`) decides whether to keep CRC-failed
  frames.

`Ax25Frame` carries `dest`, `source`, `via[]`, `control`, `pid`, `info`, and
`fcs_ok`. `is_aprs_ui` is true for control `0x03` (UI) + PID `0xF0`.

## APRS payload (`aprs.rs`)

`parse(info)` dispatches on the first info byte — the APRS 1.0.1 *data-type
identifier* (Chapter 5, p.17) — and always returns an `AprsPayload { kind,
fields }`; anything unrecognized falls through to `AprsKind::Raw` with the raw
text preserved. **Mic-E** (`` ` ``, `'`, 0x1c, 0x1d) is *not* dispatched here —
it carries its latitude in the AX.25 destination address, so it is decoded one
level up in `decode_frame` (see below).

| DTI | Handler | `AprsKind` |
|---|---|---|
| `!` `=` | `parse_position` (no timestamp) | `Position` |
| `/` `@` | `parse_position` (7-char timestamp follows the DTI) | `Position` |
| `_` | `parse_weather_positionless` | `Weather` |
| `:` | `parse_message` (or bulletin/announcement when addressee = `BLN…`) | `Message` / `Bulletin` |
| `>` | `parse_status` (free text, or Maidenhead grid locator) | `Status` |
| `;` | `parse_object` | `Object` |
| `)` | `parse_item` | `Item` |
| `?` | `parse_query` | `Query` |
| `T` | `parse_telemetry` | `Telemetry` |
| `` ` `` `'` 0x1c 0x1d | (Mic-E — dispatched in `decode_frame`, `mice::parse`) | `MicE` |
| other | `raw` | `Raw` |

The full `AprsKind` enum is `Position` / `Weather` / `Message` / `Status` /
`Object` / `Item` / `Telemetry` / `MicE` / `Bulletin` / `Query` / `Raw`; its
`as_str()` produces the `kind` strings `position` / `weather` / `message` /
`status` / `object` / `item` / `telemetry` / `mic-e` / `bulletin` / `query` /
`raw` that ride on `MessageBody::Aprs { kind, .. }`.

- **Position** (Chapter 6 / 9). Uncompressed: `DDMM.mmH` lat (8 chars),
  symbol-table id, `DDDMM.mmH` lon (9 chars), symbol code, then an **optional
  7-byte Data Extension** (Chapter 7) and a comment; lat/lon decode to decimal
  degrees (S/W negative). Compressed (Base-91, Chapter 9): symbol-table id,
  4-byte lat group, 4-byte lon group, symbol code, 2 `cs` bytes, 1
  **compression-type (`T`) byte**; `lat = 90 − N/380926`, `lon = −180 +
  N/190463` where N is the 4-digit Base-91 value (each char − 33, base 91). The
  form is disambiguated by the first char: a digit ⇒ uncompressed (lat DD),
  otherwise ⇒ compressed (the symbol-table id is never a digit). Emits `lat`,
  `lon`, `symbol_table`, `symbol_code`, `comment`, `compressed`, the decoded
  extension/sub-field fields (below), and optional `timestamp`.
- **Position Data Extensions** (Chapter 7, `parse_data_extension`). The
  fixed-width 7-byte field that may follow the uncompressed symbol code, decoded
  and stripped from the comment when present. Four forms are recognized by their
  leading literal / shape:
  - `CSE/SPD` = `nnn/nnn` (p.27): three course digits, `/`, three speed digits →
    `course_deg`, `speed_knots`.
  - `PHGphgd` (p.28, `decode_phg`): literal `PHG` + 4 codes → `phg_power_w` =
    p², `phg_height_ft` = 10·2^h, `phg_gain_db` = g, `phg_directivity_deg` =
    d·45 (or `"omni"` when d = 0).
  - `DFSshgd` (p.30, `decode_dfs`): literal `DFS` + 4 codes → `dfs_strength_s`,
    `dfs_height_ft` = 10·2^h, `dfs_gain_db`, `dfs_directivity_deg` (or `"omni"`).
  - `RNGrrrr` (p.29): literal `RNG` + 4-digit miles → `radio_range_miles`.
- **Compressed cs/T sub-field** (Chapter 9, `decode_compressed_cs`, p.38-40).
  The two `cs` bytes plus the compression-type `T` byte (all Base-91) select one
  of three sub-fields by the first `cs` byte `c`: a **space** ⇒ no data; `c == '{'`
  ⇒ pre-calculated radio range `2·1.08^s` miles (`radio_range_miles`); when the
  `T` byte's NMEA-source bits (4,3) = `10b` (GGA) ⇒ altitude `1.002^(c·91+s)`
  feet (`altitude_ft`); otherwise ⇒ course `c·4` deg + speed `1.08^s − 1` knots
  (`course_deg`, `speed_knots`). The `T` byte's bit-5 GPS-fix flag becomes
  `gps_fix_current`.
- **Weather** (positionless, Chapter 12): `_` + 8-char MDHM timestamp + field
  set; `parse_weather_fields` walks the `c`/`s`/`g`/`t`/`r`/`p`/`P`/`h`/`b`
  identifiers (wind dir/speed/gust, temp °F signed, rain 1h/24h/since-midnight,
  humidity %, barometric pressure 1/10 hPa) by their documented field widths.
  (`h00` is normalized to 100% humidity.)
- **Mic-E** (Chapter 10, `mice::parse`; dispatched in `decode_frame`). The most
  common on-air format, split across two AX.25 fields. The **destination
  address** (6 plain-ASCII chars, the AX.25 layer having already reversed the
  `<<1` shift) carries the six latitude digits, the 3 message bits A/B/C (one
  per char 1-3), the N/S indicator (char 4), the longitude offset +0/+100
  (char 5), and the W/E indicator (char 6) per the p.44 per-character table; the
  **info field** carries the longitude (3 bytes d/m/h + 28, p.48-49), speed +
  course (3 bytes SP/DC/SE + 28, p.52), and the symbol code + symbol-table id
  (p.46). The message bits resolve to a `message_type`/`message_class`
  (Standard `M0..M6`, Custom `C0..C6`, `Emergency`, or mixed-Std/Custom
  `Unknown`) via the p.45 table. Trailing bytes are decoded as Mic-E telemetry
  (leading `` ` ``/`'`/0x1d → `has_telemetry`) or as status text (which may carry
  a Maidenhead locator + altitude). Position ambiguity — latitude digits sent as
  spaces (dest chars `K`/`L`/`Z`) — is counted into `position_ambiguity`. An
  info field shorter than the mandatory 9 bytes (p.47) or a non-Mic-E
  destination character returns `None` and the frame falls back to the info-only
  dispatch. Emits `lat`, `lon`, `speed_knots`, `course_deg`, `symbol_code`,
  `symbol_table`, `message_type`, `message_class`, `mic_e`, `north`, `west`,
  `long_offset_100`, optional `position_ambiguity`, `status`, `has_telemetry`.
- **Message** (Chapter 14): `:ADDRESSEE:message{nnn` — 9-char space-padded
  addressee, then the message text and an optional `{` message number. Emits
  `addressee`, `message`, optional `message_number`.
- **Bulletin / announcement / group** (Chapter 14, p.73-74). A `:` message
  whose 9-char addressee is the literal `BLN` + an identifier char is split out
  to `AprsKind::Bulletin` (bulletins are not acknowledged, so they carry no
  message number). A **digit** identifier ⇒ general `bulletin`; a **letter** ⇒
  `announcement`; any trailing addressee chars after the identifier are the
  bulletin **group** name. Emits `addressee`, `bulletin_id`, `bulletin_kind`,
  `text`, optional `group`.
- **Status** (Chapter 16): `>` then free-text status → `status`. A status whose
  body is a 4- or 6-char Maidenhead grid locator (`AAnn` / `AAnngg`) immediately
  followed by a symbol-table id + symbol code (p.81-82) is decoded to
  `maidenhead` (upper-cased), `symbol_table`, `symbol_code`, and optional
  trailing `status` text; a plausibility check on the locator shape + symbol-table
  id guards against false-detecting plain free text.
- **Object** (Chapter 11): `;NAME     *DDHHMMz<position>` — 9-char name, state
  (`*` live / `_` killed), 7-char timestamp, then a position parsed by the same
  uncompressed/compressed logic (`dispatch_position_body`) and merged in. Emits
  `name`, `live`, `timestamp`, + position fields.
- **Item** (Chapter 11, p.59, `parse_item`): `)NAME!<position>` — a `)` DTI, a
  variable-length 3-9 char item name, then `!` (live) or `_` (killed), then a
  position (no timestamp) decoded by the shared `dispatch_position_body`. Emits
  `name`, `live`, + position fields.
- **General Query** (Chapter 15, p.78, `parse_query`): `?QUERYTYPE?` optionally
  followed by a target **footprint** `lat,long,radius` in floating-point degrees.
  Emits `query_type` and, when a footprint parses, `lat`/`lon`/`radius_miles`
  (or the raw `footprint` string if it does not parse as three fields).
- **Telemetry** (Chapter 13): `T#sss,a1,a2,a3,a4,a5,bbbbbbbb` — a sequence
  number, five analog values, and up to 8 digital bits. Emits `sequence`,
  `analog[]`, optional `digital[]`.

## Frequencies & space-based reception (ISS / satellites)

APRS is a single-channel protocol whose channel changes by region. The scan
plan (`src/commands/scan.rs`) lists the whole **2-meter cluster**, which fits
one 2.4 MHz capture window so a single tuner can watch all of it at once:

| Region | MHz | | Region | MHz |
|---|---|---|---|---|
| NA / SA (primary) | 144.390 | | EU / RU | 144.800 |
| New Zealand | 144.575 | | NA event/overflow | 144.990 |
| China / Taiwan | 144.640 | | Australia | 145.175 |
| Japan | 144.660 | | **ISS / satellite digipeat** | **145.825** |

70cm (446.100 MHz) and HF APRS (300-baud packet on 10.1476 / 14.1030 /
29.250 MHz) use a different band and/or modulation and are **not** in the
1200-baud VHF plan (HF needs a 300-baud path — a follow-up).

**145.825 MHz is the international ISS / satellite digipeat channel.** A frame
heard there (or via a known satellite digipeater callsign) arrived through a
spacecraft, so `to_message` tags `details.reception = "space"`:

- **Satellite from the path** (`satellite_digipeater`, crate-local): the AX.25
  `via` callsigns are matched against the well-known space digipeaters —
  `RS0ISS` / `ARISS` / `NA1SS` → **ISS (ARISS)**, `PSAT`/`PSAT2` → PSAT, etc. —
  and set `details.satellite`. This is the reliable primary identification (the
  spacecraft names itself in the path).
- **TLE / overhead correlation** (`xng::satmap`, station runtime): when an APRS
  station session carries a `receiver-pos`, the station fetches the Celestrak
  **amateur** TLE group at startup (`satmap::init_aprs`) and
  `satmap::enrich_aprs` adds `details.satellites_overhead` (the amateur
  satellites above the receiver's horizon at the reception time, name +
  elevation°, highest first) plus `satellite_likely` when the path didn't name
  one. `SatMap::overhead(lat, lon, unix)` reuses the SGP4 + TEME→ECEF machinery
  the Iridium satmap uses, computing observer-relative elevation (`user pos +
  TLE` → which bird was in view). No-op without `receiver-pos`.

## Validation / oracles

Two distinct verification regimes (see `PROVENANCE.md`):

**1. Framing / payload — spec ground truth (the real oracles).** Every
framing/payload test is anchored to an **external** reference — a spec-stated
rule or a published worked example — never an encode→decode loopback.

| Layer | Fact / vector | Oracle | How verified |
|---|---|---|---|
| HDLC bit-stuffing | a 0 stuffed after five consecutive 1 data bits | AX.25 v2.2 §3.7 | `bit_stuffing_inserts_zero_after_five_ones` builds the stuffed bit stream for octet `0x7F` (LSB-first `1,1,1,1,1,1,1,0`) and asserts a 0 was inserted after the fifth 1; `deframer_destuffs_and_recovers_octets` / `nrzi_round_trips_through_deframer` confirm the deframer inverts the stuffing + NRZI rules |
| AX.25 address octets | callsign chars = ASCII`<<1`, space-padded; SSID octet `0x60 \| (ssid<<1) \| ext`, last-octet LSB = 1 | AX.25 v2.2 §3.12 / §3.12.2 | `address_octets_match_spec_shift_rule` asserts `"APRS"` → `82 A0 A4 A6 40 40 60` and the final-octet extension bit (computed by the spec rule, not the crate's encoder); `parse_address_from_handbuilt_spec_octets` recovers `N0CALL-5` from hand-built §3.12 octets |
| Full UI frame + X.25 FCS | dest+source+digi address field, control `0x03`, PID `0xF0`, info, FCS low byte first | AX.25 v2.2 §3.12–3.14, §3.9 | `parse_full_ui_frame_from_spec_octets` hand-builds the frame from spec octets and asserts the parser recovers all fields and validates the FCS; `corrupt_info_breaks_fcs` flips an info bit and asserts the FCS fails |
| APRS payload formats | uncompressed/compressed position, message, status, object, telemetry, weather field table | APRS Protocol Reference 1.0.1 published worked examples | the `*_spec_example` tests feed each chapter's worked example (e.g. uncompressed `!4903.50N/07201.75W-` → 49.0583°N / −72.0292°W p.32; compressed `/5L!!<*e7>` → 49.5°N / −72.75°W p.38–39; message `:WU2Z     :Testing{003` p.71; object `;LEADER   *092345z…` p.58; telemetry `T#005,…` p.68; weather `_…c220s004…` p.63) and assert the decoded fields |
| Position Data Extensions (Ch.7) | CSE/SPD, PHG, DFS, RNG 7-byte extensions | APRS 1.0.1 Ch.7 worked examples | `uncompressed_course_speed_extension_p27` (`088/036` → 88° / 36 kt, stripped from comment), `phg_extension_p28` (`PHG5132` → 25 W / 20 ft / 3 dB / 90°), `dfs_extension_p30` (`DFS2360` → S2 / 80 ft / 6 dB / omni), `rng_extension_p29` (`RNG0050` → 50 mi) |
| Compressed cs/T sub-field (Ch.9) | course/speed, radio range, altitude, compression-type byte, no-data space case | APRS 1.0.1 Ch.9 p.38-40 | `compressed_course_speed_p39` (`7P` → 88° / 36.2 kt), `compressed_radio_range_p39` (`{?` → ≈20 mi), `compressed_altitude_p40` (GGA `T` byte → ≈10004 ft), `compressed_space_no_extension_p38` (space ⇒ no fields) |
| Mic-E (Ch.10) | destination-address latitude + message code + N/S/E/W + offset; info-field lon/speed/course/symbol | APRS 1.0.1 Ch.10 worked examples (p.44-53) | `dest_worked_example_p44` (`S32U6T` → 33.4273°N, M3 Returning), `message_type_examples_p46` (Std M3, Emergency), `info_field_worked_example_p53` / `parse_full_mic_e_p53` (`` `(_fn"Oj/ `` → 112.129°W, 20 kt, 251°, jeep `/j`), `speed_course_example_p52` (86 kt / 194°, both SP+28 schemes), `position_ambiguity_p54` (2 masked digits), `short_info_rejected` (< 9 bytes ⇒ `None`); `mic_e_decodes_through_full_ax25_frame` (lib.rs) routes the p.53 example through a full AX.25 UI frame |
| Item (Ch.11) | `)NAME!<pos>` live/killed item, uncompressed + compressed | APRS 1.0.1 Ch.11 p.59 | `item_spec_example_p59` (`)AID #2!…WA` → Aid Station `/A`), `item_killed_p59` (`_` ⇒ live=false), `item_compressed_p59` (`)MOBIL!\…` compressed Gas Station) |
| Bulletin / announcement / group (Ch.14) | `BLN` addressee split into bulletin (digit) vs announcement (letter) vs group | APRS 1.0.1 Ch.14 p.73-74 | `bulletin_spec_example_p73` (`BLN3` → bulletin), `announcement_spec_example_p73` (`BLNQ` → announcement), `group_bulletin_spec_example_p74` (`BLN4WX` → group "WX"), `normal_message_not_bulletin` (regression guard) |
| General query + footprint (Ch.15) | `?QUERYTYPE?` and `lat,long,radius` footprint | APRS 1.0.1 Ch.15 p.78 | `general_query_spec_examples_p78` (`?APRS?` / `?WX?` / `?IGATE?`), `query_with_footprint_p78` (`?APRS? 34.02,-117.15,0200` → 200 mi footprint) |
| Maidenhead-grid status (Ch.16) | 4/6-char locator + symbol after `>` | APRS 1.0.1 Ch.16 p.81-82 | `maidenhead_status_p82` (`>IO91SX/-` + status text), `maidenhead_status_4char_p82` (`>IO91/G`), `plain_status_not_maidenhead` (free text not misdetected) |

**2. Demod — SYNTHETIC modulate→AWGN→demod only (no real off-air IQ).** There
is **no recorded off-air APRS IQ paired with ground-truth packets**. The demod
is validated entirely by `modulate.rs` building the on-air Bell 202 AFSK-over-FM
waveform (1200 Bd, 1200/2200 Hz tones, NRZI line coding, narrowband FM) for a
KNOWN spec-derived frame, optionally adding **complex AWGN at a controlled
SNR**, and requiring the real `AprsChannelDecoder` to recover the frame
(FCS-valid, correct callsigns and APRS fields). The modulator is **not** an
external reference; these are explicitly **not** real-RF results.

- `decodes_clean_synth_iq` / `to_message_emits_aprs_body_from_synth_iq` —
  clean modulate→demod end-to-end through the full chain, asserting the AX.25
  addressing, position fields, and the `MessageBody::Aprs { kind, details }`
  bus mapping.
- `decodes_through_ddc_with_carrier_offset` — exercises the `xng_dsp::Ddc` at a
  4× capture rate and a 12 kHz carrier offset (the discriminator absorbs the
  residual offset).
- `frame_recovery_under_awgn_synth` / `frame_recovery_curve_vs_snr_synth` —
  modulate → add complex AWGN → demod over many independent noise seeds,
  measuring frame-recovery rate; asserts a high recovery rate at strong SNR
  (≥0.9 at 18 dB; essentially perfect ≥24 dB). PROVENANCE notes an observed
  (synthetic) cliff between ~10 and 6 dB SNR — a realistic AFSK1200 threshold.
  Reported as synthetic.
- `tolerates_baud_drift_synth` — recovers the frame at ±1% TX/RX baud mismatch
  (the transition-resync clock drains accumulated error at every HDLC
  transition).
- `frame_recovery_high_snr_is_perfect_synth` — at 35 dB SNR every trial yields
  an FCS-valid frame (no decoder-internal flakiness).

## Known limitations / deferred

- **No real off-air validation.** The demod's only validation is the
  self-generated modulate→AWGN→demod path; **no recorded off-air APRS IQ
  capture with ground-truth packets exists** in this crate. Reported
  recovery-vs-SNR figures are synthetic AWGN results, not real-RF sensitivity.
  (Contrast with NAVTEX/UAT, which have a real off-air fixture.)
- **No APRS-specific carrier search.** The front end relies on the DDC
  `freq_offset_hz` plus the FM discriminator; there is no automatic
  channel/carrier acquisition. The caller must point the DDC at the channel.
- **Single FCS error detection only.** The X.25 FCS *detects* corruption but
  the crate does no FEC / error correction; a frame that fails the FCS is
  dropped (unless `set_require_fcs(false)` is used to surface candidates).
- **APRS payload coverage (now broad, still not exhaustive).** The payload
  parser now covers position (uncompressed + Base-91 compressed, with Chapter 7
  course/speed, PHG, DFS and RNG data extensions and the Chapter 9 compressed
  cs/T course/speed/range/altitude sub-field), **Mic-E** (Chapter 10), weather,
  message, **bulletin/announcement/group**, status (incl. **Maidenhead grid
  locator**), object, **item (`)`)**, **general query (`?`) + footprint**, and
  telemetry. Still **not** specially parsed (fall through to `AprsKind::Raw`):
  raw GPS/NMEA (`$`), third-party traffic (`}`), station capabilities (`<`),
  user-defined / experimental formats, and reply-acks / message ack-reject
  semantics. Weather decode still extracts only the named numeric fields from
  the documented table; non-tabulated extensions are not parsed.
- **Mic-E trailing field not fully decoded.** The mandatory Mic-E fields
  (lat/lon/speed/course/symbol/message-type) are decoded, but the optional
  trailing field is only classified (telemetry vs status) and kept as raw
  `status` text — the Mic-E telemetry channels (2/5 hex or 5 binary) and an
  embedded Maidenhead-locator + altitude in the status text are not parsed out.
  Mixed Standard/Custom message bits report `message_class = "unknown"` per the
  p.45 rule.
- **Object/message edge cases.** Object and item positions reuse the shared
  position parser (so both uncompressed and compressed forms are handled now),
  but message acks/rejects are not distinguished from message text beyond the
  optional `{` message number. The Maidenhead-status detector uses a
  shape/symbol-table plausibility check; an unusual free-text status that
  happens to match `AAnn` + a symbol-table-like byte could in principle be
  mis-detected.

## Gotchas

1. NRZI is decoded in `hdlc.rs`, not `demod.rs`: the demod emits raw line
   symbols (mark = 1) and the deframer does the differential decode. A `1` data
   bit is *no change*; a `0` is a *change*.
2. Octets are assembled **LSB-first** (both the HDLC deframer and the AX.25
   callsign `C << 1` shift); do not assume MSB-first.
3. AX.25 callsign chars are ASCII shifted **left** one bit; the SSID octet's
   LSB is the HDLC extension bit (set only on the final address subfield), bit 7
   is the C/H bit, bits 4..1 are the 0..15 SSID.
4. UI control matching masks the P/F bit (`control & 0xEF == 0x03`), so both
   `0x03` and `0x13` are accepted.
5. A non-`0xF0` PID still parses as an AX.25 UI frame but the info field is kept
   as a `raw` payload (the APRS dispatch only runs for PID `0xF0`).
6. A bad FCS does **not** drop the frame at parse time — `fcs_ok` is recorded
   and `AprsChannelDecoder` filters on `require_fcs` (default true). Set it
   false to surface CRC-failed candidates.
7. The bit clock hard-resets to `TRANSITION_RESET_PHASE = 0.6` (not 0.5) on
   each transition to compensate for the trailing correlator window's ≈half-bit
   group delay; this is what places sampling at the symbol center.
8. `CHANNEL_RATE` (38400 S/s) must stay an integer multiple of 1200 Bd (32
   samples/bit) and ≥ both `2·CHANNEL_PASSBAND_HZ` (Nyquist) and `4·SPACE_HZ`
   (space-tone resolution); the `channel_rate_is_integer_bit_multiple` test
   pins this.
9. Position compressed-vs-uncompressed is disambiguated purely by the first
   char after the (optional timestamp +) DTI: a digit ⇒ uncompressed, anything
   else ⇒ compressed. The symbol-table id is never a digit.
10. **Mic-E spans two AX.25 fields** and is therefore dispatched in
    `decode_frame`, NOT in `aprs::parse` — the latitude, message code, and
    N/S/E/W + longitude-offset indicators live in the **destination address**,
    the longitude/speed/course/symbol in the info field. `mice::parse` takes the
    destination callsign already un-shifted to plain ASCII (the AX.25 layer
    reverses the `<<1`); it matches the raw ASCII dest chars against the p.44
    table. A non-Mic-E dest char or an info field < 9 bytes returns `None` and
    the frame falls back to the info-only dispatch.
11. Mic-E speed/course use two valid SP+28 encodings (p.50 note); both decode to
    the same value. Speed ≥ 800 and course ≥ 400 wrap (subtract 800 / 400) per
    the p.52 final adjustments.
12. A `:` message is a **bulletin/announcement** (and gets `AprsKind::Bulletin`,
    `kind = "bulletin"`) only when the 9-char addressee starts with the literal
    `BLN`; the bulletin-vs-announcement split is by whether the 4th char is a
    digit (bulletin) or a letter (announcement), and any chars after it are the
    group name. Ordinary messages keep `kind = "message"`.

## Key references

- **AX.25 Link Access Protocol for Amateur Packet Radio, Version 2.2**
  (TAPR / ARRL, July 1998) — address-field encoding (§3.12 / §3.12.2), HDLC
  flags / bit-stuffing / NRZI (§3.6–§3.8), control field (§3.13), PID (§3.14),
  X.25 FCS (§3.9). Facts only; no code copied. Cited inline in `src/ax25.rs`
  and `src/hdlc.rs`.
- **APRS Protocol Reference, Protocol Version 1.0.1** (Bob Bruninga et al.,
  2000) — data-type-identifier dispatch (Ch. 5, p.17), uncompressed/Base-91
  compressed position (Ch. 6 / 9), position Data Extensions — course/speed, PHG,
  DFS, RNG (Ch. 7, p.27-30), the compressed cs/T course/speed/range/altitude
  sub-field (Ch. 9, p.38-40), **Mic-E** (Ch. 10, p.42-56), object (Ch. 11, p.58)
  and **item** (Ch. 11, p.59), weather (Ch. 12, p.62-63), telemetry (Ch. 13,
  p.68), message and **bulletin/announcement/group** (Ch. 14, p.71-74),
  **general query + footprint** (Ch. 15, p.78), status and **Maidenhead-grid
  status** (Ch. 16, p.80-82). Each payload test uses the spec's published worked
  example. Cited inline in `src/aprs.rs` and `src/mice.rs`.
- **ISO 3309 / CCITT** HDLC — the bit-stuffing, flag, and X.25 FCS definitions
  underlying AX.25's link layer.
- `crates/xng-mode-aprs/PROVENANCE.md` — sourcing policy, per-table oracle
  notes, and the explicit statement that the demod is validated synthetically
  (no off-air IQ).
- `crates/xng-dsp/src/ddc.rs` (`Ddc`) and `crates/xng-dsp/src/checksum.rs`
  (`hdlc_fcs` / `hdlc_frame_ok`, CRC-16/X-25) — reused DSP primitives.
