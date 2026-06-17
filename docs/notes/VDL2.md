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
packet on most links, or bare CLNP/ES-IS/IDRP. Decoded into JSON nested
under the message body.

- **X.25 packet layer (`parse_x25`)** — modulo-8 profile (GFI 0bxx01,
  tolerates Q/D). DATA (P(S)/P(R)/M-bit + user data), CALL-REQUEST /
  CALL-ACCEPTED (BCD address block, facilities, call-user-data),
  CLEAR / RESET / RESTART request (cause + diagnostic) and their
  confirmations, DIAGNOSTIC (0xF1), and RR/RNR/REJ supervisory. Three
  cause tables (clear/reset/restart, ITU-T X.25 Table 5-7) and a
  ~150-entry diagnostic table (X.25 Annex E + ISO 8208 + ICAO Doc 9705
  Table 5.7-3 / Doc 9880 extensions) resolve to text; a cause octet with
  bit 8 set is normalised to 0 (remote-DTE bits) for the lookup.
  Facilities are reported numerically (code + params) — naming deferred.
- **M-bit reassembly (`X25Reassembler`)** — DATA packets with M=1 are
  buffered per logical channel and concatenated until M=0 before the
  network layer is parsed; 60 s per-LCN timeout.
- **CLNP (`parse_clnp`)** — full uncompressed ISO 8473 header: PDU type
  (DT 0x1C / ERQ 0x1E / ERP 0x1F / ER 0x01), version, lifetime, segment
  length, dst/src NSAPs (hex). DT payloads recurse into COTP.
- **COTP (`parse_cotp`)** — ISO 8073 TPDU identification (CR/CC/DR/DT/ER)
  with dst/src references; DT user data is handed to the ATN-B1
  application decoders.
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
9880/9705 ASN.1 modules (vendored as spec text in `docs/asn1/`, from
Wireshark's transcription of the ICAO standard — module text only).
Encoding is unaligned PER (ITU-T X.691), hand-walked: constrained whole
numbers, length determinants (no fragmentation), and IA5 strings (7
bits/char). Reached via COTP DT user data; `parse_apdu` tries protected
CPDLC, then `parse_cm_logon`, then `parse_cm_ground`.

- **CPDLC** — ProtectedAircraftPDUs (4 root alts) / ProtectedGroundPDUs
  (6 root alts): abort-user, abort-provider, startup, startdown, send,
  forward, forward-response. The ATCUplink/DownlinkMessage header decodes
  msg id/ref, DateTimeGroup (rendered ISO 8601), and the logical-ack
  preamble. Message elements are looked up in the **full element tables**
  — 238 uplink + 114 downlink `(name, ASN.1 arg type, phraseology)`
  generated from the module (`atn_cpdlc_tables.rs`) — and rendered into
  readable text (`CLIMB TO FL360`, `WILCO`).
- **CPDLC argument values** — readers for the common element argument
  types: Level (feet / metres / FL / metric-FL, single + block),
  Time, Position (fix / navaid / airport / lat-lon), Speed (all seven
  IAS/TAS/GS/Mach units), Degrees, Direction, Distance, plus the
  two-component compounds (LevelLevel, TimeLevel, PositionLevel, …) and
  RouteClearanceIndex. **Route clearances** in `constrainedData` decode
  departure/destination airports, runways, procedures (arrival/approach/
  departure + transition), and the RouteInformation leg list (published
  fix/navaid, lat-lon, place-bearing, place-bearing-distance, ATS route
  designator). An unsupported argument type stops the element walk
  explicitly (its size is then unknown) and the element is flagged
  undecoded rather than guessed past — the staged FANS-1/A approach.
- **CM (`parse_cm_logon`, `parse_cm_ground`)** — the dialogue that
  precedes CPDLC. CMLogonRequest yields the flight id (and a count of
  present optional fields); CMGroundMessage identifies the dialogue type
  (logon-response, update, contact-request, forward-request, abort,
  forward-response). Per-entry TSAP application lists are reported
  present-or-absent (variable-size — staged).

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
- **Octet-level ground truth** from dumpvdl2 `--debug burst_detail`
  (post-deinterleave Data+FEC octets) — this, not frame counts, exposed
  the LSB-vs-MSB RS bug (see lessons).
- **Spec self-test vectors** in the suite: UW phase sequence, first 48
  scrambler keystream bits, header-FEC parity for TL ∈ {1,100,1000,
  131071}, AVLC FCS residue, RS encode/decode roundtrip.
- **Synthetic UPER vectors** for the ATN-B1 path, built bit-by-bit from
  the module (WILCO downlink, CLIMB-TO uplink, CM logon-request,
  CM ground logon-response, cleared-route uplink).
- **RF loopback** (`tests/end_to_end.rs`): AOA frame with a real ADS-C
  payload + S-frame, both at 50/100 kS/s, plain and RC(α=0.6)
  pulse-shaped, plus a wideband-with-CFO path; the vendored 6 s off-air
  fixture is guarded by `tests/offair.rs`.

The dictionaries (XID param IDs, X.25 cause/diagnostic codes, IDRP
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
- CPDLC: extension-marked CHOICE alternatives, fragmented PER lengths,
  unsupported argument types, the routeInformationAdditional tail, and
  place-bearing-distance positions are not decoded — each stops its walk
  explicitly rather than emitting a guess.
- CM: TSAP application-list entries reported present/absent only.

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

**Decode protocol, don't guess it.** Across the XID, X.25, IDRP and
CPDLC work the recurring discipline is: parse the reliably-framed fields,
name them from a spec-pinned dictionary cross-checked against the oracle,
and leave anything whose binary layout is unverified as hex (or stop the
PER walk explicitly when a size is unknown). A mis-numbered XID table
(0x42 vs 0x83) shipped before because a name was assumed rather than
pinned; every dictionary now has a test.

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
