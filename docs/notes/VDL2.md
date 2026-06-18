# VDL Mode 2 — implementation notes

Native VDL Mode 2 demod/decode for xng-mode-vdl2 (v0.20.0). D8PSK at
10 500 sym/s, Annex 10 Vol III. Clean-room — see PROVENANCE.md; dumpvdl2
is read for facts (protocol constants, the integer→name dictionaries) and
used as an off-air oracle only.

Result: 44 AVLC frames on the sigidwiki off-air capture, against
dumpvdl2 2.6.0's 41, identical at 50 / 100 / 105 kS/s. CI bench floor
`vdl2_offair >= 42` (bench/baselines.json) runs the full capture at
105 kS/s; the vendored 6 s fixture asserts the chain end-to-end.

## Pipeline

Per channel (`lib.rs::Vdl2ChannelDecoder`): wideband IQ → DDC (or
selectivity FIR on the no-DDC path) → channel IQ → `demod::Vdl2Demod`
(acquisition, header FEC, deinterleave + RS) → `avlc::scan` → per frame:
ACARS-over-AVLC, or ATN transport (X.25 → CLNP/COTP/ES-IS/IDRP → ATN-B1
CPDLC / CM), or a structured AVLC body (S/U frames, XID, FRMR). Source:
`crates/xng-mode-vdl2/src/`.

## Channel rate

Auto-selected from the capture rate (`lib.rs::Vdl2ChannelDecoder::new`):

- 105 kS/s when the input divides into it — an exact 10 samples/symbol.
  At 100 kS/s every symbol center lands at a fractional sample and the
  linear interpolator's error becomes decision noise; integer sps
  removes it.
- 100 kS/s when the input divides into it. Every real SDR rate
  (2.4M / 3M / 6M) does, so this is the usual operating point.
- 50 kS/s floor (≈4.76 sps). Also the vendored-fixture path.

Symbol instants are linearly interpolated, so no integer relationship
between channel rate and symbol rate is required. The preamble-fit search
grid is symbol-denominated, not sample-denominated — denominating it in
samples silently shrinks the search window as the rate rises (it once
dropped decodes 16 → 9 when the rate was raised). Grid is ±0.63 symbols,
floored at ±3 samples (the original 50 kS/s width).

## Selectivity FIR (no-DDC path)

When input rate == channel rate and offset is zero (fixture and off-air
harness runs), no DDC runs, so the demod would otherwise see the full
input Nyquist band of noise. A flat-in-band lowpass fills that path
(`lib.rs`): 101 taps, -6 dB point at the symbol rate (Rs = 10.5 kHz) so
it is flat through the RC band edge (±8.4 kHz) and the windowed-sinc
transition sits entirely in the noise-only region. ~2.5-3 dB sensitivity
(`examples/sensitivity.rs`). When a DDC runs its decimation filter
already provides this.

This is NOT a matched filter — see the matched-filter trap below.

## Acquisition: coherent preamble phase-pattern fit

Two stages (`demod.rs`).

Coarse trigger is differential: correlate the 16-symbol unique word as
Δφ products (`uw_correlate`). Differential is CFO-immune, so it fires
regardless of carrier offset. A weak per-symbol energy-consistency gate
kills locks straddling the burst edge (near-zero products against
silence) while tolerating the legitimate phase-transition dips that real
preambles show at low sps. Trigger threshold 0.6.

Fine sync is a coherent fit over the whole preamble (`preamble_fit`).
Over a fine timing grid around the trigger, compare the unwrapped
per-symbol phase trajectory of the 16 UW symbols against the known
cumulative UW phase ramp and fit residual ≈ a + b·k by weighted least
squares (weights = sample energy). The minimum-cost grid point jointly
yields the sync point, the carrier phase (a, absorbed by the differential
decisions) and the per-sample CFO (b, the `dphi` derotation). This uses
all 16 symbols coherently; the differential correlation argument uses
only 15 transitions non-coherently and is far noisier.

The fit cost gates acceptance: `FIT_COST_MAX = 0.25` rad². True preambles
on the off-air capture fit below ~0.11; random data sits above ~0.5. The
low trigger threshold is only safe because of buffer retention (below).

The fit's CFO slope `b` (rad/symbol) is no longer discarded (VDL2-7): it
is surfaced as `freq_skew_hz = b·Rs/2π` on the `Burst`/`Vdl2Frame` and into
`SignalQuality.freq_skew_hz`. An optional `--max-ppm` (also `max-ppm` in the
station TOML) rejects a candidate whose `|CFO|` exceeds the limit in ppm
against the ~137 MHz band, continuing the hunt rather than collecting it;
default off. Verified by synthesizing a burst with a known carrier offset
and asserting the recovered skew tracks it (independent ground truth, not a
decode loopback).

## Symbol decisions

Per-symbol differential D8PSK (`collect`): Δφ of consecutive symbol
centers, derotated by the fitted `theta`, to the nearest π/4 multiple →
inverse Gray triplet (`GRAY_INV`), descrambled on the fly. Decision-
directed residual tracking adapts `theta` each symbol (`PHASE_GAIN =
0.1`). The |residual| at the π/4 grid is kept per symbol as a decision
confidence for RS erasure marking.

Differential beats coherent/absolute detection on real captures: the
sigidwiki signal has oscillator phase wander that differential
cancels and absolute tracking does not (a UW-trained LMS equalizer with
absolute D8PSK decoded 1 frame vs 17). Decision-directed differential
is the only in-burst adaptation; an equalizer's 16-symbol training leaves
the taps part-converged and injects more ISI than it removes at these sps.

## Gated noise-floor estimator

The energy gate's EMA (`hunt`) learns the floor only from samples below
the gate, with a tiny up-creep for re-convergence. Learning from burst
power would inflate the floor for ~0.1 s and shadow rapid back-to-back
transmissions — exactly the XID/ack exchanges in the capture. Gating it
took 17 → 19+ frames and made the count flat across `ENERGY_FACTOR` 8-20
and trigger threshold 0.4-0.6, where the symmetric estimator wobbled.

## Burst header

`header.rs`. Reserved symbol (3 bits) + 17-bit transmission length
(LSB first) + 5-bit (25,20) header FEC over the spec H matrix (Annex 10
Table 6-2). Decode checks the syndrome, then corrects a single bit error
by exhaustive flip; spec-derived parity vectors for TL ∈ {1, 100, 1000,
131071} pin the H matrix. `MAX_TL = 131_071`.

## Buffer retention / rewind (makes a low trigger safe)

A false UW lock can pass the 25-bit header FEC with a bogus length and
"collect" through a real burst. The buffer is retained back to the
collecting burst's UW start; on RS failure the hunt rewinds to
`uw_start + 1`, so any real burst inside the consumed span is still
buffered and gets retried. This is what makes lowering the trigger
threshold safe: a false header decode that fails RS rewinds without
swallowing a real burst. Worst case (max TL) this holds ~150 KB at
50 kS/s.

Two guards in demod acceptance:

- Bogus-length cap: header lengths above 16 000 bits are rejected
  outright (false locks have passed FEC with absurd lengths — up to
  3.4 s of "collection").
- A burst that already failed RS is deterministic; a re-detection that
  refines to the same UW position is skipped past, not retried (it would
  livelock until the noise floor rises).

## FEC: RS(255,249) and the octet convention

`interleave.rs`. Data octets fill a c-row × 255-column table row-major
(c = ⌈TL/1992⌉; short final row virtually zero-filled to 249 data
octets). RS(255,249) checks occupy columns 250-255 with shortening: rows
of ≤2 octets transmit no checks, 3-30 transmit 2, 31-67 transmit 4, ≥68
all 6. Transmission reads column-by-column, skipping virtual fill and
untransmitted checks.

The decisive fix: RS is computed over octets assembled **LSB-first (HDLC
wire order)**. MSB-first packing hands the RS stage bit-reversed symbols
and it rejects perfect codewords. This single-line convention was the
entire 19 → 44 gap.

## Soft-decision RS erasures

`deinterleave_soft`. Each RS row is tried as-is; on failure, retry once
erasing the two least-confident transmitted octets (per-symbol |residual|
mapped to per-octet worst-bit confidence; flagged octets must have
residual > 0.20 rad). RS trades one error of budget for two erasures, so
2·errors + erasures ≤ 6 with untransmitted checks already consuming part
of the budget — the one rung of two erasures keeps a two-error margin.

Erasure-assisted decodes rewind the cursor like a failure (do NOT advance
the hunt past the burst) so a miscorrection cannot swallow a later burst.
This guard was earned: an unbounded erasure ladder "decoded" every burst
(rs_fail 43 → 0) while real frames DROPPED 17 → 10 from cursor skips over
hallucinated codewords. The machinery is free on the happy path and
regression-proof by construction; on the sigidwiki capture (24-30 dB SNR)
it yields nothing the FCS accepts — that capture's losses were never FEC
headroom.

## AVLC link layer

`avlc.rs`. The descrambled, RS-corrected bit stream is destuffed and
scanned for `FLAG(0x7E) | dst(4) | src(4) | control(1) | info | FCS(2)
| FLAG` frames (ISO/IEC 13239, ETSI EN 301 841-2). Every accepted frame
is FCS-validated; the FCS octet order is pinned little-endian (low octet
first, ISO/IEC 13239 §4.4 / dumpvdl2 GOOD_FCS 0xF0B8) — the byte-swapped
"accept either" path was dropped because it false-accepted ~1 in 65536
bad frames. RS pass alone never emits a frame; the FCS is load-bearing.

Decoded fields:

- **Addresses** — 27-bit DLS address parsed bit-exact from the 4-octet
  field: 24-bit specific address (rendered as the 6-hex ICAO address for
  aircraft), address-type code (`Aircraft`, `GroundIcao`,
  `GroundDelegated`, `AllStations`, `Reserved`), and the status bit
  (A/G on dst octet 1, C/R on src octet 5).
- **Control** — full ISO/IEC 13239 repertoire (`parse_control`):
  I-frames (N(S)/N(R)/P), Supervisory (RR, RNR, REJ, SREJ), Unnumbered
  (UI, DM, DISC, UA, SABME, FRMR, XID, TEST). The P/F bit is masked off
  the U-frame match so SABME 0x6F and 0x7F both decode.
- **Payload class** — `Acars` (info begins 0xFF, AOA), `Atn{ipi}`
  (0x81 CLNP / 0x82 ES-IS / 0x83 IDRP per ISO TR 9577), `Xid`, `Empty`,
  `Other{first}`.

## XID handoff parameters

`avlc::parse_xid` walks the ISO/IEC 8885 structure: FI octet, then
`GI | GL(2, big endian) | params{PI, PL, PV}`. A param claiming more
bytes than its group holds returns None rather than emitting half-parsed
garbage. Two parameter sets are named (`vdl_param_name` /
`pub_param_name`), cross-checked against dumpvdl2 `xid_vdl_params` /
`xid_pub_params` (the integer→name assignment only):

- **VDL private (group 0xF0):** parameter-set-id, connection-management,
  signal-quality, xid-sequencing, avlc-specific-options,
  expedited-sn-connection, lcr-cause, autotune-frequency (0x40),
  replacement-ground-stations (0x41), timer-t4 (0x42), mac-persistence,
  counter-m1, timer-tm2/tg5/t3min, ground-station-address-filter (0x48),
  broadcast-connection, modulation-support, alternate-ground-stations,
  destination-airport (0x83), aircraft-location, frequency-support-list
  (0xC0), airport-coverage, nearest-airport-id, atn-router-nets,
  system-mask, timer-tg3/tg4, ground-station-location.
- **Public ISO 8885 HDLC (group 0x80):** parameter-set-id,
  procedure-classes, hdlc-options, n1/k up/downlink, timer-t1-downlink,
  counter-n2, timer-t2.

Typed value decode beyond raw hex: the 2-octet VDL2 frequency field
(autotune 0x40 and each frequency-support-list entry) → MHz via
`freq_khz = (raw12 + 10000)·10` rounded up to the next 25 kHz, with the
modulation-support nibble split off the top 4 bits (`decode_vdl2_freq`,
matches dumpvdl2 `parse_freq`); timer/counter parameters → big-endian
integer; printable parameters (e.g. destination-airport "KSMF") → text;
address-list parameters (replacement-GS 0x41, GS-address-filter 0x48,
alternate-GS 0x82, system-mask 0xC5; frequency-support-list 6-octet
entries) → AVLC addresses via the standard parser. Parameter IDs whose
binary layout is not verified against the spec stay as hex, not guessed.

The earlier table mis-numbered the entire 0x40–0x49 range (it labelled
0x42 "destination-airport" — that is 0x83; 0x42 is Timer T4); fixed and
pinned by tests.

## FRMR

`avlc::parse_frmr` expands the 3-octet basic (modulo-8) Frame-Reject
info field (ISO/IEC 13239 §5.5.3.5): the rejected control field (decoded
via `parse_control`), the rejecting station's V(S)/V(R), the C/R of the
rejected frame, and the W/X/Y/Z reject-reason flags.

## ATN transport (`atn.rs`)

Clean-room from the public ISO framings (ISO/IEC 8208 X.25, 8473 CLNP,
8073 COTP, 9542 ES-IS, 10747 IDRP) as profiled by ICAO Doc 9776/9705/
9880; dumpvdl2 was consulted only for protocol constants and the
integer→name dictionaries. ATN rides in I-frame info fields: an ISO 8208
packet on most links, or bare CLNP/ES-IS/IDRP. The network-layer entry
`parse_network` dispatches on the NLPID first octet (0x81 CLNP / 0x82
ES-IS / 0x83 IDRP; anything else → `clnp-compressed?` label). Decoded
into JSON nested under the message body.

- **X.25 packet layer (`parse_x25`)** — modulo-8 profile (GFI 0bxx01,
  tolerates Q/D). DATA (P(S)/P(R)/M-bit + user data), CALL-REQUEST /
  CALL-ACCEPTED (BCD address block, facilities, SNDCF, call-user-data),
  CLEAR / RESET / RESTART request (cause + diagnostic) and their
  confirmations, DIAGNOSTIC (0xF1), and RR/RNR/REJ supervisory. Three
  cause tables (clear/reset/restart, ITU-T X.25 Table 5-7) and a
  ~150-entry diagnostic table (X.25 Annex E + ISO 8208 + ICAO Doc 9705
  Table 5.7-3 / Doc 9880 extensions) resolve to text; a cause octet with
  bit 8 set is normalised to 0 (remote-DTE bits) for the lookup.
  Facilities are reported numerically (code + params) — naming deferred.
- **X.25 SNDCF (`parse_sndcf_field`)** — the Subnetwork Dependent
  Convergence Function field the ATN profile (ICAO Doc 9705 §5.7) places
  between the facility block and the call user data. On Call-Request it is
  `id(0xC1) | length | version(=1) | … | compression-bitfield`; on
  Call-Accept it is a single compression octet. The compression-support
  bitfield decodes against the ATN algorithm set (ACA 0x40, DEFLATE 0x20,
  LREF 0x02, LREF-CAN 0x01) plus the M/I bit 0x10. Decoding the SNDCF
  also un-offsets the CUD's network-protocol identifier (previously the
  field was swallowed into the CUD).
- **M-bit reassembly (`X25Reassembler`)** — DATA packets with M=1 are
  buffered per logical channel and concatenated until M=0 before the
  network layer is parsed; 60 s per-LCN timeout.
- **CLNP (`parse_clnp`)** — full uncompressed ISO 8473 header: PDU type
  (DT 0x1C / ERQ 0x1E / ERP 0x1F / ER 0x01), version, lifetime, the full
  flags byte (SP 0x80 / MS more-segments 0x40 / E·R 0x20), segment length,
  dst/src NSAPs (hex), then the options part. The optional 6-octet
  segmentation part (data-unit id, segment offset, total length) is
  decoded when SP is set. Unsegmented DT payloads recurse into COTP;
  a fragment (offset ≠ 0 or MS set) is *not* parsed as COTP — its data is
  partial, so COTP waits for reassembly (below).
- **CLNP options (`parse_clnp_options`)** — the header options part walked
  as `type|length|value`, naming the X.233 set (QoS-maintenance 0xC3,
  discard-reason 0xC1, padding 0xCC, priority 0xCD, security 0xC5,
  source-routing 0xC8, record-route 0xCB, …). The **Security option
  (0xC5)** is expanded as the ATN Security Label (ICAO Doc 9705 §5.6 /
  Doc 9880): leading globally-unique format octet (0xC0), then the
  security-registration-ID octet string, then the length-prefixed
  security-information part. Each tag set
  (`name-len | name | set-len | value`) is parsed against the ATN
  security-tag dictionary — **traffic-type (0x0F)** with
  type/category/route-policy, **security-classification (0x03)**,
  **subnetwork-type (0x05)** with subnet name + permitted-traffic-types
  bitfield, **supported-ATSC-classes (0x06/0x07)** as an A..H class
  bitfield.
- **CLNP multipart reassembly (`ClnpReassembler`)** — NEW. ISO 8473 §6.7:
  derived PDUs of one initial PDU share a data-unit identifier; each
  carries a fragment of the data part at its *segment offset*, and the
  initial PDU's *total length* (header + complete data) bounds the data
  unit. Fragments are placed by offset, so **out-of-order arrival
  reassembles correctly**; the first segment's header is preserved and on
  completion a single de-segmented CLNP PDU is rebuilt (more-segments flag
  cleared, `hdr[4] &= !0x40`) and handed to the normal CLNP/COTP walk.
  Keyed by **(src NSAP, dst NSAP, data-unit id)** with a 60 s timeout
  (mirrors the X.25 M-bit reassembler). Wired in `lib.rs::decode_network`,
  *after* X.25 M-bit reassembly; a fragment that does not complete a data
  unit surfaces its own header plus `reassembling: true`.
- **COTP (`parse_cotp`, ISO 8073 / X.224)** — all 10 TPDU types:
  CR/CC/DR/DC/ED/DT/AK/EA/RJ/ER. Each fixed header is parsed
  (dst/src references, CR/CC protocol-class + options, DR disconnect
  reason, ER reject cause, DT/ED end-of-TPDU flag, sequence numbers and
  flow-control credit for the data TPDUs), in both the normal (7-bit
  sequence) and extended (31-bit sequence) formats — extended signalled by
  an odd length-indicator. The variable part is parsed as
  `type|length|value` parameters: ATN checksum (0x08), TPDU-size
  (0xC0 → 2^value bytes), priority (0x87), inactivity timer (0xF2), and
  the rest of the X.224 set. DR-reason and ER-cause dictionaries resolve
  to text. DT/ED user data is handed to the ATN-B1 application decoders.
- **COTP TSDU reassembly (`CotpReassembler`)** — NEW. ISO/IEC 8073 §6.6
  normal-data TSDU segmentation: consecutive DT TPDUs on one connection
  (keyed by destination reference) carry user-data fragments, the final DT
  bearing the end-of-TSDU (EOT) flag. A single-segment TSDU (first DT has
  EOT set, seq 0) passes straight through; a multi-segment TSDU buffers
  fragments in TPDU-sequence order until the EOT DT, then returns the
  complete TSDU for the upper-layer (ULCS/CPDLC/CM) decode. An out-of-
  sequence or duplicate DT returns `None` rather than reassembling a
  corrupt TSDU. Wired in `lib.rs::cotp_reassemble` (single-segment TSDUs
  are still decoded inline by `parse_cotp`); only DT carries TSDU
  segmentation (ED is expedited and decoded directly). The reassembled
  user data is dispatched by `parse_cotp_user_app`.
- **ES-IS (`parse_esis`, ISO 9542)** — ESH (type 2, count + SAs) / ISH
  (type 4, single NET) with holding time and advertised NSAPs (hex), plus
  the trailing option TLVs: Mobile-Subnetwork-Capabilities (0x81),
  ATN-Data-Link-Capabilities (0x88), Priority (0xCF), Security (0xC5).
- **IDRP (`parse_idrp`, ISO 10747)** — 30-octet BISPDU common header
  (type, sequence, ack, credit offered/avail). All six PDU types named
  (OPEN, UPDATE, ERROR, KEEPALIVE, CEASE, RIB-REFRESH). OPEN body decodes
  version / hold-time / max-PDU-size / source-RDI (the reliably-framed
  leading fields; the variable RIB-Atts / Confed-IDs / auth tail stays
  hex). UPDATE decodes withdrawn-route IDs, path attributes (all 16 types
  named per §7.12; 1-octet scalars decoded, else hex), and NLRI prefixes
  (CLNP NLRI flagged). ERROR resolves the top-level code (5 names) and
  the code-keyed subcode dictionary to text.
- **Compressed CLNP** — ICAO 9705 LREF/deflate variants are labeled
  (`clnp-compressed?` + first octet) but not expanded; layout unverified.

## ATN-B1 applications (`atn_cpdlc.rs`)

Protected-mode CPDLC and Context Management, decoded from the ICAO Doc
9880/9705 ASN.1 modules (`atn-cpdlc.asn` / `atn-cm.asn` / `atn-ulcs.asn`,
vendored as spec text in `docs/asn1/`, from Wireshark's transcription of
the ICAO standard — module text only). These are the oracle for the ATN
air-ground modules: libacars 2.x ships only FANS-1/A and does not carry
the ATN ASN.1, so it is not a reference here. Encoding is unaligned PER
(ITU-T X.691), hand-walked: constrained whole numbers, the normally-small
non-negative whole number (§10.6, for CHOICE extension-addition / extended
ENUMERATED indices), length determinants — now including the fragmented
form (§10.9.3.8, multiples of 16K chained until the final fragment) so
long BIT STRING / SEQUENCE-OF lengths decode rather than aborting the walk
— and IA5 strings (7 bits/char). Reached via COTP DT/ED user data;
`parse_apdu` tries protected CPDLC, then `parse_cm_logon`, then
`parse_cm_ground`.

- **CPDLC** — ProtectedAircraftPDUs (4 root alts) / ProtectedGroundPDUs
  (6 root alts): abort-user, abort-provider, startup, startdown, send,
  forward, forward-response. The root CHOICE is decoded as the extensible
  CHOICE it is (X.691 §22): when the extension bit is set the
  extension-addition alternative index (a normally-small whole number) is
  read and the PDU is reported as `extension-alternative` with that index,
  rather than mis-reading an extension addition as a root index. The
  ATCUplink/DownlinkMessage header decodes msg id/ref, DateTimeGroup
  (rendered ISO 8601), and the logical-ack preamble. The mandatory
  **integrityCheck** BIT STRING that follows the protected message is now
  consumed and, when non-empty, surfaced as a hex digest (`integrity_check`)
  instead of being left dangling. Message elements are looked up in the
  **full element tables** — 238 uplink + 114 downlink `(name, ASN.1 arg
  type, phraseology)` generated from the module (`atn_cpdlc_tables.rs`) —
  and rendered into readable text (`CLIMB TO FL360`, `WILCO`).
- **CPDLC abort reasons** — abort-user decodes PMCPDLCUserAbortReason and
  abort-provider PMCPDLCProviderAbortReason (each extensible ENUMERATED;
  ext bit then the root index, with the dictionary resolving to text). A
  real bug was fixed here: PMCPDLCUserAbortReason has 13 root values
  (0..12) → 4 bits, but was being read as 3 bits, truncating the index;
  PMCPDLCProviderAbortReason (8 values) stays 3 bits.
- **ATCForwardMessage / ATCForwardResponse** (the `forward` /
  `forward-response` ProtectedGroundPDUs root alternatives, ICAO Doc 9880
  PMCPDLCAPDUsVersion1). `forward-response` decodes the ATCForwardResponse
  ENUMERATED (success / service-not-supported / version-not-equal).
  `forward` (`read_atc_forward_message`) decodes the ForwardHeader
  (DateTimeGroup, AircraftFlightIdentification IA5(2..8), 24-bit aircraft
  address) and the ForwardMessage CHOICE — a plain (unprotected)
  up/downElementIDs BIT STRING carrying a header-less ATCUplink/Downlink-
  MessageData, walked through the same element tables (`atc_message_data`).
- **CPDLC argument values** — `read_argument` now covers **~63 argument
  types** (up from ~22), so the element walk no longer halts at the first
  previously-unsupported argument. Beyond the original Level / Time /
  Position / Speed / Degrees / Direction / Distance and their
  two-component compounds, the round added:
  - **Frequency** CHOICE (HF kHz / VHF·0.005 / UHF·0.025 MHz / 12-digit
    SAT NumericString) and UnitNameFrequency / PositionUnitNameFrequency /
    TimeUnitNameFrequency (UnitName = designation + optional name +
    facility-function enum);
  - **Altimeter** CHOICE (english in·0.01 / metric hPa·0.1), plus
    FacilityDesignation, Facility, FacilityDesignationAltimeter,
    FacilityDesignationATISCode;
  - **Code** (4-digit octal squawk), **ATISCode** (single IA5),
    **FreeText** (IA5 1..256), **VersionNumber** (0..15);
  - the ENUMERATED arguments TrafficType, ClearanceType, ErrorInformation,
    ToFrom, SpeedType, FacilityFunction (each with its X.691 extension bit);
  - **ProcedureName** / PositionProcedureName, **RunwayRVR**,
    **VerticalRate** CHOICE (·10 fpm / ·10 m/min),
    **RemainingFuelPersonsOnBoard** (Time + 1..1024);
  - the level/speed/time/position compound shapes previously unknown
    (LevelSpeedSpeed, PositionSpeedSpeed, TimeSpeed, SpeedTime,
    TimeSpeedSpeed, PositionLevelLevel, PositionLevelSpeed,
    PositionTimeTime, PositionTimeLevel, TimePositionLevel,
    TimePositionLevelSpeed, SpeedTypeSpeedTypeSpeedType, and that +Speed);
  - the distance/direction offset family
    (DistanceSpecifiedDirection, PositionDistanceSpecifiedDirection,
    TimeDistanceSpecifiedDirection, DistanceSpecifiedDirectionTime) and the
    to/from reports (ToFromPosition, TimeToFromPosition,
    TimeDistanceToFromPosition).
  An unsupported argument type still stops the element walk explicitly
  (its size is then unknown) and the element is flagged undecoded rather
  than guessed past — the staged FANS-1/A approach.
- **Route clearances** in `constrainedData` decode departure/destination
  airports, runways, procedures (arrival/approach/departure + transition),
  and the RouteInformation leg list (published fix/navaid, lat-lon,
  place-bearing, place-bearing-distance, ATS route designator); the
  routeInformationAdditional tail is flagged present-but-undecoded (it is
  last in the structure).
- **HoldClearance** (position/level/degrees/direction + optional LegType
  CHOICE distance/time) decodes fully. **DepartureClearance** decodes the
  mandatory head (flight id + clearance-limit position) and flags the
  deeply-nested FlightInformation / FurtherInstructions optional tail
  present-but-undecoded. **PositionReport** decodes the 3 mandatory fields
  (position / time / level) and, if any of the 19 OPTIONAL fields are
  present, returns `None` to stop the walk (their sizes are then unknown)
  rather than mis-decode.
- **CM (`parse_cm_logon`, `parse_cm_ground`)** — the dialogue that
  precedes CPDLC, now decoded in full from the ICAO Doc 9705 Edition 2
  CMMessageSetVersion1 module (`docs/asn1/atn-cm.asn`). CMAircraftMessage
  (3 root alts) covers logon-request, contact-response, abort;
  CMGroundMessage (6 root alts) covers logon-response, update,
  contact-request, forward-request, abort, forward-response. The address
  primitives are decoded structurally: **ShortTsap** (optional aRS 24-bit
  address + 10–11-octet loc/sys/nsel/tsel selector), **LongTsap** (5-octet
  RDP + ShortTsap), and the **APAddress** CHOICE over the two.
  CMLogonRequest / CMForwardRequest decode the flight id, mandatory
  cMLongTSAP, and every present OPTIONAL (ground/air-only application
  lists, facility designation, departure/destination airports, ETD
  DateTime). CMLogonResponse / CMUpdate decode both OPTIONAL application
  lists. CMContactRequest decodes the facility designation + LongTsap
  address; CMForwardResponse the ENUMERATED; abort the CMAbortReason
  ENUMERATED (10 root values → text). The per-entry application lists
  (`SEQUENCE SIZE(1..256) OF AEQualifierVersion[Address]`) are now walked
  fully (ae-qualifier, ap-version, optional APAddress) rather than reported
  present-or-absent only.

## ACSE / ULCS association control (`atn_cpdlc::parse_acse_apdu`)

The ULCS ACSE-1 module (`docs/asn1/atn-ulcs.asn`) carries the
association-establishment and -release PDUs that bracket a CPDLC/CM
dialogue. `parse_acse_apdu` decodes a bare ACSE-apdu bit-vector — an
extensible CHOICE with 5 root alternatives (AARQ / AARE / RLRQ / RLRE /
ABRT), all of which are recognized and dispatched:

- **RLRQ / RLRE** — the release reason (Release-request- /
  Release-response-reason ENUMERATED) is decoded.
- **ABRT** — the abort-source (acse-service-user / acse-service-provider)
  and the OPTIONAL abort-diagnostic ENUMERATED are decoded.
- **AARQ / AARE** — recognized but the full SEQUENCE bodies
  (application-context-name OBJECT IDENTIFIER, AP-title CHOICE nested
  through ACSE-1 / InformationFramework, EXTERNAL user-information) are
  **DEFERRED** — deeply nested and needing a captured PDU to pin the walk.

`parse_acse_apdu` is intentionally **NOT** wired into the COTP user-data
dispatch (`parse_cotp_user_app`, which tries CPDLC then CM only). On the
wire these APDUs sit beneath the ATN session (ISO 8327 / X.225) and
presentation (ISO 8823 / X.226) null encodings, whose framing is not in
the `docs/asn1` oracle set; auto-dispatching a bare ACSE-apdu from COTP
would alias the CPDLC/CM CHOICEs against an unverifiable null-encoding
frame, so it is held until a verified session/presentation layer.

## Outputs (`lib.rs::to_message`)

ACARS-bearing frames emit `MessageBody::Acars` (CRC flag + parity-error
count carried through; ARINC 622 applications — ADS-C, CPDLC-over-ACARS —
rendered by `xng_acars`). Every other frame emits `MessageBody::Vdl2 {
kind, details }`: `kind` is `xid` / `atn` / `avlc-<u/s-frame>` / `avlc-i`;
`details` always carries dst/src/control, plus the XID params, FRMR
expansion, ATN protocol label + nested transport JSON, and a truncated
info-hex preview. The raw frame octets are preserved on every message.
`fec_corrected` reports the RS-corrected octet count.

## Validation / oracles

- **Off-air oracle: dumpvdl2 2.6.0** on the sigidwiki VDL-M2 capture
  (CC BY-SA, 46.9 s, Amsterdam; I/Q convention inverted — dumpvdl2 also
  decodes nothing until conjugated). xng 44 vs dumpvdl2 41, CI floor 42.
  `tests/offair.rs` asserts ≥2 AVLC frames and CRC-valid ACARS from
  HB-IJW on the vendored 6 s slice.
- **Octet-level ground truth** from dumpvdl2 `--debug burst_detail`
  (post-deinterleave Data+FEC octets) — this, not frame counts, exposed
  the LSB-vs-MSB RS bug (see lessons).
- **Spec self-test vectors** in the suite: UW phase sequence, first 48
  scrambler keystream bits, header-FEC parity for TL ∈ {1,100,1000,
  131071}, AVLC FCS residue, RS encode/decode roundtrip.
- **Synthetic UPER vectors** for the ATN-B1 path, hand-assembled
  bit-by-bit from the module (no encode→decode loopback): WILCO downlink,
  CLIMB-TO uplink, CM logon-request, CM ground logon-response,
  cleared-route uplink, plus one worked vector for each newly-added
  argument type (the expected rendering derived from the module's
  resolution/unit constraint comments). The round-5/6 PER vectors:
  user-abort-reason at its correct 4-bit width, a set-CHOICE extension-
  addition alternative, a non-empty integrityCheck BIT STRING surfaced as
  hex, a chained fragmented length determinant (§10.9.3.8), the
  ATCForwardMessage / ATCForwardResponse PDUs, the full CM ground and
  aircraft set (contact-request LongTsap, forward-response, abort), and
  the ACSE RLRQ release-reason / ABRT source+diagnostic decodes.
- **Spec-derived transport vectors** built octet-by-octet (no loopback):
  every COTP TPDU (CR/CC/DR/DC/ED/DT/AK/EA/RJ/ER), the CLNP security label
  and options, X.25 SNDCF Call-Request/Accept, IDRP OPEN/UPDATE/
  KEEPALIVE/RIB-REFRESH, and the CLNP reassembler (in-order, out-of-order,
  unsegmented pass-through, and a COTP DT recovered across two segments).
- **RF loopback** (`tests/end_to_end.rs`): AOA frame with a real ADS-C
  payload + S-frame, both at 50/100 kS/s, plain and RC(α=0.6)
  pulse-shaped, plus a wideband-with-CFO path; the vendored 6 s off-air
  fixture is guarded by `tests/offair.rs`.

The dictionaries (XID param IDs, X.25 cause/diagnostic/SNDCF codes, COTP
TPDU/parameter/reason codes, CLNP option + ATN security-tag codes, IDRP
PDU/attr/error codes, ES-IS option IDs) were cross-checked against the
corresponding dumpvdl2 source tables — the integer→name assignments only,
never code or formatter text (clean-room; see PROVENANCE.md).

## Known limitations / intentional gaps

- Acquisition sensitivity, not FEC headroom, is the gap to a perfect
  haul; on the 24-30 dB sigidwiki capture soft RS yields nothing the FCS
  accepts.
- X.25 facilities reported numerically (no naming).
- Compressed CLNP (LREF / deflate) labeled but not expanded.
- IDRP OPEN's variable tail (RIB-Atts-Set / Confed-IDs / auth) stays hex.
- CPDLC: the still-unsupported argument types, the routeInformation-
  Additional tail, place-bearing-distance positions, the deeply-nested
  **DepartureClearance** FlightInformation / FurtherInstructions tail, and
  the 19 OPTIONAL **PositionReport** fields are not decoded — each stops
  its walk explicitly rather than emitting a guess. Both deferred tails
  need a captured PDU to pin the nested SEQUENCE-OF / CHOICE walks. (A set
  root-CHOICE extension bit and fragmented PER lengths are no longer gaps:
  the extension-addition alternative is reported by index, and fragmented
  length determinants are chained.)
- ACSE: AARQ / AARE bodies (application-context OID, AP-title, EXTERNAL
  user-information) are recognized but DEFERRED, and `parse_acse_apdu` is
  not auto-dispatched from COTP — both pending a captured PDU and a
  verified ATN session/presentation null-encoding frame.
- **Native ATN-B2 ADS-C over COTP** is not implemented — the deferred big
  bet (no ATN-B2 ADS-C ASN.1 module is vendored and no sample is in hand;
  the COTP plumbing, COTP TSDU reassembly, and CLNP reassembly that would
  feed it are in place).

## Standing lessons

**The self-consistent-loopback trap.** The 19 → 44 gap was a single
MSB-vs-LSB-first RS symbol-assembly bug. Every synthetic loopback passed
it because encode and decode shared the same wrong convention. It was
caught only by octet-level ground truth from dumpvdl2's own
`--debug burst_detail` output (post-deinterleave Data+FEC octets), which
showed zero differing octets — the demodulator had been bit-perfect on
the "failing" bursts all along. Rule: demand oracle ground truth at the
octet level, never frame counts or derived files; and validate any
claimed truth file with `avlc::scan` before trusting it (an earlier
round drew conclusions from `/tmp` `.bits` files that were our own failed
post-RS output, containing zero FCS-valid frames).

**The matched-filter trap.** A naive RRC(α=0.6) receive filter passes
every synthetic loopback and collapses off-air decode to ~1 frame (with
RS-passing bursts full of AVLC-invalid bytes). The Annex 10 TX pulse is
full raised-cosine — Nyquist by itself — so the noise-optimal zero-ISI RX
filter is flat in-band and zero outside (a plain lowpass past the band
edge), NOT an RRC; an RRC creates RC^1.5 ISI at the sampling instants.
Critically the lowpass -6 dB point must sit beyond the RC band edge —
cutting at 8.5 kHz eats the outer rolloff and breaks the Nyquist
property. Synthetic loopback is blind to this because the original test
modulator did not shape pulses; it now does, via `burst_iq_shaped`
(RC α=0.6, linear modulation), so loopback covers the realistic waveform.

**RS pass is weak evidence on short rows.** Rows of 3-30 data octets
carry 2 check octets (correct one error) — an RS pass there is ~0.4%
likely on random data. The AVLC FCS is load-bearing: never emit a frame
or change control flow on an RS pass alone. Symbol-offset and clock-skew
re-walks "rescued" bursts that were all RS-passing garbage with zero
0x7E flags; every one was rejected by the FCS.

**Buffer retention is what makes a low trigger safe.** Rewinding to the
collecting burst's UW start means a false header decode that fails RS
cannot consume a real burst. Lower the trigger threshold only with this
in place.

**Decode protocol, don't guess it.** Across the XID, X.25, COTP, CLNP,
IDRP and CPDLC work the recurring discipline is: parse the
reliably-framed fields, name them from a spec-pinned dictionary
cross-checked against the oracle, and leave anything whose binary layout
is unverified as hex (or stop the PER walk explicitly when a size is
unknown). A mis-numbered XID table (0x42 vs 0x83) shipped before because
a name was assumed rather than pinned; every dictionary now has a test.

## Architecture vs dumpvdl2

dumpvdl2 runs at 10 sps, does coherent preamble phase-pattern sync
(`pr_phase[]` cumulative expected phases; picks the sample where the
error vector is most constant, whose constant value is the carrier
phase), and carries an explicit per-sample CFO (`dphi`) across bursts.
xng's `preamble_fit` is the least-squares form of the same idea — joint
(sync, carrier phase, CFO) over the full preamble — and the same approach
(coherent preamble fit + no-DDC selectivity FIR) applies to HFDL's A1
acquisition. dumpvdl2 does NOT use a matched filter either; its symbol
decisions are single-sample `atan2` phases, like xng's.
