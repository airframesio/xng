# Provenance — xng-mode-vdl2

Clean-room implementation. Sources used (protocol facts and standards text
only; **no code or text from dumpvdl2/vdlm2dec (GPL) was used**):

- ICAO Annex 10 Volume III Part I, Chapter 6 (§6.4.2–6.4.3, Tables
  6-1..6-4, Figures 6-1/6-2): D8PSK Gray mapping, burst structure (5-symbol
  ramp, 16-symbol unique word, reserved symbol, 17-bit transmission length,
  (25,20) header FEC with its H matrix), scrambler (x^15+x+1, initial state
  1101001010110 01, additive, starts after the unique word), RS(255,249)
  over GF(2^8) with p(x)=x^8+x^7+x^2+x+1 and generator roots α^120..α^125,
  shortening rules (≤2 octets: no FEC; 3–30: 2 of 6 checks; 31–67: 4;
  ≥68: 6), and the c×255 column-interleaver.
- ETSI EN 301 841-1 V1.4.1 and EN 301 841-2 V1.2.1 (freely published):
  AVLC frame structure per ISO/IEC 13239 (flags, bit stuffing, 16-bit FCS),
  4+4 octet address fields (27-bit DLS addresses, A/G and C/R bits, address
  type codes), control field repertoire.
- ISO/IEC TR 9577 conventions and public ecosystem documentation (Wiley
  air-ground data link text excerpt, GE patent US2016/0134682A1): AVLC
  information fields beginning 0xFF carry ACARS (AOA), followed by the
  classic SOH-prefixed ACARS block; 0x81/0x82/0x83 mark CLNP/ES-IS/IDRP.
- Textbook DSP and coding theory (D8PSK demodulation, Berlekamp-Massey
  errors-and-erasures decoding).

Spec-derived self-test vectors encoded in the test suite: unique-word
phase sequence, first 48 scrambler keystream bits, header-FEC parity for
TL ∈ {1, 100, 1000, 131071}, AVLC FCS residue.

Items flagged for live-capture verification (free spec ambiguity):
which 2/4 of the 6 RS check octets are transmitted for short rows
(assumed: first by transmission order). The AVLC FCS octet order is now
pinned to little-endian (low octet transmitted first) per ISO/IEC 13239
§4.4 — confirmed against the off-air fixture, which decodes only under
little-endian, and consistent with dumpvdl2's GOOD_FCS=0xF0B8 residue
check. The earlier "accept either order" behaviour was dropped to remove
a false-accept path (the byte-swapped FCS matched ~1 bad frame in 65536).

## Off-air validation (2026-06)

Validated against the sigidwiki VDL-M2 IQ recording (CC BY-SA, 46.9 s,
Amsterdam area; the capture's I/Q convention is inverted — dumpvdl2
2.6.0 also decodes nothing until the spectrum is conjugated). With
dumpvdl2 as ground truth (41 frames), two real-signal fixes:

1. **Quarter-sample UW timing refinement** (the broad differential-peak
   bias seen on HFDL): a 1-2 sample error at 4.76 samples/symbol
   degrades every later symbol decision, failing the header FEC or RS.
2. **Consistency gate relaxed 0.25 → 0.01·mean**: symbol-spaced
   interpolations legitimately dip on phase transitions, and the strict
   gate rejected most real preambles (2 frames decoded vs 11 after).
   The remaining weak gate still kills burst-edge false locks, and a
   false lock that passes the header FEC with a bogus length no longer
   swallows the real burst (re-hunt resumes at the false UW start).

Result: 11 frames decoded including CRC-valid ACARS from HB-IJW
(label B9, /EHAM.TI2/...) and TC-JRA, plus AVLC supervisory traffic —
all also present in dumpvdl2's output. dumpvdl2 decodes 41 frames from
the same file; the gap is acquisition sensitivity (proper symbol-timing
recovery is the planned follow-up). A 6 s slice is vendored as a CI
fixture (tests/data/, attributed) guarded by tests/offair.rs.

## Sensitivity investigation round 2 (2026-06)

Fixed a decode livelock: a burst whose RS decode failed was re-hunted
from one sample past its UW, deterministically re-refined to the same
position, and retried — ~1700 times per burst, escaping only when the
(symmetric) noise-floor EMA rose above the burst. The retry storms also
consumed the timeline so later bursts were never hunted. The demod now
remembers the last RS-failed UW position and skips past it on
re-detection. Result: every burst in the sigidwiki capture that passes
the header now also passes RS (14 bursts, 0 RS row failures), and the
wasted work is gone.

Remaining gap to dumpvdl2 on the same capture: four ground-station XID
bursts decode RS-"clean" but fail the AVLC FCS — running dumpvdl2's
exact destuffing algorithm over our post-RS bits fails identically, so
the corruption is upstream: our symbol decisions carry enough errors
that RS (at capacity, fixed=3 on k=6 rows) miscorrects into a nearby
codeword. Phase-gain and sampling-offset sweeps are already at their
optima; closing this needs a matched filter + symbol-timing tracking in
the demod (planned).

## Coherent preamble sync (2026-06, demod v2 step 1)

The UW hunt's quarter-sample differential refinement is replaced by a
coherent joint fit (dumpvdl2's sync in least-squares form): over a fine
timing grid, the unwrapped per-symbol phase trajectory of all 16 UW
symbols is compared against the known cumulative UW phase ramp and fit
to residual ≈ a + b·k, weighted by sample energy. The minimum-cost grid
point yields timing and the per-symbol CFO (b) jointly — far less noisy
than the differential correlation argument, which uses 15 transitions
non-coherently. Off-air result: 13 frames (from 10), including the
ground-station XID bursts whose symbol errors previously drove RS into
miscorrection. Remaining gap to dumpvdl2 is hunt trigger sensitivity on
the weakest bursts.

## AVLC structured bodies + XID parameters (2026-06)

Non-ACARS frames emit structured bodies (addresses, control, payload
class) — all from the existing EN 301 841-2 clean-room parse. The XID
parameter walk (FI octet, GI/GL groups, PI/PL/PV) follows ISO/IEC 8885
as cited by EN 301 841-2; the VDL private parameter set names
(connection-management, destination-airport, autotune-frequency, ...)
are from ICAO Doc 9776 (public manual). Values are rendered as hex plus
printable text where applicable; binary parameter layouts we have not
verified against the spec are deliberately left as hex rather than
guessed. ATN payloads are labeled by IPI per ISO TR 9577 (0x81 CLNP,
0x82 ES-IS, 0x83 IDRP).

## ATN transport (2026-06)

X.25 packet layer (ISO/IEC 8208: GFI/LCN, data with M-bit, call/clear
with causes, supervisory), CLNP full header (ISO/IEC 8473: NLPID 0x81,
type, NSAP addresses), and COTP TPDU identification (ISO/IEC 8073)
implemented clean-room from the public ISO framings as profiled by
ICAO Doc 9776/9705 — dumpvdl2 (GPL) was not consulted for this module.
X.25 M-bit sequences reassemble per logical channel before network-
layer parsing. ATN's LREF/deflate-compressed CLNP variants are labeled
but deliberately left as hex (layouts not yet verified against the
spec). XID ground-station list parameters decode as AVLC addresses via
the standard EN 301 841-2 address parser (see the XID completion note
below).

## XID parameter completion (2026-06, VDL2-3)

The VDL-private parameter-set table (group 0xF0) was completed and
corrected. The earlier table mis-numbered every entry in the 0x40–0x49
range (e.g. it labelled 0x42 "destination-airport" — that is parameter
0x83; 0x42 is Timer T4). The corrected numbering and the added IDs
(autotune-frequency 0x40, replacement-GS 0x41, T4 0x42, MAC-persistence
0x43, counter-M1 0x44, TM2 0x45, TG5 0x46, T3min 0x47, GS-address-filter
0x48, broadcast-connection 0x49, modulation-support 0x81, alternate-GS
0x82, destination-airport 0x83, aircraft-location 0x84, frequency-
support-list 0xC0, airport-coverage 0xC1, nearest-airport-id 0xC3,
ATN-router-NETs 0xC4, system-mask 0xC5, TG3 0xC6, TG4 0xC7, GS-location
0xC8) plus the public ISO 8885 HDLC parameter set (group 0x80: 0x01–0x0B)
were cross-checked against the *parameter-ID dictionary* in dumpvdl2's
`xid.c` (`xid_vdl_params` / `xid_pub_params`) — protocol facts (the
integer→name assignment and the frequency encoding), not code or
formatter text. The 2-octet VDL2 frequency field (autotune 0x40 and each
frequency-support-list entry) decodes to MHz via the SARPs encoding
`freq_khz = (raw12 + 10000)·10`, rounded up to the next 25 kHz step, with
the modulation-support nibble in the top 4 bits; timer/counter parameters
also decode to a big-endian integer alongside the preserved raw hex.
Address-list parameters (replacement-GS 0x41, GS-address-filter 0x48,
alternate-GS 0x82, system-mask 0xC5) decode as 4-octet AVLC addresses.

## IDRP + ES-IS completion (2026-06, VDL2-6)

The IDRP (ISO/IEC 10747) decoder gained the sixth BISPDU type RIB-REFRESH
(type 6), the OPEN PDU body's reliably-framed leading fields (version,
hold-time, max-PDU-size, source RDI — the variable RIB-Atts-Set /
Confed-IDs / auth-mech tail stays in raw hex), the credit-offered/avail
header octets, and named ERROR code + subcode text. The ES-IS (ISO/IEC
9542) decoder now parses the trailing option TLVs on ESH/ISH PDUs:
Mobile-Subnetwork-Capabilities (0x81), ATN-Data-Link-Capabilities (0x88),
Priority (0xCF), and Security (0xC5). The BISPDU-type number (6), the
error-code/subcode dictionaries, and the ES-IS option-type IDs/names were
cross-checked against dumpvdl2's `idrp.c`/`idrp.h` and `esis.c` — protocol
facts (the integer→name assignments) only, not code or formatter text.

## X.25 completion (2026-06, VDL2-4)

The X.25 (ISO/IEC 8208) packet decoder gained RESTART-REQUEST (0xFB,
carrying cause + diagnostic) and RESTART-CONFIRM (0xFF) — previously
dropped — and now resolves the clearing/reset/restart cause and the
diagnostic code to text. Three separate cause tables (clear/reset/restart,
ITU-T X.25 Table 5-7) and one ~150-entry diagnostic table (X.25 Annex E +
ISO 8208 + ICAO Doc 9705 Table 5.7-3 / Doc 9880 extensions) are applied;
RESET-REQUEST now captures its cause + diagnostic too (it previously
carried neither). The X.25 Table 5-7 rule that a cause octet with bit 8
set carries the remote DTE's lower bits is honoured by normalising the
lookup key to 0. The packet-type constants (RESTART 0xFB/0xFF, DIAG 0xF1)
and the cause/diagnostic dictionaries were cross-checked against
dumpvdl2's `x25.c`/`x25.h` — protocol facts only, not code or formatter
text. Facility codes remain numeric (facility naming was out of this
task's scope).

## ATN-B1 CPDLC + CM (2026-06)

Protected-mode CPDLC (ProtectedAircraftPDUs/ProtectedGroundPDUs,
ATCUplink/DownlinkMessage header, the full 238/114 element tables with
standard phraseology) and CM logon-request decode implemented from the
ICAO Doc 9880/9705 ASN.1 modules, vendored as spec text in docs/asn1/
(obtained via Wireshark's transcription of the ICAO standard — module
text only; neither Wireshark's nor dumpvdl2's generated/dissector code
was consulted). Unaligned PER (X.691) hand-walked as for FANS-1/A.
v1 decodes element identity + phraseology; argument value rendering is
the planned follow-up. Validated with synthetic UPER vectors built
bit-by-bit from the module (WILCO downlink, CLIMB-TO uplink, CM logon).

## ATN-B1 CPDLC argument values (2026-06)

Argument readers for the common element types (Level with feet/meters/
FL/metric-FL variants, Time, Position incl. fix/navaid/airport/lat-lon,
Speed in all seven units, Degrees, Direction, and their two-component
compounds) implemented from the same vendored Doc 9880 module; decoded
values render into the module's phraseology templates ("CLIMB TO
FL360"). Elements whose argument type is not yet supported stop the
walk explicitly (sizes unknown), matching the staged FANS approach.

## COTP TPDU completion (2026-06, VDL2-2.2 partial)

The COTP (ISO/IEC 8073 / ITU-T X.224) decoder was extended from 5 TPDU
types to all 10: it now decodes DC, ED, AK, EA and RJ in addition to the
existing CR/CC/DR/DT/ER. Each TPDU's full fixed header is parsed
(destination/source references, CR/CC protocol class + options, DR
disconnect reason, ER reject cause, DT/ED end-of-TPDU flag, and the TPDU
sequence numbers and flow-control credit for the data-flow TPDUs), in
both the normal (7-bit sequence) and extended (31-bit sequence) formats —
the extended format being signalled by an odd length-indicator per X.224.
The variable part is parsed as `type|length|value` parameters including
the **ATN checksum (0x08)** profiled by ICAO Doc 9705, the **TPDU-size
(0xC0, decoded to bytes as 2^value)**, priority (0x87), inactivity timer
(0xF2) and the rest of the X.224 parameter set; the DR disconnect-reason
and ER reject-cause dictionaries are applied to text. The TPDU code
values (CR 0xE0 … ER 0x70), the header octet layouts and variable-part
offsets, the parameter-code/name table, and the reason/cause dictionaries
were cross-checked against the ISO/IEC 8073 framing as profiled by ICAO
Doc 9705 and against dumpvdl2's `src/cotp.{c,h}` — protocol facts (the
integer→name/layout assignments) only, not code or formatter text. Tests
pin spec-derived TPDU vectors built octet-by-octet from the X.224 layout
(no encode→decode loopback). Multipart COTP reassembly and native ATN-B2
ADS-C over COTP remain the deferred big bet (VDL2-2.3).
