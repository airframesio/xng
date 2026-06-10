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
