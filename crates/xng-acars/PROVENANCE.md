# Provenance — xng-acars

Ported from **libacars** (https://github.com/szpajder/libacars), MIT
license, Copyright (c) 2018-2023 Tomasz Lemiech <szpajder@gmail.com> —
porting is permitted with attribution, which this file and the crate
documentation provide.

Ported pieces and their sources:

- ARINC 622 envelope (`arinc.c`): IMI table (.AT1/.CR1/.CC1/.DR1/.ADS/.DIS),
  7-or-4-char ground station address rules, IMI(3)+air_reg(7)+hex payload
  layout, CRC-16 (poly 0x1021, MSB-first, init 0xFFFF) over the IMI+air_reg
  ASCII plus all binary bytes, validated by residue 0x1D0F.
- ADS-C (`adsc.c`): downlink and uplink tag tables, bit-level field layouts,
  and the exact scaling formulas (coordinate (180−90/2^19)·r/0xFFFFF,
  altitude ×4 ft, timestamp ×0.125 s, heading (180−90/2^10)·r/0x7FF with
  +360 wrap, wind direction (180−90/2^7)·r/0xFF, temperature
  (512−256/2^10)·r/0x7FF, speed ÷2, vertical speed ×16, distance ÷8).
- Media advisory (`media-adv.c`): label SA text format and link codes.
- Sublabel/MFI extraction (`acars.c`): H1 `#xxB`/`- #xx` and `/yy ` rules.

Test fixtures: the four real off-air ADS-C messages embedded in libacars'
`examples/adsc_get_position.c` (MIT), with field values cross-verified by
independent reimplementation before porting.

Differences from libacars: Rust-native types with serde serialization;
CPDLC (AT1/CR1/CC1/DR1) payloads are currently carried as verified raw hex
pending the FANS-1/A ASN.1 PER decoder; MIAM/OHMA not yet ported.

## Reassembly, MIAM, OHMA (2026-06)

Ported from MIT-licensed libacars with attribution:

- `reasm.rs`: multi-block reassembly semantics from reassembly.c +
  acars.c — key (tail, label, msg_num), downlink sequencing via the 4th
  message-number character, uplink sequencing via block id with the
  A..W wrap, the empty-uplink-ACK skip, per-bearer timeouts.
- `miam.rs`: miam.c / miam-core.c — ACARS CF frame map (T/F/K/S/A/Y/X),
  base85 armor ('!' offset, 'z' zero-word), bpad/hpad + '|' framing,
  v1/v2 DATA header layouts, DEFLATE bodies inflated as raw streams
  (windowBits −15 equivalence via miniz_oxide). CRC fields parsed but
  not verified (matches libacars default behavior).
- `ohma.rs`: ohma.c — OHMA/RYKO marker with downlink/uplink routing
  prefixes, the duplicated-first-block quirk workaround, base64 → zlib
  → JSON.

Synthetic roundtrip vectors (compress → render → parse) stand in for
off-air samples until label-MA/OHMA traffic is captured at the live
station.

## FANS-1/A composite arguments (2026-06)

Additional element-argument readers: the two/three-component composites
(PositionAltitude, TimeAltitude, PositionTimeAltitude, ...) compose the
existing verified readers; new scalars take their PER constraints from
libacars's asn1c tables (MIT, as before): VerticalRateEnglish (0..60,
100 ft/min), VerticalRateMetric (0..200, 10 m/min), Degrees (1..360,
magnetic/true), Direction (ENUMERATED 0..10), FreeText (IA5 SIZE
1..256). Route clearances remain unsupported (deep optional-laden
sequence; staged separately).

## FANS-1/A route clearances (2026-06)

FANSRouteClearance (ten optional components: airports, runways,
SID/STAR/approach procedures, airway intercept, the route-information
sequence) decoded with constraints from the libacars asn1c tables (MIT,
as before): FANSProcedure SIZE(1..6), route sequence SIZE(1..128),
route legs as published-identifier (fixname + optional lat/lon),
lat/lon, place-bearing pairs, place-bearing-distance (NM 0.1 / KM), or
airway designators. trackDetail legs and the trailing
routeInformationAdditional stay undecoded (reported as present).

## Reassembly-status names / `assstat` (2026-06)

`reasm.rs`: `Reasm::assstat()` returns the reassembly-status name acarsdec
emits in its JSON `assstat` field. The exact strings (`complete`,
`in progress`, `skipped`, `duplicate`, `out of sequence`) are taken from
libacars' `la_reasm_status_name_get` (reassembly.c); our `Incomplete`
(final block with sequence holes) maps to libacars'
`LA_REASM_FRAG_OUT_OF_SEQUENCE` → `"out of sequence"`.

## Q-series classification (2026-06)

`qseries.rs`: classifies the ARINC 620 `Q`-series link-test / squitter /
OOOI-event downlink labels (`Q0`–`Q7`, `QA`–`QX`). Descriptions are taken
from airframes' own published references (not invented): the
acars-message-documentation repo (`Q0` "ACARS Link Test", `Q2` "ETA
Report", `QF` "OFF Destination Report", `QQ` "OFF Report") and the
acars-decoder-typescript plugin descriptions (`QP` "OUT Report", `QR` "ON
Report", `QS` "IN Report"). The remaining OOOI-bearing `Q` labels are named
from the gate/wheels event each carries per f00b4r0/acarsdec `label.c`
(`QA` gate-out, `QB` wheels-off, `QC` wheels-on, `QD` gate-in, ...).

## OOOI text extraction (2026-06)

`oooi.rs`: OUT/OFF/ON/IN gate and wheels times plus departure/destination
airports and ETA, extracted from the message text. The per-label field
offsets and the airport/time event each label carries are a clean-room
port of f00b4r0/acarsdec `label.c` (`DecodeLabel` + the `label_*` helpers;
facts only, reimplemented) covering the `Q`-series (Q1/Q2/QA–QT) and the
airline-application labels acarsdec handles (10/11/12/15/17/1G/20/21/2N/
2Z/33/39/45/80/83/8D/8E/8S). The emitted JSON field names match acarsdec's
`output.c` exactly (`depa`/`dsta`/`eta`/`gtout`/`gtin`/`wloff`/`wlin`).
Unlike acarsdec's raw `memcpy`s we bounds-check every slice and validate
airport codes (4 alphanumerics) and times (HHMM range), dropping
misaligned fields rather than emitting junk.

## Winds-aloft / met (2026-06)

`met.rs`: decodes the verifiable winds-aloft met set from the free-text
`4J` "POSWX" position-and-weather report — wind direction/speed (`/WND
334060`), static air temperature (`/SAT -032`), true airspeed (`/TAS
490`) and altitude/flight-level (`/ALT 270` → 27000 ft). Field meanings,
example string and expected values are from airframes'
acars-message-documentation `research/4J.md`; the temperature `M`/`P`
sign convention is airframes' own (`research/H1/POS.md`,
acars-decoder-typescript `ResultFormatter.temperature`). The WMO-BUFR
AMDAR binary schema (NOAA `dcacar`) is intentionally out of scope: it is
not present in any airframes documented example, so there is no real
reference to verify a decoder against.

## H1 #CFB maintenance family (2026-06)

`cfb.rs`: classifies the H1 `#CFB` ("Crew Flight Bag") Boeing/Airbus
maintenance-telemetry family into its documented sub-types (`APM_REPORT`,
`ATA`, `AL`, `FDE`, `ECT`, `FLR`, `LIGHTS`, `MIL`, `MPF`, `PAGE`, `WRN`,
and the `.01`/`.1` failure-record form). The sub-type set and the
descriptions come from airframes' acars-message-documentation
`research/H1/CFB.md` acronym table (`CFB` = Crew Flight Bag, `APM` =
Aircraft Performance Monitoring, `FDE` = Flight Deck Effect, `FLR` =
Realtime Failure, `MPF` = Maintenance Planning Function, `WRN` = Warning,
`MIL` = Engine Spool Vibration Units) and `research/H1/CFB/CFB.01.md`;
sub-types without an acronym-table entry are described from the documented
example content. Tested against the real documented example strings.

## Free-text position reports (2026-06)

`position.rs`: extracts latitude/longitude from the free-text position
reports on labels `20`/POS, `4J` and `H1` POS. Clean-room port of the
coordinate decoders in airframes' own acars-decoder-typescript
(`utils/coordinate_utils.ts`, `utils/arinc_702_helper.ts`,
`plugins/Label_20_POS.ts`; facts only). Two packed conventions are
handled: label `20`/POS scaled-decimal (`38160` → 38.160°) and `H1`
POS / `4J` `PS`/`POS` degrees-plus-tenths-of-a-minute (`43312` →
43° 31.2′ → 43.52°), plus the legacy `4J` literal-decimal-point form
(`N5043.5E01121.8`). Verified against the real example strings and the
expected lat/lon in airframes' acars-decoder-typescript test suite and
acars-message-documentation (`research/20/POS.md`, `H1/POS.md`, `4J.md`).

## FANS-1/A additional argument readers (2026-06, ACARS-3.1)

`cpdlc/mod.rs`: further element-argument readers for the shapes that
previously fell to the bracketed template. PER constraints, CHOICE order
and value scaling are taken from the libacars asn1c tables (MIT, as
before) and the libacars text formatters (`asn1-format-cpdlc-text.c`):
DistanceOffset (CHOICE Nm 1..128 / Km 1..256, integer units), Distance
(CHOICE Nm 0..9999 tenths / Km 1..1024), Frequency (CHOICE hf
2850..28000 kHz / vhf 117000..138000 kHz / uhf 225000..399975 kHz both
rendered in MHz / 12-char NumericString satchannel), BeaconCode
(SEQUENCE OF SIZE(4) of octal digit 0..7), ProcedureName (SEQUENCE type
0..2 + IA5 1..6 + optional transition), Altimeter (CHOICE english
2200..3200 inHg×0.01 / metric 7500..12500 hPa×0.1), ATISCode (IA5
SIZE 1), RemainingFuel (HH:MM) + RemainingSouls (1..1024),
ErrorInformation (ENUM 0..16, labels from FANSErrorInformation.c),
VersionNumber (0..15), ICAOfacilitydesignation (IA5 SIZE 4), Tp4table /
ToFrom (ENUM 0..1), ICAOUnitName (facility-id CHOICE designation/name +
function ENUM 0..7) and ICAOUnitNameFrequency, plus the FANSPosition
`placeBearingDistance` CHOICE alternative (fixName + optional lat/lon +
degrees + distance). Composite elements (DistanceOffsetDirection,
PositionICAOunitnameFrequency, TimeDistanceToFromPosition, ...) compose
these readers.

Verification: each new shape is pinned to a spec-derived UPER body whose
EXPECTED decode was independently confirmed by running the same body,
wrapped in a valid ARINC-622 envelope, through the installed libacars
reference decoder (`decode_acars_apps`) — not an encode→decode loopback.
The headline case is a real off-air message from libacars'
`examples/decode_acars_apps.c` (`/AKLCDYA.AT1.9M-MTB...`, uM118 CONTACT
AUCKLAND control 123.900 MHz). FANSPositionReport (the deep position-report
SEQUENCE) and RouteClearance trackDetail remain undecoded (reported as
the bracketed template).

## FLIGHTPLAN (FPN) + 5Z telex / structured free-text (2026-06, ACARS-2.4)

`fpn.rs`: decodes the ARINC 702 flight plan carried on label H1 with the
`FPN/` preamble — header (route status `RI`/`RP`, optional flight number
`FN`, serial `SN`, timestamp `TS`), the `:`-separated key/value record
(`DA` origin, `AA` destination, `CR` company route, `R` departure runway,
`D` departure procedure, `A` arrival procedure, `AP` approach procedure,
`F` aircraft route), the trailing 4-character message checksum, and the
route waypoints (name + decoded position). Waypoint coordinates use the
degrees-plus-decimal-minutes convention (`N40010` → 40° 01.0′ → 40.017°),
reusing `position::decode_decimal_minutes`. Format, key table, status
codes and the coordinate conversion are a clean-room reimplementation of
airframes' own documentation and decoder: acars-message-documentation
`research/H1/FPN.md` and acars-decoder-typescript `plugins/ARINC_702.ts`
+ `Label_H1_FPN.test.ts` (facts only). Tested against the real off-air
example messages and their expected field/coordinate values in that test
suite.

`airline5z.rs`: decodes the label 5Z "Airline Designated Downlink"
United-Airlines telex / structured free-text family — the `/TXT` plain
telex message and the typed `/<TYPE> ...` downlinks (message-type table
from United), with origin/destination (IATA) + day + arrival runway for
the structured `B3` (request departure clearance) and `C3` (off message)
variants. Clean-room reimplementation of acars-decoder-typescript
`plugins/Label_5Z_Slash.ts` + `Label_5Z_Slash.test.ts` and
acars-message-documentation `research/5Z.md` (facts only). Tested against
the documented example messages and their expected fields.

## Raw MIN / 4th-char downlink rule (2026-06, ACARS-1.2)

`min.rs`: splits the downlink Message Identifier Number the way libacars
(`acars.c`) and acarsdec do — the 3-character message number (`msg_num`)
plus the 4th character (`msg_num_seq`), the per-message sequence
character. The block-id class follows libacars' `IS_DOWNLINK_BLK(bid) =
(bid >= '0' && bid <= '9')`, and the reassembly sequence index is
`msg_num_seq - 'A'` (`acars.c` `.seq_num = down ? msg->msg_num_seq - 'A'
: ...`, `.seq_num_first = 0`). The 4th-character edge cases are handled
explicitly: only `'A'..='Z'` yields a sequence index; other 4th bytes
(digits, punctuation, the `'.'` libacars substitutes for embedded NULs)
leave the index unset rather than producing a bogus value. `block.rs`
surfaces the split on `AcarsBlock::min` (a crate-local field — the shared
`AcarsCore::msg_num` keeps the combined 4-character value for
back-compat), and `reasm.rs` now derives its downlink sequence from the
same `min::split_downlink` helper. Verified against the libacars `acars.c`
field layout (`msg_num[4]` + `msg_num_seq`).

## MIAM file-transfer reassembly (2026-06)

File transfers spanning multiple label-MA messages reassemble per the
libacars semantics (MIT, as before): the FileTransferRequest registers
file id and size, segments numbered from 1 carry CORE-PDU text
fragments, completion at the declared size parses the combined text as
a CORE PDU (attached to the closing segment's message as
miam_file_complete). Abort frames cancel; 10-minute timeout.
