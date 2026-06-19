# Provenance — xng-mode-aprs

Clean-room implementation of the APRS / AX.25 packet-radio receive stack. No
decoder code was copied or ported; only protocol facts, the published
specifications, and their worked examples were used, each cited below. Every
DECODE/FRAMING/PAYLOAD test is anchored to an **external** reference (a
spec-stated rule or a published worked example) — none is an encode→decode
self-consistency loopback. The demod is validated **synthetically** and is
documented as such.

## What this crate is

APRS on VHF (144.39 MHz North America, 144.800 MHz Europe) is **Bell 202
AFSK** — 1200 Hz mark / 2200 Hz space tones keyed at 1200 baud — carried in
**narrowband FM**, framed as **AX.25 v2.2 Unnumbered-Information (UI)** packets
whose information field is an **APRS Protocol Reference 1.0.1** payload.

The receive stack, bottom-up:

- `demod` — FM discriminator → Bell 202 AFSK1200 non-coherent dual-tone
  correlator → transition-resync bit clock, emitting NRZI line symbols.
- `hdlc` — NRZI differential decode, HDLC bit de-stuffing, `0x7E` flag
  framing.
- `ax25` — AX.25 v2.2 UI frame parsing (addresses, control, PID, X.25 FCS).
- `aprs` — APRS 1.0.1 payload dispatch on the data-type identifier.

`AprsChannelDecoder` is the channelized IQ entry point: an `xng_dsp::Ddc`
mixes a wideband capture by the channel offset and decimates to
`CHANNEL_RATE` (38400 S/s, 32 samples/bit), then the pipeline above runs and
emits one `AprsFrame` per recovered packet. `to_message` normalizes a frame
into the `xng_types` bus form (`MessageBody::Aprs { kind, details }`).

## Sources (protocol facts / worked examples only)

### AX.25 link layer — AX.25 Link Access Protocol v2.2 (TAPR/ARRL, 1998)

The address-field encoding, control/PID octets, and FCS are taken from the
AX.25 v2.2 specification, cited inline at `src/ax25.rs`:

- **§3.12 The Address Field** — each callsign subfield is the 6 ASCII
  callsign characters shifted left one bit (`C << 1`), space-padded to 6,
  followed by an SSID octet; the HDLC address-extension bit (LSB) of every
  address octet is 0 except the **last** octet of the whole address field,
  whose LSB is 1.
- **§3.12.2 SSID** — the SSID octet is `0x60 | (ssid << 1) | ext`, with the
  C/H bit in bit 7.
- **§3.13 Control Field** — a UI frame uses control `0x03`.
- **§3.14 PID Field** — `0xF0` = no layer-3 protocol (APRS).
- **§3.9 Frame Check Sequence** — the FCS is the 16-bit ISO 3309 / CCITT
  (X.25 / HDLC) CRC: poly 0x1021, reflected, init 0xFFFF, complemented,
  transmitted low-order byte first. This is exactly `xng_dsp::checksum`'s
  `hdlc_fcs` (CRC-16/X-25), reused here — no new CRC implementation.

The `src/ax25.rs` tests hand-build the exact octets from §3.12–§3.14 (e.g.
`"APRS"` → `82 A0 A4 A6 40 40 60`, computed by the spec's `C<<1` rule, NOT by
this crate's encoder) and assert the parser recovers callsign / SSID /
digipeater path / control / PID / info, and that the X.25 FCS validates and a
corrupted octet breaks it.

### HDLC framing — AX.25 v2.2 §3.6–§3.8 / ISO 3309

The `0x7E` flag, bit-stuffing (a 0 stuffed after five consecutive 1 data
bits), NRZI line coding (0 = transition, 1 = hold), and LSB-first octet
assembly follow AX.25 §3.6–§3.8, cited inline at `src/hdlc.rs`. The
`bit_stuffing_inserts_zero_after_five_ones` test asserts the spec stuffing
rule directly on the bit stream; the de-stuff / NRZI tests confirm the
deframer inverts that rule.

### APRS payload — APRS Protocol Reference, Protocol Version 1.0.1 (2000)

The data-type-identifier dispatch and each payload format are taken from the
APRS 1.0.1 reference, cited inline at `src/aprs.rs` with chapter/page numbers.
Every payload test uses the **published worked example** from the spec as the
oracle:

- **Chapter 6, p.32** uncompressed position: `!4903.50N/07201.75W-` →
  49°03.50′N (49.0583°), 072°01.75′W (−72.0292°), primary symbol table,
  symbol `-`. (Asserted in `uncompressed_position_spec_example`.)
- **Chapter 9, p.38–39** Base-91 compressed position: groups `5L!!` / `<*e7`
  decode via lat = 90 − N/380926, lon = −180 + N/190463 to 49.5°N / −72.75°W.
  (Asserted in `compressed_position_spec_example`; the conversion constants
  are the spec's.)
- **Chapter 9, p.38–40** compressed course/speed, radio-range and altitude
  sub-fields + the compression-type `T` byte (`src/aprs.rs::decode_compressed_cs`):
  cs `7P` → course 88°, speed 36.2 kt (`compressed_course_speed_p39`); cs `{?`
  → radio range 20 mi (`compressed_radio_range_p39`); cs `S]` with a GGA `T`
  byte → altitude 10004 ft (`compressed_altitude_p40`); the `c = space`
  special case → no sub-field (`compressed_space_no_extension_p38`).
- **Chapter 7, p.27–30** position Data Extensions (`src/aprs.rs::parse_data_extension`):
  `088/036` course/speed (`uncompressed_course_speed_extension_p27`); `PHG5132`
  → power 25 W, height 20 ft, gain 3 dB, directivity 90° E (`phg_extension_p28`,
  the p.29 worked example); `DFS2360` → strength S2, height 80 ft, gain 6 dB,
  omni (`dfs_extension_p30`, the p.30 worked example); `RNG0050` → 50 mi
  (`rng_extension_p29`).
- **Chapter 10, p.42–55** Mic-E (`src/mice.rs`) — the single biggest format gap
  on real APRS traffic. The destination-address worked example on p.44
  (`S32U6T` → 33°25.64′N, North, offset +0, West, message bits 1/0/0 = Standard
  M3 Returning) is asserted in `dest_worked_example_p44`; the message-type
  examples on p.46 in `message_type_examples_p46`; the information-field worked
  example on p.53 (`` `(_fn"Oj/ `` → 112°07.74′W, 20 kt, course 251°, jeep `/j`)
  in `info_field_worked_example_p53` / `parse_full_mic_e_p53`; the speed/course
  worked example on p.52 (86 kt, 194°, both SP+28/DC+28 encoding schemes) in
  `speed_course_example_p52`; the p.54 position-ambiguity example (`T4SQZZ`) in
  `position_ambiguity_p54`. The full path through a real AX.25 UI frame (Mic-E
  packs the latitude into the AX.25 **destination address**, so it is decoded
  at the `decode_frame` level, joining the destination callsign to the info
  field) is asserted in `lib.rs::mic_e_decodes_through_full_ax25_frame`.
- **Chapter 11, p.59** Item Report (`src/aprs.rs::parse_item`): `)AID #2!4903.50N/07201.75WA`
  → item "AID #2", live, 49°03.50′N/072°01.75′W, symbol `/A`
  (`item_spec_example_p59`); the killed (`_`) variant (`item_killed_p59`); and
  the compressed-position item `)MOBIL!\5L!!<*e79_sT` (`item_compressed_p59`).
- **Chapter 14, p.71** message: `:WU2Z     :Testing{003` → addressee WU2Z,
  text "Testing", message number 003.
- **Chapter 14, p.73–74** bulletins / announcements / group bulletins
  (`src/aprs.rs::parse_message` BLN detection): `:BLN3     :Snow expected in
  Tampa RSN` general bulletin (`bulletin_spec_example_p73`); `:BLNQ     :…`
  announcement, letter id (`announcement_spec_example_p73`); `:BLN4WX   :Stand
  by your snowplows` group bulletin (`group_bulletin_spec_example_p74`).
- **Chapter 15, p.78** general query (`src/aprs.rs::parse_query`): `?APRS?`,
  `?WX?`, `?IGATE?` (`general_query_spec_examples_p78`); `?APRS?
  34.02,-117.15,0200` target footprint (`query_with_footprint_p78`).
- **Chapter 16, p.80** status: `>Net Control Center`.
- **Chapter 16, p.81–82** status with Maidenhead grid locator
  (`src/aprs.rs::parse_maidenhead_status`): `>IO91SX/-` (+ ` My house` status
  text) (`maidenhead_status_p82`) and the 4-char `>IO91/G`
  (`maidenhead_status_4char_p82`); a plain free-text status is **not**
  misdetected (`plain_status_not_maidenhead`).
- **Chapter 11, p.58** object: `;LEADER   *092345z4903.50N/07201.75W>`.
- **Chapter 13, p.68** telemetry: `T#005,199,000,255,073,123,01101001`.
- **Chapter 12, p.63** weather field table: wind/temp/humidity/pressure
  identifiers (`c`,`s`,`g`,`t`,`r`,`p`,`P`,`h`,`b`).

`MessageBody::Aprs { kind, details }` is emitted with `kind` ∈
{`position`, `weather`, `message`, `status`, `object`, `item`, `telemetry`,
`mic-e`, `bulletin`, `query`, `raw`} and `details` a JSON object merging the
AX.25 addressing (`source`, `dest`, `via[]`, displayed in TNC-2 `CALL-SSID`
form) with the decoded APRS fields (`lat`, `lon`, `symbol_table`,
`symbol_code`, `comment`, course/speed, `phg_*`/`dfs_*`, `altitude_ft`,
`radio_range_miles`, …). No new `Mode`/`MessageBody` variant is introduced —
the new formats are additional `kind` strings under the existing
`MessageBody::Aprs`, so no bus rewiring is needed.

## Demod validation — SELF-GENERATED modulate→AWGN→demod (synthetic)

No off-air APRS IQ capture paired with ground-truth packets is available
here, so the demod is validated **synthetically**: `src/modulate.rs` builds
the on-air Bell 202 AFSK-over-FM waveform (1200 Bd, 1200/2200 Hz tones, NRZI
line coding, narrowband FM) for a KNOWN frame (assembled from the spec-rule
AX.25 octets), optionally adds **complex AWGN at a controlled SNR**, and the
real `AprsChannelDecoder` must recover the frame (FCS-valid) with the correct
callsigns and APRS fields. The tests are in `tests/end_to_end.rs`:

- `decodes_clean_synth_iq`, `to_message_emits_aprs_body_from_synth_iq` —
  clean modulate→demod end-to-end.
- `decodes_through_ddc_with_carrier_offset` — through the `xng_dsp::Ddc` at a
  4× capture rate and a 12 kHz carrier offset.
- `frame_recovery_under_awgn_synth` / `frame_recovery_curve_vs_snr_synth` —
  the one allowed synthetic test: modulate → add complex AWGN → demod,
  measuring frame-recovery rate over many independent noise realizations.
  Observed (synthetic): 100% recovery down to ~10 dB SNR, cliff between 10 and
  6 dB — a realistic AFSK1200 threshold.
- `tolerates_baud_drift_synth` — recovers the frame at ±1% TX/RX baud
  mismatch (the transition-resync clock drains accumulated error at every
  HDLC transition).

This proves only that the demod inverts the standard Bell 202 AFSK-over-FM
modulation, including through the DDC and under noise. The modulator is **not**
an external reference and these are **not** real-RF results. The FRAMING and
APRS-payload layers remain oracle-anchored by their own spec-cited tests, so
the synthetic demod path does not weaken those guarantees. The waveform
parameters themselves (1200 Bd, 1200/2200 Hz Bell 202, narrowband FM) are the
published on-air APRS spec.

## DSP reuse

The crate reuses `xng_dsp` primitives rather than reinventing them:
`xng_dsp::Ddc` (NCO mix + decimating anti-alias FIR) for channelization and
`xng_dsp::checksum::{hdlc_fcs, hdlc_frame_ok}` (CRC-16/X-25) for the AX.25
FCS. The FM discriminator and the dual-tone AFSK correlator are textbook
non-coherent FSK detection (clean-room).
