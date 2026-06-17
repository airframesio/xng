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

## Distress device classification (2026-06)

The `distress` tag classifies SART/MOB/EPIRB-AIS transmitters by MMSI
prefix per the ITU-R M.1371 / MID allocation for device identities:
970 = AIS-SART, 972 = AIS-MOB, 974 = EPIRB-AIS (standards facts only). The
devices emit ordinary AIS messages; the prefix marks the distress class.
