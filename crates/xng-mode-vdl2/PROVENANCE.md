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
(assumed: first by transmission order), and AVLC FCS octet order (both
orders accepted, which one matched is recorded).

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
