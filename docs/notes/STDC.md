# Inmarsat STD-C / EGC — implementation notes

Native Inmarsat-C NCS common-channel (TDM) decode for `xng-mode-stdc`.
Coherent BPSK at 1200 sym/s, full PHY → frame → packet → application
chain. Clean-room: protocol facts cross-verified across inmarsatc
(GPL-3), SatDump (GPL-3) and Scytale-C (GPL-3) and numerically
re-verified; **all code is re-derived** (see PROVENANCE.md). Field-decode
tables are typed verbatim from inmarsatc (facts only) and the IMO
International SafetyNET Manual.

Result: oracle-validated field-exact on the public sigidwiki Inmarsat-C
TDM/EGC IQ capture (AOR-E). The full native chain decodes one frame's
worth of packets (51 in the source recording; ≥5 in the vendored 14 s
slice) — bulletin boards with consecutive TDM frame numbers, logical-
channel announcements with MES IDs and named LES routing, signalling-
channel descriptors and confirmations — every packet checksum passing.
No count-style benchmark (no peer decoder is run head-to-head); fenced by
the off-air fixture + RF-loopback tests in CI. Source:
`crates/xng-mode-stdc/src/`.

## Pipeline

wideband IQ → `xng_dsp::Ddc` → 12 kHz channel IQ → `demod::BpskDemod`
(coarse AFC, Costas, Gardner) → `frame::FrameDecoder` (UW sync both
polarities, depermute, deinterleave, Viterbi, descramble) →
`packet::PacketParser` (Fletcher checksum, EGC/LCN/multiframe assembly)
→ `xng_types::Message::StdC`. `CHANNEL_RATE` = 12 kS/s (10 samples/sym);
one-sided passband 2 kHz (DDC bypassed when input is already 12 kHz at
zero offset).

## PHY / demod

- NCS carriers (continuous): AOR-W 1537.70 MHz, IOR 1537.10,
  AOR-E 1541.45, POR 1541.45.
- BPSK 1200 sym/s, **coherent** (not differential despite some wiki
  labels). `demod.rs`: square-law FFT coarse frequency acquisition
  (8192-pt FFT on x², tone at 2× carrier offset; snaps the NCO only when
  the estimate is >4 bins from the current frequency, preserving the
  Costas fine correction), decision-directed Costas loop (phase gain
  0.05, freq gain 0.002), Gardner timing (gain 0.02, ±0.08 clamp).
- Gardner is **gated on carrier lock** (EMA of |Costas error| < 0.4):
  while the carrier spins, Gardner errors random-walk the clock into
  symbol slips that corrupt whole 10368-symbol frames. The tight timing
  clamp ensures a spike can never accumulate into a slip within a frame.
- 180° BPSK ambiguity is **not** resolved in the demod — it is resolved
  at the frame layer by correlating the UW in both polarities and
  complementing the frame when the inverted UW wins.
- Known demod limit (PROVENANCE): cold-start timing acquisition on an
  unfiltered direct-injection signal is weak; the Gardner loop needs the
  receive-path DDC filtering and a few seconds of the continuous carrier
  to converge — which deployment always provides. Loopback tests
  prepend settling symbols to mirror this.

## Frame structure (`frame.rs`)

- Frame = 10368 symbols = 8.64 s exactly; 64 rows × 162 columns sent
  row by row (2 UW symbols + 160 data symbols per row). Frame number
  0..9999 resets at UTC midnight.
- **UW** = `07 EA CD DA 4E 2F 28 C2` (64 bits, each bit sent twice at
  row start, MSB-first). Sync threshold ≥121/128 matching symbol pairs
  (`UW_MIN_MATCH`) over a sliding window, scored in both polarities.
- **Row depermute**: original row i was transmitted as row (i·23) mod 64
  on RX (TX uses the inverse (j·39) mod 64).
- **Deinterleave**: strip the 2 UW columns, read the 64×160 matrix
  column-wise → 10240 soft symbols.
- **FEC**: K=7 r=1/2 convolutional, 171/133 octal, shared `xng_dsp`
  Viterbi. **Coded-pair order is 133-output first** — the off-air
  finding (same as Aero and HFDL): with 171-first the frame decodes to
  pseudorandom bytes and no packet checksum passes; with 133-first every
  packet validates. 10240 → 5120 bits = 640 bytes (639 info + 1 flush,
  trellis ends in state 0).
- **Bit packing**: decoded bits pack **LSB-first per byte** (equivalent
  to KA9Q chainback + per-byte bit reversal in the GPL references).
- **Descrambler**: 640 bytes = 160 four-byte groups; 7-bit LFSR
  G = 1 + x³ + x⁴ + x⁵ + x⁷, **init 0x80** (docs saying 0x40 are
  wrong); one output bit per group, bit=1 → XOR the 4 bytes with 0xFF.
  Self-inverse (same routine used by the TX encoder).
- `encode_frame` is the full inverse TX chain (scramble → conv encode →
  column-write/inverse-permute → doubled UW per row), exercised by
  round-trip and error-injection unit tests.

## Packet layer (`packet.rs`, within the 640-byte frame)

- Descriptor sizing: `0xxxxxxx` short — len = (b & 0x0F) + 1;
  `10xxxxxx` medium — len = byte[1] + 2; `11xxxxxx` long —
  len = (b1<<8|b2) + 3. Descriptor 0x00 = padding (stop).
- **Checksum** (last 2 bytes, ISO-8473 / Fletcher style): C0 += B,
  C1 += C0 over the packet with the two checksum bytes zeroed;
  CB1 = u8(C0 − C1), CB2 = u8(C1 − 2·C0). A transmitted `00 00` is
  accepted (re-encapsulated multiframe content). Packets failing the
  checksum are silently skipped; only checksum-valid packets emit.

### Packet types decoded (descriptor → name + fields)

The C-channel descriptor field depth is typed verbatim from inmarsatc's
`decode_*` functions (facts only; re-derived — see PROVENANCE "STDC-2").
Each descriptor surfaces only the fields actually present in the packet
(short forms fall back to the bare name).

- **0x7D bulletin-board**: network version, frame number (BE [2..3]),
  **UTC-of-day** (frame × 8.64 s), `signalling_channel`, `count`,
  `channel_type` + `channel_type_name` (1 NCS / 2 LES TDM / 3 joint /
  4 ST-BY NCS), `local`, NCS `sat_les`, decoded `status` flags
  (bauds_600 / operational / in_service / clear / links_open) and the
  16-bit `services` list.
- **0x27 logical-channel-clear**: MES id (24-bit), sat/LES, LCN —
  terminates the LCN and flushes its assembled message.
- **0x81 announcement**: MES id, sat/LES, LCN.
- **0x83 logical-channel-assignment**: MES id, sat/LES, status_bits,
  LCN, frame_length, duration, downlink/uplink **MHz**, frame_offset,
  packet_descriptor1 — enough to actually tune the message channel.
- **0xAA message-data**: sat/LES, LCN, packet sequence — payload bytes
  buffered per logical channel for reassembly.
- **0xB0 / 0xB1 / 0xB2 EGC** single / double-header parts (see below);
  surface only as the assembled `egc-message`.
- **0xBD / 0xBE multiframe** start / continue — reassembled into a byte
  stream and **re-parsed recursively** through the packet walker.
- **0x6C signalling-channel**: 8-bit `services` byte, uplink
  channel-number word → **uplink MHz** = (word − 6000)·0.0025 + 1626.5
  (downlink helper: (word − 8000)·0.0025 + 1530.5), and the 28-entry
  `tdm_slots` array (4 two-bit codes per byte).
- **0x92 login-ack**: login-ack length, LES id, downlink MHz, station
  start, and (when the list is present) station count + a `stations`
  directory (6-byte records: sat/LES, services, downlink MHz).
- **0xA8 confirmation**: MES id, sat/LES, short-message length, and the
  short IA5 message text when present.
- **0xAB les-list**: list length, station start/count, full `stations`
  directory (same 6-byte record layout as login-ack).
- **0x08 ack-request**: sat/LES, LCN, uplink MHz (ship's return channel).
- **0xA3 individual-poll**: MES id, sat/LES, and the short IA5 message
  text when the packet is long enough (inmarsatc: packetLength ≥ 38).
- Flagged-only (name, no extra fields): 0x2A inbound-message-ack, 0x91
  distress-alert-ack, 0x9A enhanced-data-report-ack, 0xA0
  distress-test-request, 0xAC request-status, 0xAD test-result.
  Anything else → `unknown` (hex).

### EGC header (0xB0/B1/B2, common layout)

[2] service code; [3] bit7 continuation, bits6-5 priority (routine /
safety / urgency / distress), bits4-0 repetition; [4-5] message sequence
(BE); [6] packet sequence; [7] presentation; [8..] address (length by
service code) then payload; 2-byte checksum.

- **Address length by service code**: 0x00→3; 0x02,0x72→5;
  0x04,0x14,0x24,0x34,0x44→7; 0x11,0x31→4; 0x13,0x23,0x33,0x73→6;
  default 3.
- **Service codes named** (short + canonical long name from inmarsatc
  `getServiceCodeAndAddressName` / IMO SafetyNET): 0x00 all-ships,
  0x02 FleetNET group-call, 0x04 SafetyNET rect warning, 0x11 Inmarsat
  system, 0x13 coastal warning, 0x14 distress-circ, 0x23 EGC system,
  0x24 warning-circ, 0x31 NAVAREA/METAREA warning, 0x33 download
  group-id, 0x34 SAR-rect, 0x44 SAR-circ, 0x72 FleetNET chart
  correction, 0x73 SafetyNET chart correction.
- **Geographic area address — classified _and_ decoded** (STDC-1 /
  STDC-1.1 / STDC-1.2, `area_shape` / `egc_area` + `*_geom`): the C2
  service code is classified into its addressing shape and documented C3
  field layout (per the IMO International SafetyNET Manual 2019 Annex 4
  part A §5.2–5.3), **and** the on-air binary C3 address code is now
  decoded into machine-readable geometry. The structured `details["area"]`
  object carries `shape`, `c2`, the `c3_format` digit-layout string, the
  raw `address_payload_hex` (the leading C2-repeat byte stripped), and a
  nested `geometry` object — see the EGC geometry table below.
- Assembly (`push_egc`): keyed by message sequence; parts ordered by
  pkt_seq·2 + (part==2); complete when a terminating part arrives
  (single header, or part 2) with continuation cleared; entries age out
  after 8 frames.

### EGC geographic area geometry (STDC-1.1 / STDC-1.2, `egc_area`)

The on-air binary C3 address code is decoded into signed degrees /
nautical miles and emitted as `details["area"]["geometry"]`. Layout is
read on the C3 payload (the address field with the leading C2-repeat byte
stripped). **This is the only known open decode of the C3 binary** —
inmarsatc, SatDump, sdrangel and inmarsat-sniffer all carry the EGC
address as raw bytes (each marks the area decode "TODO" / `lat = NaN`).

| Shape | C2 | On-air C3 bytes (post C2-repeat) | `geometry` fields emitted |
|---|---|---|---|
| Rectangular | 04, 34 | `[0]` bit7 N(0)/S(1) ∣ bits6-0 SW-lat°; `[1]` SW-lon°; `[2]` bit7 E(0)/W(1) ∣ bits6-0 north extent NM; `[3]` east extent NM | `sw_corner.{lat_deg,lon_deg}` (signed), `north_extent_nm`, `east_extent_nm`, `lat_hemisphere`, `lon_hemisphere` |
| Circular | 14, 24, 44 | `[0]` bit7 N/S ∣ bits6-0 centre lat°; `[1]` centre lon°; `[2]` bit7 E/W ∣ bits6-0 radius hi; `[3]` radius lo (15-bit NM) | `center.{lat_deg,lon_deg}` (signed), `radius_nm`, `lat_hemisphere`, `lon_hemisphere` |
| NAVAREA/METAREA | 31 | `[0]` area number 1–21 | `area_number`, `area_roman` (e.g. "XII"), `coordinator` |
| Coastal / NAVTEX | 13, 73 | `[0]` area number; `[1]` coastal-area letter A–Z; `[2]` subject indicator | `area_number`, `area_roman`, `coordinator`, `coastal_area`, `subject_indicator`, `subject` |
| All-ships | 00 | — | (no geometry; shape only) |

- **Signs**: latitude bit7 set → south (negative `lat_deg`); the longitude
  hemisphere bit lives in C3 byte `[2]` bit7 (set → west, negative
  `lon_deg`). Both the signed degrees **and** the explicit
  `lat_hemisphere`/`lon_hemisphere` strings are surfaced.
- **Units note**: the manual's *MSI-provider* rectangular C3 _string_
  states the extent in degrees (worked example `60N010W30025` = 30°/25°),
  but the LES re-encodes the on-air binary field in **nautical miles**
  (Scytale-C). The raw on-air integer (`*_extent_nm` / `radius_nm`) and
  the corner/centre degrees are both surfaced so a map layer plots without
  re-deriving the packing.
- **Coastal subject indicator** (byte `[2]`, IMO Manual Annex 4 §5.3/§3.3):
  `A` navigational-warnings, `L` other-navigational-warnings, `B`
  meteorological-warnings, `E` meteorological-forecasts.
- **NAVAREA/METAREA coordinator** table (issuing authority for area 1–21)
  is verbatim from Scytale-C `ReturnNavMetAreaCoordinator`.

### Text / presentation decode (`decode_payload`)

- Presentation **0 = IA5** — one char per byte, top bit masked,
  non-printable → `·`.
- Presentation **6 = ITA2 / Baudot** (STDC-6) — one 5-bit code per
  on-air byte with LTRS (0x1F) / FIGS (0x1B) shift; international ITU-T
  ITA2 alphabet tables (`ITA2_LTRS` / `ITA2_FIGS`). No open decoder
  (inmarsatc, SatDump) implements this.
- Unknown presentation → IA5 when the payload is ≥85 % printable 7-bit
  (`looks_textual`, the pragmatic inmarsatc test), else raw hex.

### Logical-channel message reassembly

0xAA payloads buffer per LCN; the 0x27 channel-clear flushes them — parts
sorted by sequence, concatenated, text-decoded via the heuristic path,
emitted as a `message` event. Stale channels age out after 8 frames.

### Field-decode tables (oracles)

- **frame_number → UTC-of-day** and **channel-frequency formulas**:
  deterministic; cross-checked against inmarsatc `decode_7D` /
  `uplinkChannelMhz` / `downlinkChannelMhz`. Validated on the real
  capture (frame 5987 → 14:22:07; uplink word 0x2748 → 1636.64 MHz,
  inside the L-band uplink band).
- **Ocean region** (sat/LES bits 7-6: AOR-W / AOR-E / POR / IOR) +
  short/long names; **LES/NCS operator name** keyed on the full
  region×100+id display code (the same id maps to different operators by
  region) — both verbatim from inmarsatc `getSatName` / `getLesName`.
- **C-channel descriptor field maps (STDC-2)**: per-descriptor byte
  layouts typed verbatim from inmarsatc `decode_6C` / `decode_7D` /
  `decode_83` / `decode_92` / `decode_AB` / `decode_A3` / `decode_A8` /
  `decode_08` / `getStations`; **services bit→name** tables
  (`services_short` 8-bit, `services_full` 16-bit) from inmarsatc
  `getServices_short` / `getServices`; **channel-type name** and the
  **0x7D status-byte flag names** from `decode_7D`. Two transcription
  bugs in the inmarsatc C++ are fixed here: `getStations` reads the
  downlink byte twice (the field is a two-byte word), and `decode_7D`'s
  channelType `switch` omits the `break`s (so its name always falls
  through to "Reserved"); the intended per-value names are used. Channel
  frequencies reuse the off-air-validated uplink/downlink formulas. The
  descriptor maps without a public real-byte sample are pinned by
  spec-derived packets built to the exact inmarsatc byte layout (not
  encode→decode loopbacks).
- **EGC service long names** — verbatim from inmarsatc
  `getServiceCodeAndAddressName`.
- **EGC C3 geometry binary packing** (STDC-1.1/1.2): oracle is
  **Scytale-C** `PacketDecoderGeoUtils.cs` (`ReturnRectangularArea` /
  `ReturnCircularArea` / `ReturnNavArea`), whose own cited bibliography is
  the IMO/USCG International SafetyNET Manual; Scytale-C is the upstream
  origin of the inmarsatc reference this crate already cross-verifies
  against (facts only; re-derived in Rust). Verified against the manual's
  published worked examples — rectangular `60N010W30025` (SW 60°N 010°W,
  30 NM N, 25 NM E), circular `56N034W035` (centre 56°N 034°W, r 35 NM)
  and body example `14N 66W 300` (centre 14°N 66°W, r 300 NM) — each
  re-encodes bit-exact through the Scytale-C layout and is pinned as an
  inline test vector. A southern/eastern case (38°S 164°E, r 999 NM)
  pins both negative-lat and positive-lon paths.

## Output

`to_message` maps each packet to `Message::StdC { name, text, details }`
with `Mode::StdC`, `crc_ok = checksum_ok`, RSSI from the demod level, raw
bytes preserved. `details` is JSON carrying the decoded fields above.

## Validation / oracles

- **Off-air** (`tests/offair.rs`): the sigidwiki Inmarsat-C TDM/EGC IQ
  recording (CC BY-SA, AOR-E, TDM carrier +216 Hz). The full native
  chain decodes the real frame — UW scored 128/128 on the first frame;
  bulletin board frame 5987 → 14:22:07, announcement LES → "Vizada-
  Telenor, Norway" (AOR-E, region_long "Atlantic Ocean Region East"),
  0x6C → 1636.64 MHz uplink. The deepened descriptors decode self-
  consistently on the real bytes: 0x7D channel_type 1 = NCS, sat/LES =
  AOR-E NCS station (les 144), status operational + in-service, services
  including SafetyNet/InmarsatC; the same 0x6C carries services byte 0xB4
  and a 28-entry TDM-slot array. A 14 s slice is vendored as a CI fixture
  (`tests/data/stdc_egc_14s.i16`, 24 kHz I/Q, attributed).
- **RF loopback** (`tests/end_to_end.rs`): packets → `encode_frame` →
  BPSK `modulate` → DDC → coherent decoder, with CFO and noise, both at
  48 kS/s and 2.4 MS/s wideband; asserts EGC text, priority, service and
  bulletin-board frame number round-trip exactly.
- **Unit** (`packet.rs`, `frame.rs`): descrambler table prefix, frame
  round-trip (clean / inverted / ~1 % symbol errors), Fletcher checksum,
  LCN assembly, EGC single/multi-part assembly, ITA2 alphabet, area-shape
  classification, the rectangular/circular/NAVAREA/coastal C3 geometry
  decode against the manual worked examples (incl. a southern/eastern
  hemisphere case) and end-to-end through the EGC assembly path, the
  STDC-2 helper tables against their inmarsatc oracle values
  (`channel_type_name`, `bulletin_status`, `services_short` /
  `services_full`, `tdm_slots` two-bit unpacking, `parse_stations` record
  layout, `sat_les` region+operator), `frame_to_utc_hms` against the
  off-air oracle, and every other field-decode table against its oracle.
- Oracles cross-referenced: inmarsatc (field tables, descriptor field
  maps, services/status/channel-type tables + formulas), SatDump (`.frm`
  stage goldens on the sigidwiki capture), Scytale-C (C3 geometry binary
  packing + NAVAREA coordinator table), inmarsat-sniffer (C2 service-name
  classification cross-check), IMO International SafetyNET Manual (2019)
  (EGC area addressing + worked examples), ITU-T ITA2 (Baudot alphabet).
  STD-C is oracle-validated field-exact — see
  [BENCHMARKS.md](BENCHMARKS.md) (no count-style head-to-head yet).

## Known limitations / intentional gaps

- **EGC area coordinate extraction — now DONE** (was deferred). The
  on-air binary C3 address code is decoded into signed degrees /
  nautical miles for rectangular, circular, NAVAREA/METAREA and
  coastal/NAVTEX areas, emitted as `details["area"]["geometry"]` (see the
  EGC geometry table). The binary packing is sourced from Scytale-C and
  pinned to the SafetyNET Manual worked examples. Remaining gap: no EGC
  area packet appears in the vendored off-air fixture, so the geometry
  path is verified against the manual's worked-example byte layouts (round-
  trip + Scytale-C) rather than against a real area-addressed capture.
- The remaining control descriptors (0x2A inbound-message-ack, 0x91
  distress-alert-ack, 0x9A enhanced-data-report-ack, 0xA0
  distress-test-request, 0xAC request-status, 0xAD test-result) are
  recognized and named but their inner fields are not broken out — most
  have no public real-byte sample to verify a field map against.
- The 28-entry 0x6C `tdm_slots` array is surfaced as raw two-bit codes;
  the per-slot allocation semantics are not interpreted further.
- Demod cold-start timing acquisition is weak without receive-path
  filtering (see PHY).

## Gotchas

1. Coded-pair order: 133-output first (171-first → garbage).
2. Bit packing is LSB-first per byte (chainback + byte reversal).
3. Scrambler init 0x80, not 0x40.
4. Accept checksum `00 00` inside multiframe content.
5. Packet length fields exclude the descriptor byte(s) — add 1/2/3.
6. 180° ambiguity is resolved at the UW (both polarities), not in demod.
7. Gardner must be gated on carrier lock or frames slip and corrupt.
8. EGC C3 geometry: longitude hemisphere bit is in byte `[2]` bit7 (with
   the N/S extent / radius), not on the longitude byte itself; and the
   rectangular extent is on-air nautical miles even though the manual's
   MSI-provider string states degrees.

## Key references

- inmarsatc (GPL-3) — field tables, channel/frame formulas (facts only).
- SatDump (GPL-3) — PHY constants, `.frm` stage goldens (facts only).
- Scytale-C (GPL-3) — frame structure cross-check and the C3 area
  geometry binary packing (`PacketDecoderGeoUtils.cs`:
  `ReturnRectangularArea` / `ReturnCircularArea` / `ReturnNavArea`) +
  NAVAREA/METAREA coordinator table (`ReturnNavMetAreaCoordinator`)
  (facts only).
- inmarsat-sniffer — C2 service-name classification cross-check (facts
  only).
- IMO International SafetyNET Manual (2019), Annex 4 part A §5.2–5.3 /
  part B §3.3 — EGC geographic area addressing + worked examples
  (`60N010W30025`, `56N034W035`, `14N 66W 300`).
- ITU-T ITA2 (International Telegraph Alphabet No. 2) — Baudot alphabet.
- sigidwiki Inmarsat-C TDM page — public IQ test vector (CC BY-SA).
- PROVENANCE.md — sourcing policy and per-table oracle notes.
