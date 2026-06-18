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
`ax25` → `aprs` — with `AprsChannelDecoder` as the channelized IQ entry point
and `to_message` normalizing a decoded frame into the bus form
`MessageBody::Aprs { kind, details }`. The DECODE/FRAMING/PAYLOAD tests are
spec-anchored; the demod is exercised by a self-generated modulate→AWGN→demod
path and documented as synthetic.

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
  → aprs::parse             dispatch on the data-type identifier
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
  a `raw` payload). Returns `None` if it is not a parseable UI frame.

`AprsFrame` bundles the decoded `ax25: Ax25Frame`, the parsed
`payload: AprsPayload`, and `raw: Vec<u8>` (the deframed link-layer octets,
address…control PID info FCS).

`to_message(frame, frequency_hz, level_dbfs, source) -> Message` normalizes an
`AprsFrame` into the bus `Message`: `mode = Mode::Aprs`, body
`MessageBody::Aprs { kind, details }` where `kind` is the APRS data class
(`position` / `weather` / `message` / `status` / `object` / `telemetry` /
`raw`) and `details` is a JSON object merging the AX.25 addressing (`source`,
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
text preserved.

| DTI | Handler | `AprsKind` |
|---|---|---|
| `!` `=` | `parse_position` (no timestamp) | `Position` |
| `/` `@` | `parse_position` (7-char timestamp follows the DTI) | `Position` |
| `_` | `parse_weather_positionless` | `Weather` |
| `:` | `parse_message` | `Message` |
| `>` | `parse_status` | `Status` |
| `;` | `parse_object` | `Object` |
| `T` | `parse_telemetry` | `Telemetry` |
| other | `raw` | `Raw` |

- **Position** (Chapter 6 / 9). Uncompressed: `DDMM.mmH` lat (8 chars),
  symbol-table id, `DDDMM.mmH` lon (9 chars), symbol code, then an optional
  comment; lat/lon decode to decimal degrees (S/W negative). Compressed
  (Base-91, Chapter 9): symbol-table id, 4-byte lat group, 4-byte lon group,
  symbol code, 2 cs bytes, 1 compression-type byte; `lat = 90 − N/380926`,
  `lon = −180 + N/190463` where N is the 4-digit Base-91 value (each char − 33,
  base 91). The form is disambiguated by the first char: a digit ⇒ uncompressed
  (lat DD), otherwise ⇒ compressed (the symbol-table id is never a digit). Emits
  `lat`, `lon`, `symbol_table`, `symbol_code`, `comment`, `compressed`, and
  optional `timestamp`.
- **Weather** (positionless, Chapter 12): `_` + 8-char MDHM timestamp + field
  set; `parse_weather_fields` walks the `c`/`s`/`g`/`t`/`r`/`p`/`P`/`h`/`b`
  identifiers (wind dir/speed/gust, temp °F signed, rain 1h/24h/since-midnight,
  humidity %, barometric pressure 1/10 hPa) by their documented field widths.
  (`h00` is normalized to 100% humidity.)
- **Message** (Chapter 14): `:ADDRESSEE:message{nnn` — 9-char space-padded
  addressee, then the message text and an optional `{` message number. Emits
  `addressee`, `message`, optional `message_number`.
- **Status** (Chapter 16): `>` then free-text status → `status`.
- **Object** (Chapter 11): `;NAME     *DDHHMMz<position>` — 9-char name, state
  (`*` live / `_` killed), 7-char timestamp, then a position parsed by the same
  uncompressed/compressed logic and merged in. Emits `name`, `live`,
  `timestamp`, + position fields.
- **Telemetry** (Chapter 13): `T#sss,a1,a2,a3,a4,a5,bbbbbbbb` — a sequence
  number, five analog values, and up to 8 digital bits. Emits `sequence`,
  `analog[]`, optional `digital[]`.

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
- **Partial APRS payload coverage.** The payload parser covers the common data
  classes (position uncompressed + Base-91 compressed, weather, message,
  status, object, telemetry). Less-common DTIs (e.g. Mic-E, item reports, raw
  GPS/NMEA, third-party traffic, bulletins, capabilities/query) are not
  specially parsed and fall through to `AprsKind::Raw`. Weather decode extracts
  the named numeric fields from the documented table; non-tabulated extensions
  are not parsed.
- **Object/message edge cases.** Object position reuses the position parser;
  compressed-in-object and item (`)`) reports are not separately handled.
  Message acks/rejects are not distinguished from message text beyond the
  optional `{` message number.

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

## Key references

- **AX.25 Link Access Protocol for Amateur Packet Radio, Version 2.2**
  (TAPR / ARRL, July 1998) — address-field encoding (§3.12 / §3.12.2), HDLC
  flags / bit-stuffing / NRZI (§3.6–§3.8), control field (§3.13), PID (§3.14),
  X.25 FCS (§3.9). Facts only; no code copied. Cited inline in `src/ax25.rs`
  and `src/hdlc.rs`.
- **APRS Protocol Reference, Protocol Version 1.0.1** (Bob Bruninga et al.,
  2000) — data-type-identifier dispatch (Ch. 5, p.17), uncompressed/Base-91
  compressed position (Ch. 6 / 9), weather (Ch. 12), message (Ch. 14), status
  (Ch. 16), object (Ch. 11), telemetry (Ch. 13). Each payload test uses the
  spec's published worked example. Cited inline in `src/aprs.rs`.
- **ISO 3309 / CCITT** HDLC — the bit-stuffing, flag, and X.25 FCS definitions
  underlying AX.25's link layer.
- `crates/xng-mode-aprs/PROVENANCE.md` — sourcing policy, per-table oracle
  notes, and the explicit statement that the demod is validated synthetically
  (no off-air IQ).
- `crates/xng-dsp/src/ddc.rs` (`Ddc`) and `crates/xng-dsp/src/checksum.rs`
  (`hdlc_fcs` / `hdlc_frame_ok`, CRC-16/X-25) — reused DSP primitives.
