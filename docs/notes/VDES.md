# VDES ASM (VHF Data Exchange System — Application-Specific Messages, ITU-R M.2092-1) — implementation notes

Native VDES **ASM** message decode core for `xng-mode-vdes`. VDES (ITU-R
**M.2092-1**, "Technical characteristics for a VHF data exchange system in
the maritime mobile band between 156 MHz and 162.05 MHz") augments AIS with
two new sub-systems: **ASM** (Application-Specific Messages) on dedicated
channels and **VDE** (the high-rate VHF Data Exchange links). This crate
decodes **ASM only**. The ASM channels — **ASM 1 = 161.950 MHz** and
**ASM 2 = 162.000 MHz** (the former AIS channels 2027 / 2028) — carry
**GMSK at 9600 bit/s**, h = 0.5 (±2400 Hz deviation), Gaussian filter
**BT = 0.5** — the same link family as AIS (ITU-R M.1371), but reserved for
the application-specific (DAC/FID binary) traffic moved off the AIS position
channels. Clean-room: no decoder was copied or ported — only standards/spec
text. The crate splits into a spec-anchored DECODE/transport core (HDLC
deframing, CRC-16/X-25, AIS Message 6/8 transport, DAC/FID payloads) and a
GMSK IQ front end. The DECODE core is verified against **independent
spec-cited bit vectors**; the DEMOD is validated **ONLY** by a synthetic
modulate→AWGN→demod path — **no real off-air VDES IQ exists.**

Status: **WIRED, SYNTHETIC-ONLY validation.** Full runtime mode:
`Mode::Vdes`, `MessageBody::Vdes`, `--mode vdes`, CLI/scan paths, and a
`VdesChannelDecoder` that owns an `xng_dsp::Ddc`. The transport/payload
layer is anchored to spec-cited ground-truth bit vectors; the IQ→bits front
end is exercised only by a synthetic modulate→complex-AWGN→demod path.
There is **no real off-air capture** in this crate — VDES has sparse public
deployment and no published off-air ASM IQ.

## Pipeline

```
wideband capture IQ
  → Ddc                      mix by freq_offset_hz, decimate to CHANNEL_RATE (48 kS/s)
  → demod::GmskDemod         freq discriminator + DC tracker + 9600 Bd timing → NRZI decode → 1 bit/symbol
recovered NRZI-decoded bit stream
  → frame::HdlcDeframer      0x7E flag hunt, bit destuffing, octet assembly, CRC-16/X-25 FCS, per-octet bit reversal
  → frame::VdesFrame         (msg_type, mmsi, wire_bytes, message_bits MSB-first)
  → asm::decode              AIS Msg 6/8 transport header (source/dest MMSI + DAC/FID) + DAC=1 payloads
  → asm::Asm                 → to_message → xng_types::Message bus form
```

Two entry points:

- `VdesChannelDecoder::new(input_rate, freq_offset_hz)` — channelized IQ
  entry (mirrors the AIS/NAVTEX/POCSAG `*ChannelDecoder` contract).
  `input_rate` is any capture rate ≥ the 48 kHz channel rate (a non-integer
  multiple is resampled by the DDC); `freq_offset_hz` is the ASM channel
  center relative to the capture center. `process(iq)` feeds the DDC +
  demod, pushes each NRZI-decoded bit into the HDLC deframer, and returns a
  `Vec<frame::VdesFrame>` for every CRC-valid frame that completed in the
  chunk. When `input_rate == CHANNEL_RATE` and offset is 0 the DDC is
  skipped (IQ is already channelized). Returns `Err(String)` only if the
  DDC cannot be constructed.
- `asm::decode(message_bits)` — the verified bit→ASM transport core: reads
  the AIS Message 6/8 header (message ID, source MMSI, DAC/FID, dest MMSI)
  and the DAC/FID application payload. Returns `None` for any message ID
  other than 6 / 8.

`to_message(f, frequency_hz, level_dbfs, source)` normalizes a `VdesFrame`
into the bus `Message`: `mode = Mode::Vdes`, body
`MessageBody::Vdes { kind, details }` where `kind` is the ASM transport class
(`"asm-addressed"` / `"asm-broadcast"`) and `details` is a JSON object with
`msg_id`, `source_mmsi`, optional `dest_mmsi`, `dac`, `fid`, and (when
recognised) a nested `app` object. `decode.crc_ok = true` (every emitted
frame passed the HDLC FCS by construction), `fec_corrected = None`, RSSI from
the channel level, and the raw wire octets (FCS included) travel as `raw`.
`to_message` returns `None` when the frame's message ID is not 6/8 — the
runtime counts those as seen-but-not-a-message.

The public IQ constants:

- `CHANNEL_RATE = 48_000.0` S/s — 5 samples/bit at 9600 Bd
  (`48000 = 5·9600`), so the bit clock is an integer sample count.
- `CHANNEL_PASSBAND_HZ = 8_000.0` (one-sided) — passes the ±2400 Hz GMSK
  swing (BT=0.5 at 9600 Bd) inside a 25 kHz channel plus realistic tuning
  offset, well below the 24 kHz Nyquist of the channel rate.
- `ASM1_HZ = 161_950_000`, `ASM2_HZ = 162_000_000` — the ITU-R M.2092-1
  Annex 1 ASM channel center frequencies.

## IQ front end (`demod.rs`)

The 9600 Bd GMSK demodulator, a textbook frequency discriminator (clean-room
DSP) reusing the same approach as `xng-mode-ais`, but for the slightly wider
ASM pulse (M.2092-1 BT = 0.5 vs AIS's BT = 0.4):

- per-sample frequency discriminator `arg(x · conj(x_prev))`;
- a **slow DC tracker** (`FREQ_ALPHA = 0.002`) that absorbs residual carrier
  frequency offset (tuning error, ship + receiver ppm) so only the FSK swing
  remains;
- per-bit **integrate-and-dump** at 9600 Bd (`SAMPLES_PER_BIT = 5`) with a
  zero-crossing timing nudge (`TIMING_GAIN = 0.15`): a sign change in the
  discriminator marks a bit boundary and pulls the timing phase toward it;
- hard slice of the per-bit accumulator into a frequency level (±1), then
  **NRZI decode** — a transmitted **0 is a level change**, a **1 is no
  change** (per M.2092 / M.1371). One bit per symbol is appended.

`GmskDemod::new` asserts `CHANNEL_RATE == BAUD · SAMPLES_PER_BIT`.
`BAUD = 9_600.0` (ITU-R M.2092-1 Annex 1, ASM). `level_dbfs()` reports
smoothed channel power. NRZI removes the absolute-polarity ambiguity of raw
NRZ at the demod stage, so (unlike POCSAG) the deframer does not need to try
both polarities.

## HDLC deframing + FCS (`frame.rs`)

The link layer is HDLC per ISO/IEC 13239 — the same profile AIS uses (ITU-R
M.1371):

- **Flag hunt**: a rolling raw-bit window matches the `0x7E` flag
  (`0,1,1,1,1,1,1,0` in arrival order) to start collecting.
- **Bit destuffing**: a 0 following five consecutive 1s is removed; seven or
  more consecutive 1s abort the frame; six consecutive 1s (`0111111`)
  signals the closing flag, and the trailing flag bits are stripped from the
  collected buffer. The closing flag is kept available to also open the
  next frame.
- **Octet assembly**: destuffed bits are packed into wire octets with bit *i*
  = the *i*-th arrived bit (arrival-LSB-first). `hdlc_frame_ok` (from
  `xng_dsp::checksum`) checks the trailing 16-bit **CRC-16/X-25 FCS**;
  frames that fail are dropped.
- **Field-order conversion**: the payload octets (FCS dropped) are reversed
  per octet (LSB-first wire order → **MSB-first field order**) to produce
  `message_bits` — the form the ASM transport decode consumes.

Length bounds: `MIN_BITS = 56` (a Message-8 broadcast with empty payload:
56 header bits) and `MAX_BITS = 1280` (well above the longest single-slot
ASM). `close()` rejects frames that are not octet-aligned, fail the FCS, or
carry fewer than 38 message bits. The `VdesFrame` exposes `msg_type`
(bits 0..6), `mmsi` (source, bits 8..38), `wire_bytes`, and `message_bits`.

## ASM transport + payload decode (`asm.rs`)

ITU-R M.2092-1 carries ASMs using the **AIS binary-message transport**
verbatim: the addressed-binary (**AIS Message 6**) and broadcast-binary
(**AIS Message 8**) structures of ITU-R M.1371-5, and the **same DAC/FID
application-identifier catalogue** (a 10-bit Designated Area Code + a 6-bit
Function Identifier). `decode(bits)` reads the transport header by absolute
`(offset, width)` over the MSB-first message bit string:

**Message 8 (broadcast ASM)** — `kind = "asm-broadcast"`:

| Field | Bits | Width |
|---|---|---|
| message ID (= 8) | 0..6 | 6 |
| repeat indicator | 6..8 | 2 |
| source MMSI | 8..38 | 30 |
| spare | 38..40 | 2 |
| DAC | 40..50 | 10 |
| FID | 50..56 | 6 |
| application data | 56.. | — |

**Message 6 (addressed ASM)** — `kind = "asm-addressed"`:

| Field | Bits | Width |
|---|---|---|
| message ID (= 6) | 0..6 | 6 |
| repeat indicator | 6..8 | 2 |
| source MMSI | 8..38 | 30 |
| sequence number | 38..40 | 2 |
| destination MMSI | 40..70 | 30 |
| retransmit flag | 70 | 1 |
| spare | 71 | 1 |
| DAC | 72..82 | 10 |
| FID | 82..88 | 6 |
| application data | 88.. | — |

`decode` returns `None` for any message ID other than 6 / 8. The decoded
`Asm` carries `msg_id`, `source_mmsi`, `dest_mmsi` (`Some` only for
Message 6), `dac`, `fid`, and an `app` JSON value.

### Application payloads decoded (DAC=1, IMO international)

The DAC/FID catalogue is the one shared with AIS Message 6/8, catalogued by
**IMO SN.1/Circ.289** ("Guidance on the use of AIS application-specific
messages"). Two well-documented DAC=1 payloads are decoded; each arm of
`app_decode` cites its governing clause:

- **DAC=1 FID=16 — Number of persons on board** (IMO SN.1/Circ.289 Annex;
  ITU-R M.1371-5 Annex 5 §3.10): a 13-bit unsigned count, `0 = not
  available` (omitted when zero), emitted as `persons_on_board`.
- **DAC=1 FID=31 — Meteorological and hydrological data** (IMO SN.1/Circ.289
  Annex; ITU-R M.1371-5 Annex 8): a 360-bit application block. The decoder
  reads the leading grounded scalar fields from the application-data offset:
  **longitude FIRST** (25-bit signed) then **latitude** (24-bit signed),
  both in units of 1/1000 minute (raw / 60000 → degrees) with **181° / 91°
  not-available sentinels**; a position-accuracy flag; UTC day (5) / hour (5)
  / minute (6); average wind speed and gust (7-bit kt each, 127 = N/A); wind
  direction (9-bit deg, 360 = N/A); air temperature (11-bit signed 0.1 °C,
  raw -1024 = N/A); relative humidity (7-bit %, 101 = N/A). All N/A
  sentinels are **honoured — omitted, never emitted as junk values**. The
  WMO-coded weather tail (and the wind-gust-direction field between wind
  direction and air temperature) is **deferred**: the decoder skips it and
  reads air temp / humidity at their absolute offsets.

`details()` flattens the header (`msg_id`, `source_mmsi`, `dest_mmsi` if
present, `dac`, `fid`) plus a nested `app` object (omitted when empty) into a
single JSON object for the bus body.

### Raw-payload fallback (unknown DAC/FID)

Any DAC/FID **not** in the recognised set falls through to a **hex dump** of
the application payload: `app = {"data_hex": "<hex>"}` (the application data
from its start offset, MSB-first per octet). The transport header (source
MMSI + DAC/FID, and dest MMSI for Message 6) is **always** decoded, so an
unrecognised application is fully attributed and its binary body preserved
verbatim — **nothing is fabricated**. (The sibling `xng-mode-ais` crate
decodes a much larger DAC/FID set for the AIS position channels and is where
to extend if needed.)

## Validation / oracles

The DECODE/transport layer verifies against **spec-cited** ground truth —
hand-built bit vectors assembled from the ITU-R / IMO field layout, not an
encode→decode self-loopback. The DEMOD front end is validated **only** by a
synthetic modulate→complex-AWGN→demod path. There is **no real off-air
VDES IQ** anywhere in this crate (none is published).

The transport tests (`tests/asm_decode.rs`) use an **independent** MSB-first
bit packer (`pack` / `pack_i`) that lays down `(value, width)` pairs in
**document order** per the cited clause; the decoder reads by absolute
`(offset, width)`. The two share no code, so a wrong offset or width
mismatches the hand-laid vector — this is not a self-encode/self-decode
loopback.

| Layer | Fact | Spec cite | How verified |
|---|---|---|---|
| Message 8 transport | broadcast header: source MMSI, DAC 40/FID 50, data 56 | ITU-R M.2092-1 Annex 1 + ITU-R M.1371-5 Message 8 | `broadcast_msg8_header_dac_fid_source` (source MMSI, DAC=1/FID=16, `kind="asm-broadcast"`, no dest) |
| Message 6 transport | addressed header: source + dest MMSI, DAC 72/FID 82, data 88 | ITU-R M.2092-1 Annex 1 + ITU-R M.1371-5 Message 6 | `addressed_msg6_header_with_dest_mmsi` (source/dest MMSI, DAC/FID, `kind="asm-addressed"`) |
| DAC=1 FID=16 | 13-bit persons-on-board, 0 = N/A | IMO SN.1/Circ.289 Annex; M.1371-5 Annex 5 §3.10 | `broadcast_msg8_header_dac_fid_source` (count 167 round-trips) |
| DAC=1 FID=31 | met/hydro physical fields, lon-first, sentinels | IMO SN.1/Circ.289 Annex; M.1371-5 Annex 8 | `dac1_fid31_met_hydro_fields` (4.0°E/52.0°N, wind, temp 15.3 °C, humidity 80%) |
| N/A sentinels | day 0, hour 24, minute 60, wind 127, dir 360, temp -1024, humidity 101, lon 181°, lat 91° | IMO SN.1/Circ.289 | `na_sentinels_are_omitted_not_emitted_as_junk` (all omitted; position-accuracy flag stays) |
| Unknown DAC/FID | header decoded, body preserved as `data_hex` | (fallback policy) | `unknown_dac_fid_preserves_raw_payload` (DAC 999/FID 5 → `data_hex = "abcd"`) |
| HDLC + FCS | flag hunt, bit destuffing, CRC-16/X-25, octet/field bit order | ITU-R M.2092-1 Annex 1 / ISO/IEC 13239 / M.1371 | `roundtrip_with_stuffing` (heavy-stuffing type-8 round trip), `rejects_bad_fcs` (corrupt FCS drops the frame) |

**SYNTHETIC DEMOD** validation (explicitly reported as synthetic — NO real
RF), in `tests/end_to_end.rs`, using `modulate.rs`:

- `modulate_msk_awgn_demod_decodes_asm` — modulate a DAC=1 FID=16 broadcast
  ASM as MSK-shaped (rectangular-pulse) 9600 Bd IQ, add complex AWGN, run
  the full `VdesChannelDecoder` (DDC → discriminator → timing → NRZI → HDLC
  → FCS → ASM), assert one frame with the exact source MMSI / DAC / FID /
  persons-on-board.
- `modulate_gmsk_awgn_demod_decodes_asm` — the **realistic** M.2092-1 ASM
  waveform (Gaussian frequency pulse, BT=0.5, h=0.5) through the same chain
  in light AWGN.
- `wideband_capture_with_carrier_offset` — a 2.4 MS/s wideband capture with
  the ASM channel at +50 kHz plus a deliberate **600 Hz carrier offset**;
  exercises the DDC channelization and the demod's DC tracker absorbing the
  CFO.
- `synthetic_ber_at_moderate_snr` — **40 independent bursts** (varying MMSI,
  payload, and noise seed) at a fixed SNR; requires the overwhelming
  majority (`≥ trials − 2`) to deframe **and** decode correctly, exercising
  the timing/offset loops across bit patterns rather than a single vector.
  A synthetic AWGN figure, not a real-RF claim.
- `corrupted_frame_is_rejected` — a flipped wire bit drops the frame (FCS)
  rather than emitting a bogus ASM.

The modulator (`modulate.rs`) is a self-generated reference, NOT an external
oracle: its waveform parameters (9600 Bd, ±2400 Hz deviation, BT=0.5
Gaussian pulse, 32-bit ASM training/ramp-up before the opening flag,
MSB-first fields) are the published M.2092-1 spec, but it only proves the
demod inverts this modulation. The transport/payload core stays
spec-anchored by its own independent-packer bit-vector tests.

## Known limitations / deferred

VDES has **sparse public deployment** and the full spec detail (especially
VDE and the satellite component) is not freely available. The following are
**skipped, not guessed** (recorded in `PROVENANCE.md` "DEFERRED"):

- **No real off-air validation.** The entire DEMOD chain is validated only
  by the synthetic modulate→complex-AWGN→demod path; no recorded VDES ASM
  capture exists in this crate, and none is published off air. All SNR/BER
  figures are synthetic AWGN, not real RF.
- **ASM only — no VDE links.** VDE-TER (terrestrial) and VDE-SAT
  (satellite) high-rate data exchange use different modulation (π/4-QPSK /
  8-PSK / 16-APSK), FEC, and framing per M.2092-1 — out of scope; no public
  worked examples to ground a clean-room decoder. The satellite VDES
  component is likewise not implemented.
- **No coded long-ASM format.** M.2092-1 specifies an optional 3/4-rate
  convolutional code + interleaver for the long ASM format; the implemented
  PHY is the uncoded GMSK + HDLC link (the AIS-style ASM transport). The
  coded variant is deferred — no public reference vector to ground it.
- **Limited DAC/FID catalogue.** Only DAC=1 (IMO international) FID=16 and
  FID=31 have decoded application bodies. All other DAC/FID values (regional
  DACs, the remaining IMO FIDs, inland-AIS DAC=200, the wider IALA ASM
  registry) fall through to `data_hex` — the header is decoded and the body
  preserved, but per-FID fields are not fabricated.
- **FID=31 weather tail deferred.** Only the leading grounded scalar fields
  of the met/hydro block are decoded; the WMO-coded weather tail (and the
  wind-gust-direction field) are skipped.
- **Operator-known channel offset.** `VdesChannelDecoder::new` requires the
  caller to supply `freq_offset_hz`; there is no automatic ASM channel /
  carrier acquisition. The demod's slow DC tracker absorbs only residual
  tuning error, not a coarse offset.
- **`crc_ok` is always `true` for emitted frames.** Only frames that pass
  the HDLC FCS reach a `VdesFrame`, so every bus message has `crc_ok = true`
  by construction; there is no soft/partial-FCS reporting.
- **No position output for FID=16.** Only DAC=1 FID=31 carries a
  lat/lon; a persons-on-board (FID=16) or an unknown-DAC ASM has no map
  location. VDES messages surface as text/record only — not on the
  dashboard "beacons" layer.

## Gotchas

1. NRZI is decoded **in the demod** (`0 = level change`, `1 = no change`),
   so the bit stream the HDLC deframer sees is already polarity-resolved —
   unlike POCSAG, the deframer does **not** try both polarities.
2. Bit order changes layer to layer: wire octets are assembled
   **arrival-LSB-first**, then reversed per octet to give the **MSB-first**
   `message_bits` that ASM fields use. The FCS is checked on the
   LSB-first wire octets; do not mix the two.
3. `CHANNEL_RATE = 48000` is exactly `5 · 9600`; changing it breaks the
   integer samples/bit invariant (asserted by
   `channel_rate_is_integer_bit_multiple` and `GmskDemod::new`).
4. The closing HDLC flag is retained to also open the next frame; do not
   assume one frame per flag or that the buffer is cleared between adjacent
   frames.
5. FID=31 packs **longitude before latitude** (M.1371-5 Annex 8 order),
   opposite the lat-then-lon convention of many AIS position messages.
6. `asm::decode` returns `None` for any message ID other than 6/8, and
   `to_message` propagates that as `None` — such frames are counted as seen
   but never become a bus message.
7. The ASM burst leads with a **32-bit** training/ramp-up (M.2092-1),
   longer than the AIS 24-bit training; the modulator emits this, but the
   demod/deframer rely only on the flag hunt, not the training length.

## Key references

- **ITU-R Recommendation M.2092-1** ("Technical characteristics for a VHF
  data exchange system in the maritime mobile band between 156 MHz and
  162.05 MHz"), Annex 1 — the authoritative VDES spec: ASM channel
  frequencies (ASM 1 / ASM 2), GMSK 9600 Bd / h=0.5 / BT=0.5 PHY, the HDLC
  link layer, and the reuse of the AIS Message 6/8 binary transport + DAC/FID
  catalogue for ASM. VDE and satellite components defined here are out of
  scope.
- **ITU-R Recommendation M.1371-5** — AIS Message 6 (addressed binary) /
  Message 8 (broadcast binary) transport header bit layout, reused verbatim
  by M.2092-1 for ASM; Annex 5 §3.10 (persons on board) and Annex 8
  (met/hydro data).
- **IMO SN.1/Circ.289** ("Guidance on the use of AIS application-specific
  messages") Annex — the DAC=1 (IMO international) application catalogue:
  FID=16 persons on board, FID=31 meteorological & hydrological data, with
  field layouts and N/A sentinels.
- **ISO/IEC 13239** — HDLC framing (flags, bit stuffing, CRC-16/X-25 FCS),
  the link profile shared with AIS.
- `docs/notes/POCSAG.md` / `docs/notes/NAVTEX.md` — sibling
  GMSK/FSK modes whose `*ChannelDecoder` / DDC front-end structure this crate
  mirrors (and which, like VDES, are spec-anchored decode + synthetic-only or
  off-air demod validation).
- `crates/xng-mode-vdes/PROVENANCE.md` — sourcing policy, the decoded
  transport/payload list, and the per-item "DEFERRED" record.
