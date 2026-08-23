# ACARS (ARINC 618 "plain old" VHF ACARS) — implementation notes

Native VHF ACARS demod/decode (`xng-mode-acars`) plus the carrier-shared
application layer (`xng-acars`). The PHY/framing is clean-room from ARINC
618 (`xng-mode-acars/PROVENANCE.md`); the application layer is a
MIT-attributed port of **libacars** with clean-room additions from
airframes' own `acars-message-documentation` and `acars-decoder-typescript`
and f00b4r0/acarsdec `label.c`/`syndrom.h` (`xng-acars/PROVENANCE.md`).

This is the live production mode: the test station feeds it to Airframes
end-to-end, CRC-verified. Benchmarked against acarsdec on a real off-air
capture (the Opflasher 3.0 MS/s capture, slice vendored as
`bench/data/acars_100k.cs16`): **xng 16 CRC-OK vs acarsdec 3.7 17 clean** on the
same capture (comparable), CI floor 13 — see [BENCHMARKS.md](BENCHMARKS.md). Also
validated by RF loopback, field-exact off-air ADS-C/CPDLC vectors, and the live feed.

## Pipeline

Per channel (`lib.rs::AcarsChannelDecoder`): wideband IQ →
[`xng_dsp::Ddc`] → 24 kHz channel IQ → `demod::MskDemod` (bit stream) →
`frame::Deframer` (`AcarsFrame`, with syndrome-table FEC) → `to_message`
→ `xng-acars::decode` (application layer) → `xng_types::Message`. The DDC
is skipped only when input rate is exactly 24 kHz and the offset is zero
(the loopback path); any capture rate ≥ 24 kHz works, with the DDC
resampling non-integer multiples (e.g. an Airspy's 2.5 MS/s). One wideband
capture drives many channels (the acarsdec-replacement scenario; the
end-to-end test decodes two simultaneous bursts at ±50/75 kHz from one
2.4 MS/s stream).

### Shared multi-channel front end (CPU)

A per-channel `Ddc` runs a full-rate anti-alias decimation for **every**
channel, so N channels do N full-rate convolutions over the same wideband
stream — the dominant cost (~17× the bit demod, linear in channel count).
When a session has ≥2 ACARS channels, `runtime.rs::collapse_shared_acars`
replaces the N independent `AcarsChannelDecoder`s with one
`AcarsMultiChannelDecoder` that does the wideband-rate work **once** for all
channels, then runs the usual per-channel `MskDemod`/`Deframer`. Output is
identical (a `cargo test` asserts both front ends decode the same frames);
it is purely a CPU optimization.

Two interchangeable front ends (same `(input_rate, output_rate, offsets,
passband)` contract), selected in `AcarsMultiChannelDecoder::new`:

- **`xng_dsp::ChannelizedDdc`** (default) — a polyphase channelizer: one
  shared FFT pass produces every channel at once, so cost is **independent
  of channel count and of how far apart the channels sit**. VHF airband
  channels are all on a 25 kHz raster, so the bin grid `fs/M` is chosen to
  land every requested channel on a bin center (no scalloping); a small
  residual NCO + a gentle resampler land the exact 24 kHz channel rate.
- **`xng_dsp::SharedDdc`** (fallback, `new_shared`) — one shared full-rate
  decimation feeds cheap per-channel finishes. Its win shrinks as channels
  spread across the band (the coarse stage can only decimate as far as the
  widest channel allows), so the channelizer is preferred; `SharedDdc` is
  the fallback when the channelizer cannot be built for a rate/offset set.

Both live in `xng-dsp` and are general-purpose: they are intended to be
adopted by the other narrowband multi-channel modes (VDL2, AIS, Aero,
STD-C, which all use the same per-channel offset DDC) after the ACARS path
is validated on live RF. See `bench/cpu.sh` for the per-channel-count
×-realtime numbers.

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

**Per-burst noise floor + SNR (ACARS-4.1)**: the demod keeps a second,
slower envelope-power EMA (`NOISE_ALPHA` 0.002) that tracks only the
inter-burst silence — a sample more than `NOISE_GATE` (8×) above the
running floor is treated as signal and frozen out, so a long transmission
can't drag the floor up to the carrier level (the pure-noise tail above
the gate is ~e⁻⁸, negligible). A high seed from tuning in mid-burst
self-corrects once silence falls back below the gate. `noise_dbfs()`
surfaces it; `to_message` fills `SignalQuality.noise_db` from it and
`snr_db` as `rssi - noise` (level_dbfs − noise_dbfs). A unit test
(`noise_floor_tracks_known_awgn_power`) checks the estimate converges to
the analytic complex-AWGN envelope power 2σ² (an independent ground truth,
**not** a demod loopback) within 1 dB, and that doubling σ raises the
measured floor by 10·log₁₀4 ≈ 6.02 dB.

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
- Decoded fields: `mode`, `tail` (dot-padding stripped; `None` for all-NUL
  squitter/all-call), `ack` (`None` on NAK), 2-char `label` (`0x7F` → `'d'`
  per the WAVECOM display convention), `block_id` (`None` for NUL uplink),
  `downlink` (block id is a digit `'0'..='9'`), and for downlinks the
  4-char message number + 6-char flight id leading the text.

### Error correction (`fec.rs` fast path + `frame.rs::correct_errors`)

A single bit error breaks both a character's odd parity *and* the CRC, so
the corrector runs in two tiers:

1. **O(1) syndrome-table fast path** (`fec.rs`, ACARS-4.2). Because
   CRC-16/KERMIT is linear over GF(2), the residue of a received block
   equals `crc(error_pattern)` alone — independent of the message. We
   tabulate, for a lone 1-bit at byte-distance `d` from the buffer end and
   bit `b`, the residue that error produces (running the *same*
   `xng_dsp::checksum::acars_crc` over one-hot buffers), and invert that
   map at runtime: `correct_single_bit` computes the residue, looks up the
   offending bit in O(1), flips it, and confirms residue 0. This is exactly
   acarsdec's `syndrom.h` / `fixprerr` scheme; it covers a lone error
   anywhere — including a parity-less BCS byte, where odd-parity gives no
   localization. Table span 256 bytes × 8 bits (acarsdec covers 242).
2. **Parity-guided multi-error fallback** (`correct_errors`). When the
   residue is not a single-bit error, fall back to the bad-parity-character
   search: each suspect (a character that failed odd parity) is assumed to
   hold one flipped bit; up to **3 suspects** are searched jointly (8
   candidates each → 8³) for the combination that restores residue 0. A
   parity-clean body that still fails (and is not a single-bit error) is
   not recoverable here and is left failed.

`fixed_bits` and `parity_errors` are reported on the frame
(`fec_corrected`/`errors` in `DecodeQuality`). Frames that still fail with
more than 8 bad-parity characters are dropped as noise (tolerant sync makes
false starts common). Runaway collections (>250 chars, lost suffix) reset
to hunting.

`modulate.rs` is the matching ARINC 618 modulator (frame octets +
MSK-on-AM IQ) used for loopback; it shares no state with the decoder, so a
convention error on either side surfaces as a loopback failure.

### Generic sublabel / MFI (`xng-mode-acars/src/sublabel.rs`, ACARS-3.2)

ARINC 620 lets a message carry a 2-char *sublabel* and optional *MFI* at
the front of the text. libacars implements the byte-grammar (downlink
`#xxB`, uplink `- #xx`, optional MFI `/yy ` space-terminated) but gates it
on `label == "H1"`. The shared `xng-acars` crate ports that H1 path
verbatim; this module reuses the **identical grammar** (mirroring
libacars's index arithmetic) on the wider `#`-sublabel family — canonically
**H2** (the documented structural twin of H1) — without modifying the
shared crate. `to_message` only consults it when `xng-acars` produced no
H1 sublabel, never shadows H1, and only emits when the sentinel is actually
present (no forcing). Surfaced as `sublabel`/`mfi` on `AcarsCore`.

`xng-acars::block` is the parallel **octet-level** ACARS block parser used
by carriers that deliver ACARS as bytes rather than an MSK bitstream
(VDL2 AOA, HFDL, Aero, Iridium SBD) — same header/suffix/CRC/parity
handling, no demod. It additionally splits the downlink MIN (see below).

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
and FPN do not claim the text): OHMA/RYKO marker behind optional routing
prefixes, the duplicated-first-block workaround, base64 → zlib → JSON.

**ARINC 702 flight plan (FPN)** (`fpn.rs`) — H1 text beginning `FPN/`. A
`:`-separated key/value record behind a `[SN…/][FN…/][TS…/]<status>`
header (`RI`=Route Inactive / `RP`=Route Planned). Decoded fields:
`flight_number`, `serial_number`, `origin` (`DA`), `destination` (`AA`),
`company_route` (`CR`), `departure_runway` (`R`), `departure_procedure`
(`D`), `arrival_procedure` (`A`), `approach_procedure` (`AP`), and the
aircraft-route (`F`) waypoints. Route values are `.`/`..`-tokenized;
`NAME,N12345W123456` annotations decode to decimal-minute lat/lon (shared
`position::decode_decimal_minutes`). The trailing 4 chars are the message
checksum, rendered `0x….` lowercase to match acars-decoder-typescript.
Embedded CR/LF inside split coordinates are stripped first. Verified
field-exact against the documented `Label_H1_FPN.test.ts` examples
(landing/full-flight/in-flight/with-newlines).

**Label 5Z "Airline Designated Downlink"** (`airline5z.rs`) — United
Airlines telex / structured free-text on label **5Z** with a leading `/`.
Two shapes: `/TXT\r\n<free text>` → plain telex; `/<TYPE> <args>` → a typed
downlink whose 2-char `<TYPE>` maps to a description (United's 24-entry
message-type table, e.g. `B3`=Request Departure Clearance, `C3`=Off
Message, `EO`=In Range). For `B3`/`C3` the IATA origin/destination pair is
broken out; `B3` additionally yields `day` and `arrival_runway`. Unknown
types are not decoded (matching the TS plugin's `decoded=false`). Verified
against the documented `Label_5Z_Slash.test.ts` examples.

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
(`APM_REPORT`/`APM`, `ATA`, `AL`, `ECT`, `FDE`, `FLR`, `LIGHTS`, `MIL`,
`MPF`, `PAGE`, `WRN`, and the `.01`/`.1` failure-record form),
longest-prefix matched; bodies are not parsed per-sub-type yet.

**Downlink MIN split** (`min.rs`) — libacars splits a downlink block's
4-char MIN into the 3-char message number (`msg_num`), the 4th sequence
character (`msg_num_seq`), and the reassembly index `msg_num_seq - 'A'`
(only for `'A'..='Z'`; non-letter 4th chars yield `seq = None` rather than
a bogus index — the embedded-NUL→`.` and digit edge cases). `block.rs`
surfaces this as the `min` field. `is_downlink_block` mirrors libacars's
`IS_DOWNLINK_BLK` (digit = downlink, letter = uplink).

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
  PS/POS (degrees+tenths-of-a-minute, `43312`→43.52°), the legacy `4J`
  literal-dot form (`N5043.5E01121.8`), and the decimal-minute form
  (`N12345W123456`) reused by the FPN route decoder.
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

`apply_reassembly` stamps that verdict onto `AcarsCore.assstat` for
**every** CRC-OK message that passes the reassembler (not just completed
ones, ACARS-5.1) — so even a lone single-block message carries `skipped`.
It surfaces in the native message JSON and in the acarsdec-JSON feed
(`src/outputs/acarsdec_json.rs`, omitted when the message never went
through the reassembler). That feed also now emits the per-burst `noise`
floor field (dBFS, from `SignalQuality.noise_db`, omitted when unmeasured),
matching acarsdec's `noise`.

## Validation / oracles

- **libacars** (MIT, attributed) is the structural oracle for ARINC 622,
  ADS-C, CPDLC element tables, MIAM, OHMA, media advisory, sublabel/MFI,
  the downlink MIN split, and reassembly semantics. The four real off-air
  ADS-C messages from libacars' `examples/adsc_get_position.c` are
  field-exact conformance fixtures (`tests/real_messages.rs`: lat/lon,
  altitude, track, mach, wind, temp to 1e-6/1e-4).
- **acarsdec** (`label.c`/`output.c`/`syndrom.h`) fixes the OOOI field
  offsets, JSON field names, Q-series event mapping, `assstat` strings,
  and the syndrome-table FEC. The single-bit FEC table is verified against
  acarsdec's published `syndrom.h` (canonical entries asserted verbatim:
  `syndrom[0]=0x1189`, `[7]=0x8408`, `[1935]=0x721c`, …; full 1936-entry
  table validated offline) — an external oracle, **not** a self-loopback —
  and recovery is exercised on the ARINC 618 "K7" block after a bit flip.
- **airframes' own references** — `acars-message-documentation` (research
  notes per label) and `acars-decoder-typescript` (coordinate utils,
  Q-series/CFB wording, the FPN/ARINC-702 and 5Z plugins) back every
  position/met/Q/CFB/FPN/5Z description and the real example strings the
  tests assert against.
- **ARINC 618** + reveng CRC catalogue back the PHY/framing/BCS; the "K7"
  CRC example is fenced in `xng-dsp`. ARINC 620-4 App C backs the extended
  sublabel→SMI families; ARINC 702 backs FPN.
- **RF loopback** (`modulate.rs` → decoder) covers the full demod/frame
  chain at the channel rate and across multiple offset channels in one
  wideband capture, including OOOI/position surfacing in the message body,
  and the syndrome/parity error correction (single-bit in body, in BCS,
  two separate single-bit errors, one-bit sync error).
- **Live**: the production Airframes feed (`KE-KSMF-ACARS1-TEST`).

## Known limitations / intentional gaps

- No frame-count benchmark vs acarsdec — no vendored off-air capture (the
  capture-able captures are too large to fence in CI, and unlike the newer
  narrowband modes — SONDE/NAVTEX/UAT — no small representative ACARS IQ
  fixture has been cut). Confidence is the live feed + loopback +
  field-exact app vectors; ACARS appears in neither the count-gated nor the
  oracle-fixture rows of [BENCHMARKS.md](BENCHMARKS.md).
- FEC corrects a single bit error anywhere (O(1)) and small multi-error
  patterns localized by parity; a non-single-bit error in a parity-clean
  body (or >3 bad-parity suspects, or >8) is not corrected.
- CR1/CC1/DR1 CPDLC (context-management) bodies are reported with verified
  raw hex, not ASN.1-decoded (different ASN.1 from AT1).
- CPDLC element arguments are decoded for the implemented shapes; others
  keep the bracketed template (e.g. `[frequency]`); trackDetail /
  routeInformationAdditional reported present-but-undecoded.
- `#CFB` sub-types are classified, not body-parsed.
- FPN surfaces route fields/waypoints; the `TS` timestamp token is parsed
  but not surfaced as a structured time, and the checksum is rendered, not
  verified.
- 5Z decodes United's documented type table; unknown types are left
  undecoded (no guessing).
- Generic sublabel extension is limited to H2 (the only documented twin of
  H1); it never overrides the upstream H1 decode.
- MIAM CRC fields are parsed but not verified (libacars default).
- The WMO-BUFR AMDAR met binary is deliberately unsupported (no real
  reference to verify against).
- No equalizer in the demod (unnecessary at VHF SNRs).

## References

- ARINC Specification 618 (Air/Ground Character-Oriented Protocol) §2–4.
- ARINC 622 (ATS data link), ARINC 620-4 (Q-series labels + App C
  label/sublabel→SMI), ARINC 702 (flight plan / FPN), ARINC 841 (MIAM).
- ICAO Annex 10 (ISO-5 character set); reveng CRC catalogue (CRC-16/KERMIT).
- libacars (MIT) — `arinc.c`, `adsc.c`, `media-adv.c`, `acars.c`
  (sublabel/MFI + MIN), `miam.c`, `miam-core.c`, `ohma.c`, `reassembly.c`,
  FANS asn1c tables.
- f00b4r0/TLeconte acarsdec — `label.c`, `output.c`, `syndrom.h`, `acars.c`
  (`fixprerr`/`fixdberr`).
- airframes `acars-message-documentation`, `acars-decoder-typescript`
  (`ARINC_702.ts`/`Label_H1_FPN`, `Label_5Z_Slash`).
- [BENCHMARKS.md](BENCHMARKS.md) (sibling modes), [HFDL.md](HFDL.md),
  [VDL2.md](VDL2.md), [IRIDIUM.md](IRIDIUM.md) (carriers sharing this layer).
</content>
</invoke>
