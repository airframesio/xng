# AIS (ITU-R M.1371-5) — implementation notes

Native AIS demod/decode for `xng-mode-ais`. GMSK 9600 bd, h=0.5, BT=0.4 in
the 25 kHz channels at 161.975 (A) / 162.025 (B) MHz. Clean-room — see
PROVENANCE.md; AIS-catcher is an off-air oracle only, pyais is the
field-decode oracle. Source: `crates/xng-mode-ais/src/`.

Result: 48 unique frames on a 5 min Sacramento capture vs AIS-catcher's 53
(91%), **zero false decodes**. The capture is inland (mostly weak distant
type-4 base-station reports), a sensitivity test by construction; the gap
is 5 payloads that anchor but never pass FCS under any tested hypothesis —
a fade tail, not a convention bug. CI-fenced by the vendored fixture.

## Pipeline

Per channel (`lib.rs::AisChannelDecoder`): wideband IQ → `xng_dsp::Ddc`
(48 kHz channel IQ; any capture rate ≥ 48 kHz, fractional rates like an
Airspy's 2.5 MS/s resampled, integer multiples skip the resampler) →
**two demods run in parallel** → `frame::HdlcDeframer` (flag hunt,
destuffing, FCS) → `frame::AisFrame` → `nmea::SentenceBuilder` (AIVDM) →
`fields::decode` → `xng_types::Message`. The NMEA UDP/TCP servers
(`--nmea-tcp`, default port 10110) live in the `xng` binary; the crate
emits the AIVDM strings.

`CHANNEL_RATE` = 48 kHz fixed (5 samples/bit at 9600 bd); the
`GmskDemod::new` constructor asserts the 5× relationship. Channel
passband 8 kHz one-sided.

Two channels are decoded by instantiating one `AisChannelDecoder` per
designator off a single wideband capture (the e2e test does both A and B
off one 2.4 MS/s capture, B carrying a deliberate 700 Hz CFO).

## PHY / demod — two paths per channel

**Streaming (`demod.rs::GmskDemod`)** — the strong-signal path (~14 dB
SNR). Per-sample frequency discriminator (`arg(x · conj(prev))`) → slow DC
tracker (`FREQ_ALPHA = 0.002`) that absorbs ship + receiver-ppm carrier
offset → per-bit integrate-and-dump with zero-crossing timing recovery
(`TIMING_GAIN = 0.15`) → NRZI decode (a *zero* is a level change, a *one*
is no change). Also tracks smoothed channel power for the dBFS RSSI.

**Coherent (`coherent.rs::CoherentDemod`)** — the weak-signal path,
+11–12 dB over the discriminator. Power gate (`GATE_FACTOR = 2.0` over a
tracked floor) finds candidate bursts; a complex template (last 16
preamble bits `0101…` + the 8-bit HDLC start flag, 24 bits) anchors them
via a differential-coherent, CFO-immune metric (four partial-sum
magnitudes); stride-2 coarse hunt with ±1 refine and low-metric span
skipping for CPU. At the anchor: coarse CFO grid (±1200 Hz, 150 Hz step) →
fractional-sample timing refine (±0.5, linear interp) → fine CFO from the
two-half template phase slope → carrier phase. Then a **16-state
GMSK-exact MLSE Viterbi**: state = (phase quadrant of completed pulses ×
two in-flight levels l_prev, l_cur), branch chooses l_next; branch
waveforms are synthesized from the true BT=0.4 Gaussian integrated phase
pulse q̃(t) (cached erf table). NRZI is decoded inside the trellis
transitions; a traceback matrix (not per-branch path clones) keeps it
O(n). Both **GMSK and MSK pulse hypotheses** run per burst; the FCS
arbitrates.

Weak-burst escalations, gated by `--demod-effort` (live vs max):
- **Hypothesis fan-out (rescue):** when the nominal single hypothesis
  yields no FCS-valid frame, re-decode over a CFO × phase-gain grid
  (±60/±120 Hz × gain 0.25/0.1/0.0, both pulse tables ≈ 30 hypotheses) and
  shifted timing windows (±1..±4 samples max, ±1..±2 live).
- **Successive interference cancellation (SIC):** an FCS-valid burst is
  reconstructed exactly (bits known, synthesis is the modulator's: CFO +
  complex-LS amplitude fit, gated to explain ≥25% of window energy),
  subtracted, and the residual re-hunted for a colliding weaker burst.
- **Max effort** lowers the anchor threshold from 0.72 to 0.55 (the deep-
  weak floor; off-air misses peak at 0.56–0.60) at ~3× hunt CPU.

The streaming and coherent paths dedup against each other (a `recent`
ring keyed on message bits, expired after ~4 slots).

## Framing (`frame.rs::HdlcDeframer`)

ISO/IEC 13239 as profiled by ITU-R M.1371: 0x7E flag hunt, bit destuffing
(a 0 after five 1s removed), 7+ ones abort, octet assembly, **CRC-16/X-25
FCS** (`xng_dsp::checksum::hdlc_frame_ok`). A closing flag may also open
the next frame (back-to-back bursts share a flag). Length bounds 56–1280
bits. Wire octets are LSB-first (arrival order); the emitted
`message_bits` are per-octet reversed to the MSB-first field order that
AIS fields and NMEA armoring consume. `msg_type` = bits 0..6, `mmsi` =
bits 8..38.

## NMEA output (`nmea.rs::SentenceBuilder`)

AIVDM per IEC 61162-1: 6-bit ASCII armoring (value +48, +56 above 39),
fill bits, XOR checksum, multi-sentence fragmentation at 60 armored chars
(82-char sentence limit) with a rotating 0–9 message sequence ID for
multi-fragment messages. Channel A/B designator from the frequency. The
e2e test reproduces a published gpsd example sentence
(`!AIVDM,1,1,,B,177KQJ5000G?tO`K>RA1wUbN0TKH,0*5C`) byte-for-byte from a
synthesized burst, anchoring bit order / armoring / checksum to real data.

## Message / field decode (`fields.rs::decode`)

Field-decoded message types (positions in degrees, speeds in knots; "not
available" sentinels honored — 181°/91° position, SOG 1023, COG 3600,
heading 511, ROT −128, etc.):

| Type | Content |
|---|---|
| 1–3 | Class A position: nav status, ROT (ROTais → deg/min), SOG, position accuracy, lat/lon, COG, heading, UTC second, maneuver, RAIM |
| 4, 11 | Base-station report / UTC-date response: UTC datetime, position, EPFD, RAIM |
| 5 | Static & voyage: AIS version, IMO, callsign, name, ship type, dimensions, EPFD, draught, destination, ETA, DTE |
| 6 | Addressed binary: seqno, dest MMSI, retransmit, **DAC/FID → ASM** (else data_hex) |
| 7, 13 | Binary / safety ACK: dest MMSI |
| 8 | Broadcast binary: **DAC/FID → ASM** (else data_hex) |
| 9 | SAR aircraft: altitude, SOG, position |
| 12 | Addressed safety text: dest MMSI, retransmit, 6-bit text |
| 14 | Broadcast safety text: 6-bit text |
| 17 | DGNSS broadcast: lat/lon (1/10-min /10 to match pyais), data_hex |
| 18 | Class B position: SOG, accuracy, lat/lon, COG, heading, UTC sec, RAIM |
| 19 | Extended Class B: 18's kinematics + name, type, dimensions, EPFD, DTE |
| 20 | Data-link management: up to four slot-reservation blocks (offset/slots/timeout/increment) |
| 21 | Aids-to-navigation: AtoN type, name, position, dimensions, EPFD, off-position, RAIM, virtual flag, name extension |
| 22 | Channel management: channels A/B, txrx, high power, addressed flag, region corners, bands, zone size |
| 23 | Group assignment: region corners, station type, ship type, txrx, interval, quiet time |
| 24 | Static data report — part A (name); part B (type, vendor ID, model, serial, callsign, dimensions, or **mothership MMSI** for 98x auxiliary craft) |
| 27 | Long-range position: lat/lon (1/600), SOG |

Supporting tables: 16-entry nav-status, 9-entry EPFD (code 15 = "internal
GNSS"), AtoN type as a raw code, ship type as a raw code.

**Application-specific messages (ASM), `fields::asm_decode`** — type-6/8
binary payloads dispatched by DAC/FID. Implemented: **DAC=200 (Inland
AIS, UNECE ECE/TRANS/SC.3/176)**:
- FID 10 — ship static & voyage (VIN, length 1/10 m, beam, ship type,
  hazard, draught 1/100 m, loaded),
- FID 23 — EMMA warning (start/end date-time, region corners 1/600000°,
  type, min/max, intensity, wind),
- FID 24 — water-level report (country, 4 × gauge id + level),
- FID 40 — signal-strength / bridge status (lat/lon, form, facing,
  direction, raw status).

Unrecognised DAC/FID (e.g. DAC=1 IMO Circ.289, DAC=669) falls back to a
hex dump of the payload (`data_hex`) — no unverified subtypes are
fabricated.

**Distress classification (`fields::distress_class`)** — `lib.rs`
tags a `distress` field by MMSI prefix per ITU-R M.1371 / MID device
allocations: 970 = AIS-SART, 972 = AIS-MOB, 974 = EPIRB-AIS. These devices
emit ordinary AIS messages; the prefix is the marker.

## Validation / oracles

- **Field decode:** every type-and-subtype arm asserts against **pyais**
  (MIT, 2.x/3.1) decode vectors as ground truth — published AIVDM
  sentences with pyais-asserted values, hand-checked for the AIS-3 fills
  pyais doesn't expose. Covers types 1, 4, 5, 6, 8 (incl. DAC=200 FID
  10/23/24/40), 12, 14, 17, 18, 19, 20, 21, 22, 23, 24A/B, plus the ROT
  helper and distress classifier. No pyais code copied; vectors and
  asserted values are the reference.
- **Framing / NMEA:** RF loopback (`tests/end_to_end.rs`) anchored to a
  published gpsd example sentence — reproduced exactly, so bit order,
  armoring, and checksum are checked against real-world data, not self-
  consistency. Stuffing roundtrip, bad-FCS rejection, back-to-back shared
  flag, GMSK-shaped burst, and dual-channel-with-CFO from one wideband
  capture are unit-tested.
- **Off-air:** AIS-catcher on a shared capture (`bench/`), CI-fenced by
  the vendored fixture floor. See
  [BENCHMARKS.md](BENCHMARKS.md).

## Known limitations / intentional gaps

- **Types 10, 15, 16, 25, 26 are not field-decoded** (coordination /
  interrogation / single-and-multi-slot binary) — `decode` returns `None`
  for them; the NMEA and frame layers still emit.
- **ASM coverage is DAC=200 only.** Other DAC/FID combinations emit
  `data_hex`, not parsed application fields.
- **No multi-fragment reassembly on receive.** Each FCS-valid HDLC frame
  decodes independently; type-5/24 messages spanning two NMEA sentences on
  *transmit* are fragmented by the encoder, but the decoder works on whole
  air frames (one AIS slot/burst), which is the natural unit.
- **No soft-decision FCS repair.** A standing falsification: a max-log
  Chase-style search flipping the K least-reliable bits recovered none of
  the 5 genuine misses and *forged* a valid-FCS frame from a foreign MMSI
  (it even subverted the two-sighting MMSI guard by emitting two variants
  that "confirm" each other). FCS-16 is too weak to gate a search that
  large; at the noise floor, sensitivity is a capture problem.

## Rescue acceptance (false-decode discipline)

The wide hypothesis fan-out + SIC makes a random FCS-16 pass a *real*
rate. Nominal single-hypothesis decodes pass through (negligible odds).
Rescue/SIC decodes additionally require a sane message type (1–27) **and**
a confirmed source MMSI — one already seen, or a second held frame from
the same MMSI (random passes never repeat a source). This is the Mode S
two-sighting ICAO-confirmation policy transplanted; the pending table is
capped at 64 with age eviction. Result on the off-air capture: zero false
decodes.

## References

- ITU-R M.1371-5 (GMSK 9600 bd BT=0.4, NRZI, 24-bit training, HDLC
  framing, message field layouts).
- ISO/IEC 13239 (HDLC), NMEA 0183 / IEC 61162-1 (AIVDM armoring).
- UNECE ECE/TRANS/SC.3/176 + gpsd AIVDM reference (Inland AIS DAC=200).
- pyais (field oracle), AIS-catcher (off-air oracle). See PROVENANCE.md.
