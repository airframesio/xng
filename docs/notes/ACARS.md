# ACARS (ARINC 618 "plain old" VHF ACARS) — implementation notes

Native VHF ACARS demod/decode (`xng-mode-acars`) plus the carrier-shared
application layer (`xng-acars`). The PHY/framing is clean-room from ARINC
618 (`xng-mode-acars/PROVENANCE.md`); the application layer is a
MIT-attributed port of **libacars** with clean-room additions from
airframes' own `acars-message-documentation` and `acars-decoder-typescript`
and f00b4r0/acarsdec `label.c` (`xng-acars/PROVENANCE.md`).

This is the live production mode: the test station feeds it to Airframes
end-to-end, CRC-verified. Not benchmarked against a frame-count oracle (no
vendored off-air capture); validated by RF loopback, field-exact off-air
ADS-C/CPDLC vectors, and the live feed.

## Pipeline

Per channel (`lib.rs::AcarsChannelDecoder`): wideband IQ →
[`xng_dsp::Ddc`] → 24 kHz channel IQ → `demod::MskDemod` (bit stream) →
`frame::Deframer` (`AcarsFrame`) → `to_message` → `xng-acars::decode`
(application layer) → `xng_types::Message`. The DDC is skipped only when
input rate is exactly 24 kHz and the offset is zero (the loopback path);
any capture rate ≥ 24 kHz works, with the DDC resampling non-integer
multiples (e.g. an Airspy's 2.5 MS/s). One wideband capture drives many
channels (the acarsdec-replacement scenario; the end-to-end test decodes
two simultaneous bursts at ±50/75 kHz from one 2.4 MS/s stream).

## PHY / demod (`demod.rs`)

ACARS is MSK at **2400 bd** carried as **AM** sidebands; tones 1200 Hz and
2400 Hz are **differential** (1200 Hz = bit change, 2400 Hz = no change;
the all-ones pre-key radiates continuous 2400 Hz). Channel rate is 24 kHz
= **10 samples/bit** (asserted at construction).

Chain: AM envelope (`|IQ|`, immune to carrier frequency offset) → DC block
(EMA highpass, fc ≈ 19 Hz, settles within the pre-key, removes the carrier
level) → complex mix down by −1800 Hz (the tone midpoint) → 1300 Hz
lowpass (121-tap, passes the MSK main lobe at ±600 Hz, rejects the
−3000/−4200 Hz mix images) → per-sample frequency discriminator (phase
advance; mean < 0 over a bit → 1200 Hz → bit change) → per-bit
integrate-and-dump with **zero-crossing timing recovery** (gain 0.15,
nudges the bit phase so tone transitions land on bit boundaries) →
differential decode (`prev_bit ^= change`). Smoothed envelope power is
exposed as a rough RSSI in dBFS. There is no equalizer — the AM-envelope
front end and 10 sps integrate-and-dump are sufficient at VHF SNRs.

The differential mapping leaves the bit stream polarity-ambiguous at
start-up (we tune in mid-burst), so the deframer hunts both polarities.

## Framing / character assembly (`frame.rs`)

Block layout after the `SYN SYN SOH` sync the deframer hunts for:
`Mode(1) Address(7) TechAck(1) Label(2) BlockId(1)` (12-octet header), then
either `STX Text ETX/ETB` or a bare `ETX` (textless uplink), then the
2-byte BCS (no parity) and `DEL`. Characters are **LSB-first with odd
parity in bit 8**; pre-key, BCS and DEL carry no parity.

- **Sync hunt**: 24-bit `SYN SYN SOH` pattern, **1 bit error tolerated**,
  matched in both polarities (a match against the inverted pattern sets an
  XOR mask that resolves the differential polarity for the whole block).
- **BCS**: CRC-16/KERMIT over the parity-bearing octets Mode→ETX/ETB
  inclusive (SOH excluded); appending the two received BCS bytes must leave
  residue 0. ARINC 618's "K7" worked example (0xCB 0x37 → 0x6B3E) is
  fenced in `xng-dsp` tests.
- **Error correction** (`correct_errors`): a single bit error breaks both a
  character's odd parity and the CRC, so bad-parity characters localize the
  search. On CRC failure, try one bit flip per bad-parity suspect (8
  candidates each, up to **3 suspects** → 8³ joint search) and accept the
  combination restoring residue 0; if the body is parity-clean the error is
  in the parity-less BCS, so the 16 single-bit BCS flips are tried too.
  `fixed_bits` and `parity_errors` are reported. Frames that still fail with
  more than 8 bad-parity characters are dropped as noise (tolerant sync
  makes false starts common). Runaway collections (>250 chars, lost suffix)
  reset to hunting.
- Decoded fields: `mode`, `tail` (dot-padding stripped; `None` for all-NUL
  squitter/all-call), `ack` (`None` on NAK), 2-char `label` (`0x7F` → `'d'`
  per the WAVECOM display convention), `block_id` (`None` for NUL uplink),
  `downlink` (block id is a digit), and for downlinks the 4-char message
  number + 6-char flight id leading the text.

`modulate.rs` is the matching ARINC 618 modulator (frame octets +
MSK-on-AM IQ) used for loopback; it shares no state with the decoder, so a
convention error on either side surfaces as a loopback failure.

`xng-acars::block` is the parallel **octet-level** ACARS block parser used
by carriers that deliver ACARS as bytes rather than an MSK bitstream
(VDL2 AOA, HFDL, Aero, Iridium SBD) — same header/suffix/CRC/parity
handling, no demod.

## Application layer (`xng-acars`)

`decode(label, text, downlink)` dispatches on the ACARS label and returns
an `AppDecode` (sublabel/MFI + flat OOOI/position/met fields + a structured
`AcarsApp`). The same crate serves every ACARS carrier.

**ARINC 622 ATS envelope** (`arinc622.rs`) — `<gs_addr>.<IMI><air_reg><hex>`;
IMI table **AT1/CR1/CC1/DR1/ADS/DIS**; CRC-16/IBM-3740 (poly 0x1021, init
0xFFFF, unreflected) over IMI+air_reg ASCII + payload bytes, residue
0x1D0F. CRC failure is recorded, not fatal (libacars behavior). Labels
`A6/AA/B6/BA` and `H1` route here.

- **ADS-C (FANS-1/A)** (`adsc.rs`) — full tag decode, direction-aware
  (downlink and uplink share tag numbers with different meanings).
  Downlink tags: Ack, Nack (14 reason texts + ext), Noncompliance,
  CancelEmergency, **Basic/Emergency/LateralDeviation/VerticalRate/
  AltitudeRange/WaypointChange Reports** (lat/lon, alt, timestamp,
  figure-of-merit accuracy 0–7, nav-redundancy/TCAS flags), FlightId,
  PredictedRoute, EarthRef (track/ground-speed/vertical-speed), AirRef
  (heading/mach/vert-speed), Meteo (wind/temperature), AirframeId (ICAO
  hex), Intermediate/Fixed Projection. Uplink: CancelAllContracts,
  CancelContract, UplinkCancelEmergency, **ContractRequest**
  (Periodic/Event/EmergencyPeriodic) with the nested request groups
  (reporting interval, modulus selectors, lateral/vertical/altitude
  thresholds, aircraft intent). `.DIS` is a single disconnect-reason byte.
  Exact libacars scaling formulas (coordinate (180−90/2¹⁹)·r/0xFFFFF,
  altitude ×4 ft, timestamp ×0.125 s, heading/wind/temperature scalings,
  speed ÷2, vert-speed ×16, distance ÷8).
- **CPDLC (FANS-1/A)** (`cpdlc/`) — unaligned-PER decode of the
  ATCDownlink/UplinkMessage header (msgId, optional msgRef, optional
  timestamp) and the message elements against the generated element tables
  (**129 downlink + 183 uplink** templates, e.g. `dM0NULL`="WILCO",
  `dM9Altitude`="REQUEST CLIMB TO [altitude]"). Element **arguments** are
  decoded for the shapes implemented — Altitude (QNH/QFE/GNSS/FL, metric),
  Speed (IAS/TAS/GS/Mach, metric), Position (fixName/navaid/airport/
  lat-lon), Time, Degrees, Direction, VerticalRate, FreeText, the
  two/three-component composites, and **RouteClearance** (dep/dest airports,
  runways, SID/STAR/approach procedures, airway-intercept, multi-leg route
  with published identifiers, lat-lon, place-bearing(-distance), airways) —
  and substituted into the template (`REQUEST CLIMB TO FL360`). Additional
  elements beyond the first decode while every preceding argument shape is
  decodable (UPER has no per-element length prefix); an undecoded shape
  leaves its bracketed template intact and stops the walk. Only AT1 carries
  ATC messages; CR1/CC1/DR1 (context-management) are reported with verified
  raw payload hex. trackDetail legs and `routeInformationAdditional` are
  reported present-but-undecoded.

**MIAM (ARINC 841)** (`miam.rs`) — rides label **MA**. CF frame map
(T single-transfer / F/K/S/A/Y/X file-transfer signalling), base85 armor
('!' zero digit, 'z' all-zero word), v1/v2 DATA CORE-PDU header walk
(version, pdu_type data/ack/aloha/aloha-reply, aircraft id, msg num, app
id, compression), raw-DEFLATE bodies inflated via miniz_oxide (windowBits
−15 equivalence). Multi-message **file transfers** reassemble
(`FileReassembler`): request registers id+size, segments carry CORE-PDU
fragments, completion at the declared size parses the combined text; abort
cancels; 10-minute timeout.

**OHMA** (`ohma.rs`) — Boeing aircraft-health JSON on H1 (when ARINC 622
does not claim the text): OHMA/RYKO marker behind optional routing
prefixes, the duplicated-first-block workaround, base64 → zlib → JSON.

**Media advisory** (`media_adv.rs`) — label **SA** datalink-availability
report (`0EV121314VS/text`): version, established/lost, changed link,
HH:MM:SS, available links (V/S/H/G/C/2/X/I), free text.

**Q-series classification** (`qseries.rs`) — every ARINC 620 `Q0`–`Q7` /
`QA`–`QX` link-test/squitter/OOOI-event label gets a kind
(LinkTest/Out/Off/On/In/Oooi/Eta/Other) and a documented description
(airframes wording where it exists; OOOI labels named from acarsdec
`label.c`).

**H1 #CFB family** (`cfb.rs`) — classifies the "Crew Flight Bag"
Boeing/Airbus maintenance telemetry into its documented sub-types
(`APM_REPORT`, `ATA`, `AL`, `ECT`, `FDE`, `FLR`, `LIGHTS`, `MIL`, `MPF`,
`PAGE`, `WRN`, and the `.01`/`.1` failure-record form), longest-prefix
matched; bodies are not parsed per-sub-type yet.

**H1 sublabel / MFI** (`sublabel.rs`) — downlink `#xxB` / uplink `- #xx`
sublabel, optional `/yy ` MFI; stripped from the text before app dispatch.

**Flat text extractors** (run on every message, surfaced at the top of the
`app` JSON to match acarsdec's flat output):

- **OOOI** (`oooi.rs`) — OUT/OFF/ON/IN gate/wheels times + departure/
  destination airports + ETA across the Q-series (Q1/Q2/QA–QT) and the
  airline-application labels acarsdec handles (10/11/12/15/17/1G/20/21/2N/
  2Z/33/39/45/80/83/8D/8E/8S). JSON names match acarsdec `output.c`
  (`depa/dsta/eta/gtout/gtin/wloff/wlin`); every slice is bounds-checked
  and airports/times validated (acarsdec's raw `memcpy`s are not), dropping
  misaligned fields rather than emitting junk.
- **Position** (`position.rs`) — lat/lon from free-text position reports on
  labels `20`/POS (scaled-decimal, `38160`→38.160°), `H1` POS and `4J`
  PS/POS (degrees+tenths-of-a-minute, `43312`→43.52°), and the legacy `4J`
  literal-dot form (`N5043.5E01121.8`).
- **Met** (`met.rs`) — winds-aloft set from the `4J` POSWX report:
  `/WND` direction+speed, `/SAT` static air temp (M/P sign convention),
  `/TAS` true airspeed, `/ALT` flight level → feet. The WMO-BUFR AMDAR
  binary schema (NOAA `dcacar`) is **intentionally out of scope** — no
  airframes documented example carries it, so there is nothing to verify
  against.

**Multi-block reassembly** (`reasm.rs`, wired in `src/runtime.rs`) — long
messages spanning ACARS blocks (intermediate ETB, final ETX) are stitched:
fragments keyed by (tail, label, msg-number), downlinks sequenced by the
4th message-number character, uplinks by block id with the A..W wrap and
the empty-ACK skip. On completion the runtime replaces the final block's
text with the assembly, flags `reassembled`, and **re-runs the application
layer** over the full text (long CPDLC/OHMA/MIAM only decode complete).
Bearer timeouts: VHF/VDL2 120 s, satcom/HF 660 s. `Reasm::assstat()`
returns acarsdec's exact `assstat` strings (`complete`/`in progress`/
`skipped`/`duplicate`/`out of sequence`).

## Validation / oracles

- **libacars** (MIT, attributed) is the structural oracle for ARINC 622,
  ADS-C, CPDLC element tables, MIAM, OHMA, media advisory, sublabel/MFI and
  reassembly semantics. The four real off-air ADS-C messages from libacars'
  `examples/adsc_get_position.c` are field-exact conformance fixtures
  (`tests/real_messages.rs`: lat/lon, altitude, track, mach, wind, temp to
  1e-6/1e-4).
- **acarsdec** (`label.c`/`output.c`) fixes the OOOI field offsets, JSON
  field names, Q-series event mapping, and `assstat` strings.
- **airframes' own references** — `acars-message-documentation` (research
  notes per label) and `acars-decoder-typescript` (coordinate utils,
  Q-series/CFB wording) back every position/met/Q/CFB description and the
  real example strings the tests assert against.
- **ARINC 618** + reveng CRC catalogue back the PHY/framing/BCS; the "K7"
  CRC example is fenced in `xng-dsp`.
- **RF loopback** (`modulate.rs` → decoder) covers the full demod/frame
  chain at the channel rate and across multiple offset channels in one
  wideband capture, including OOOI/position surfacing in the message body.
- **Live**: the production Airframes feed (`KE-KSMF-ACARS1-TEST`).

## Known limitations / intentional gaps

- No frame-count benchmark vs acarsdec — no vendored off-air capture; the
  capture-able captures are too large to fence in CI. Confidence is the
  live feed + loopback + field-exact app vectors.
- CR1/CC1/DR1 CPDLC (context-management) bodies are reported with verified
  raw hex, not ASN.1-decoded (different ASN.1 from AT1).
- CPDLC element arguments are decoded for the implemented shapes; others
  keep the bracketed template (e.g. `[frequency]`); trackDetail /
  routeInformationAdditional reported present-but-undecoded.
- `#CFB` sub-types are classified, not body-parsed.
- MIAM CRC fields are parsed but not verified (libacars default).
- The WMO-BUFR AMDAR met binary is deliberately unsupported (no real
  reference to verify against).
- No equalizer in the demod (unnecessary at VHF SNRs).

## References

- ARINC Specification 618 (Air/Ground Character-Oriented Protocol) §2–4.
- ARINC 622 (ATS data link), ARINC 620 (Q-series labels), ARINC 841 (MIAM).
- ICAO Annex 10 (ISO-5 character set); reveng CRC catalogue (CRC-16/KERMIT).
- libacars (MIT) — `arinc.c`, `adsc.c`, `media-adv.c`, `acars.c`, `miam.c`,
  `miam-core.c`, `ohma.c`, `reassembly.c`, FANS asn1c tables.
- f00b4r0/acarsdec — `label.c`, `output.c`.
- airframes `acars-message-documentation`, `acars-decoder-typescript`.
- [BENCHMARKS.md](BENCHMARKS.md) (sibling modes), [HFDL.md](HFDL.md),
  [VDL2.md](VDL2.md), [IRIDIUM.md](IRIDIUM.md) (carriers sharing this layer).
