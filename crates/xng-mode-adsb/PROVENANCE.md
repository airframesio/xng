# Provenance — xng-mode-adsb

Clean-room implementation. Sources used (protocol facts and standards text
only; no code from any decoder was read or ported):

- ICAO Annex 10 Vol IV: Mode S downlink format — 1090 MHz PPM at 1 Mbps
  (each 1 µs bit cell: pulse in the first half = 1, second half = 0),
  8 µs preamble with pulses at 0, 1.0, 3.5, 4.5 µs, 56/112-bit frames
  (DF ≥ 16 → 112), CRC-24 generator 0xFFF409, parity field either clean
  (extended squitter PI with II=0) or overlaid with the interrogator /
  aircraft address.
- "The 1090 MHz Riddle" (junzis, open educational reference): worked
  field layouts for extended squitter (TC 1–4 identification charset,
  TC 9–18 12-bit altitude with Q-bit, N·25 − 1000 ft) and the published
  example frames used as test vectors (8D4840D6... → KLM1023 ident;
  8D40621D... → 38000 ft).
- Textbook DSP (magnitude-domain pulse detection).
