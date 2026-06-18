# Provenance — xng-mode-ais

Clean-room implementation. Sources used (protocol facts and standards text
only; no code from any decoder was read or ported):

- ITU-R M.1371-5 (freely published): GMSK 9600 bd BT=0.4, NRZI encoding
  (a zero is encoded as a level change), 24-bit training sequence, HDLC
  framing (ISO/IEC 13239): 0x7E flags, bit stuffing after five consecutive
  ones, 16-bit FCS (CRC-16/X-25), octet transmission LSB-first with message
  fields defined MSB-first (hence the per-octet bit reversal between wire
  bytes and the message bit string).
- NMEA 0183 / IEC 61162-1: AIVDM sentence structure, 6-bit ASCII armoring
  (value +48, +56 above 39), fill bits, XOR checksum, multi-sentence
  fragmentation.
- Textbook DSP (frequency-discriminator GMSK demodulation, timing
  recovery).

The end-to-end test is anchored to a widely published example AIVDM
sentence (type 1, MMSI 477553000) reconstructed back to wire bits, so the
bit-order/armoring conventions are verified against real-world data, not
just self-consistency.

## ASM (DAC/FID binary) dispatch — DAC=200 Inland AIS (2026-06)

Type-6/8 application-specific message bodies are dispatched by DAC/FID. The
DAC=200 (Inland AIS) subtypes — FID 10 (ship static & voyage), FID 23 (EMMA
warning), FID 24 (water level) and FID 40 (signal strength) — follow the
field layouts in UNECE ECE/TRANS/SC.3/176 (Inland AIS) and gpsd's published
AIVDM reference (standards/spec text only). Field offsets, scaling
conventions (1/10 m length/beam, 1/100 m draught, and the re-use of the
1/600000-degree lat/lon scaling for the EMMA/signal-strength coordinates),
and emitted values are anchored to the **pyais** (MIT) decode oracle:
`tests/test_decode.py::test_msg_type_8_inland`, `_inland_2`,
`_dac_200_fid_23`, `_dac_200_fid_24`, `_dac_200_fid_40` (pyais 3.1.0). No
pyais code was copied; the vectors and asserted values are the reference.
Unrecognised DAC/FID fall back to the existing `data_hex` field — no
unverified subtypes are fabricated.

## ASM dispatch — DAC=200 Inland AIS message-6 + regional DACs (2026-06)

Three further DAC=200 Inland AIS application messages (carried in message 6,
addressed) are decoded. **pyais has no decoder for any of them** (it ships
only DAC=200 FID 10/23/24/40), so these are **spec-derived** from UNECE
ECE/TRANS/SC.3/176 (Inland AIS) / the CESNI Test Standard for Inland AIS,
cross-checked field-for-field between two independent references that agree —
the IALA ASM registry (iala.int/asm) / e-Navigation.nl, and gpsd's published
AIVDM reference. The governing source is cited on each arm of
`fields::asm_decode`. FIDs decoded:

- **FID 21** — ETA at lock/bridge/terminal: UN country code (12 b / 2 chars),
  UN/LOCODE (18 b / 3), fairway section number (30 b / 5), terminal code
  (30 b / 5), fairway hectometre (30 b / 5), ETA month 4 (0=N/A) / day 5
  (0=N/A) / hour 5 (24=N/A) / minute 6 (60=N/A), assisting tugs 3 (7=unknown),
  air draught 12 (0.01 m, 0=not used), spare 5.
- **FID 22** — RTA at lock/bridge/terminal (shore→ship reply): same five
  location strings + month/day/hour/minute, then lock/bridge/terminal status
  2 (0=operational, 1=limited, 2=out of order, 3=N/A), spare 2.
- **FID 55** — number of persons on board: crew 8 (255=unknown), passengers
  13 (8191=unknown), shipboard personnel 8 (255=unknown), spare 51.

Regional/national AtoN monitoring is also decoded:

- **DAC 235 (UK) / DAC 250 (Ireland) FID 10** — AtoN monitoring data
  (message 6): analogue internal 10 (0.05 V/step), analogue external #1 10,
  analogue external #2 10, RACON status 2, light status 2, health/alarm 1,
  status external 8, off-position 1, spare 4. Layout per the gpsd AIVDM
  reference; no pyais oracle.

HEADER-ONLY (per the skip-don't-fake mandate): **DAC 366/316** (US/Canada
St. Lawrence Seaway & PAWSS), **DAC 367** (US environmental/area-notice) and
**DAC 265** (Sweden STM route) have no clean-room body layout available — the
gpsd tables list the DAC/FID pairs but document no bit fields, and the IALA
ASM registry layouts were not reproduced clean-room. For these a header-only
identification (`region`, `fid`) is emitted and the raw body is preserved as
`body_hex`; the per-FID body fields are deliberately NOT guessed.

VERIFICATION (no OSS oracle): each fully-decoded FID has a unit test whose
expected values are the documented physical quantities from the cited spec.
Fixtures are built by the independent MSB-first packer (`build_t6_dac200` /
`build_t6` / `pack` / `pack_i` / `pack_str`) in document order — it shares no
code with the by-`(offset, width)` decoder, so this is not a self-loopback. A
wrong offset or width in the decoder mismatches the packer. N/A/unknown
sentinels are regression-tested (omitted, never emitted as junk).

## ASM (DAC/FID binary) dispatch — DAC=1 IMO SN.1/Circ.289 (2026-06)

DAC=1 is the IMO international application-identifier space. **pyais has no
DAC=1 decoder**, so there is no OSS decode oracle for these; every field
layout is **spec-derived** from IMO SN.1/Circ.289 ("Guidance on the use of
AIS application-specific messages", 2 June 2010) and the legacy layouts in
IMO SN/Circ.236 retained by ITU-R M.1371-5 Annex 5 / Annex 8. The governing
circular section is cited in a code comment on every FID arm of
`fields::dac1_decode`. FIDs decoded:

- **FID 31** — meteorological & hydrological data (Circ.289; supersedes
  FID 11). lon 25 / lat 24 (1/1000 min, raw/60000°) FIRST, position-accuracy
  flag, UTC day/hour/minute, average + gust wind speed (kt) and direction
  (deg), air temp (0.1 °C), humidity (%), dew point (0.1 °C), air pressure
  (hPa, offset +799), tendency, visibility (0.1 NM + ">" flag), water level
  (0.01 m, offset −10 m), trend, surface current (0.1 kt) + direction. N/A
  sentinels (127/360/−1024/511/4001/255) honoured.
- **FID 11** — legacy met/hydro (SN/Circ.236 Annex 4). Same physical fields
  but **latitude precedes longitude** and the position is 1/1000 min in a
  24/25-bit pair; water level is 0.1 m in a 9-bit field. The lat-before-lon
  order is the key divergence from FID 31 and is regression-tested.
- **FID 16** — number of persons on board (13-bit count, 0 = N/A).
- **FID 17** — VTS-generated/synthetic targets: repeating 122-bit records
  (id-type 2, target id 42, spare 4, lat 24, lon 25, COG 9, timestamp 6,
  SOG 10); id-type 0 carries a 30-bit MMSI in the high bits of the 42-bit id.
- **FID 21** — weather observation report from ship: variant flag, location
  name (6-bit ASCII), position, UTC. The WMO-coded weather block is deferred.
- **FID 22/23** — area notice (broadcast/addressed): header (message
  linkage, notice description, valid-from month/day/hour/minute, duration
  minutes) + sub-area shape count. Per-shape geometry deferred.
- **FID 24** — extended ship static & voyage: message linkage, air draught
  (0.1 m), last/next/second-next port UN/LOCODEs. Cargo table deferred.
- **FID 25** — dangerous cargo indication: linkage, amount unit, amount,
  cargo-code count. Per-item IMDG/IGC codes deferred.
- **FID 26** — environmental / sensor report: site position + UTC header +
  sensor-report count. Per-sensor type-specific blocks deferred.
- **FID 27/28** — route information (broadcast/addressed): linkage, sender
  class, route type, valid-from time, duration, waypoint count, and the
  waypoint list at the core 1/10000-min (raw/600000°) resolution.
- **FID 29/30** — text description (broadcast/addressed): linkage + 6-bit
  ASCII free text.
- **FID 32** — tidal window: header (linkage, month, day) + repeating 88-bit
  window records (lon/lat 1/1000 min, from/to UTC hour:minute, current
  direction deg, current speed 0.1 kt).

VERIFICATION (no OSS oracle): each FID has a unit test whose **expected
values are the documented physical quantities** from the cited circular
section. The test fixtures are built by an *independent* MSB-first bit
packer (`build_t8_dac1` / `pack` / `pack_i` / `pack_str` in the test module)
that takes `(value, width)` pairs in document order — it shares no code with
the decoder, which reads by `(offset, width)`. A wrong offset or width in
the decoder mismatches the packer, so this is not a self-encode/self-decode
loopback of the decode logic. FID 11's lat-before-lon ordering is the same
physical position as FID 31's lon-first test, so a decoder that copied FID
31's layout into FID 11 would fail.

DEFERRED (recorded honestly, fall through to `data_hex` for the unparsed
remainder or simply omitted): FID 21 WMO weather block; FID 22/23 sub-area
shape geometry (circle/rectangle/sector/polyline/polygon/text records); FID
24 cargo amounts table; FID 25 per-item cargo codes; FID 26 per-sensor
type-specific report blocks; FID 19 (marine-traffic-signal) and FID 18/20
(clearance/berthing) are not decoded. These need worked examples with known
ground-truth values to ground safely and were skipped rather than guessed.

## Distress device classification (2026-06)

The `distress` tag classifies SART/MOB/EPIRB-AIS transmitters by MMSI
prefix per the ITU-R M.1371 / MID allocation for device identities:
970 = AIS-SART, 972 = AIS-MOB, 974 = EPIRB-AIS (standards facts only). The
devices emit ordinary AIS messages; the prefix marks the distress class.

## Multi-fragment AIVDM reassembly + per-MMSI tracking (AIS-2, 2026-06)

`reassembly.rs` adds the inbound counterpart to the `nmea.rs` encoder: it
parses AIVDM/AIVDO/BSVDM/ARVDM sentences (the interchange form every other
AIS tool, and the AIS-Catcher HTTP feed, speaks) and joins multi-fragment
messages (`!AIVDM,2,1,...`/`!AIVDM,2,2,...`) back into one bit string before
field decode. Fragments are keyed on `(channel, total, seq)` and accepted in
any order; the fill bits of the final fragment alone are trimmed.

`AisTracker` aggregates per-MMSI static/identity fields across messages — the
type-24 **Part A** (name) + **Part B** (type/vendor/callsign/dimensions or
mothership) merge, and successive type-5 voyage records, collapse into one
`VesselRecord`. The merge rule (a newer non-null field overwrites; absent
fields are preserved) matches the pyais tracker `update_track`.

Sentence structure, 6-bit ASCII de-armoring, fill-bit accounting, and the
reassembly/merge semantics are anchored to the **pyais** (MIT) decode oracle
(pyais 3.1.0): the multi-fragment vectors are taken verbatim from
`tests/test_decode.py` (`test_msg_type_5`, `test_msg_type_8_multipart`, the
two-fragment type-21, `test_msg_type_6_very_large`, `test_decode_out_of_order`,
`test_byte_stream`/`test_multiline_message`), and the type-24 Part A/Part B
pair is gpsd's canonical example (MMSI 271041815, "PROGUY"/"TC6163"). Asserted
field values were produced by running pyais on the same sentences. No pyais
code was copied; the vectors and decode outputs are the reference.
