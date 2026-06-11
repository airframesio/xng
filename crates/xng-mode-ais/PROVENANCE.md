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

## Field-level decode (2026-06)

Message-type field layouts (1-5, 9, 18/19, 21, 24, 27: positions at
1/600000 deg, SOG tenths of knots, 6-bit ASCII strings, nav-status
table) implemented from ITU-R M.1371-5 as already referenced. Validated
against pyais (MIT) as a decode oracle: the canonical type 1, two-part
type 5, and type 18 sentences decode field-identically (vendored as
unit vectors with the oracle outputs recorded, 2026-06-10).
