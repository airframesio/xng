# Provenance — xng-mode-acars

Clean-room implementation. Sources used (protocol facts and standards text
only; no code from any decoder was read or ported):

- ARINC Specification 618 (Air/Ground Character-Oriented Protocol):
  modulation (§4.4), preamble (§4.2–4.3), frame layout (§2.1–2.3),
  BCS/CRC definition and worked example (§2.2.10–2.2.11).
- ICAO Annex 10 (ISO-5 character set conventions).
- reveng CRC catalogue (CRC-16/KERMIT parameters).
- WAVECOM ACARS decoder reference (display conventions, e.g. label `_d`).
- Textbook DSP (frequency-discriminator FSK demodulation, timing recovery).

Key verified facts encoded here:
- MSK tones are differential: 1200 Hz = bit change, 2400 Hz = no change;
  pre-key is all-ones (continuous 2400 Hz).
- Characters are LSB-first with ODD parity in bit 8; pre-key, BCS, and DEL
  carry no parity.
- BCS is CRC-16/KERMIT over the parity-bearing octets from Mode through
  ETX/ETB inclusive (SOH excluded), low byte transmitted first;
  ARINC 618's "K7" example (0xCB 0x37 → 0x6B3E) verified in xng-dsp tests.

## ACARS-4.2 — Syndrome-table FEC (`src/fec.rs`)

Single-bit error correction by O(1) syndrome lookup, the approach acarsdec
uses in `syndrom.h` (TLeconte, GPL; `acars.c` `fixprerr`/`fixdberr`).

- **Parity polynomial:** CRC-16/KERMIT (poly 0x1021 reflected = 0x8408,
  init 0) — the ARINC 618 BCS, reused from `xng_dsp::checksum::acars_crc`.
- **Table semantics (oracle-grounded, NOT a self-loopback):** because CRC is
  linear over GF(2), the residue of a received block equals the CRC of the
  error pattern alone. acarsdec tabulates `syndrom[8*d + b]` = the residue of
  a lone 1-bit at byte-distance `d` from the buffer end, bit `b`. We build the
  identical map by running the *same* CRC over one-hot buffers, and assert
  canonical entries of acarsdec's published `syndrom.h` verbatim
  (`syndrom[0]=0x1189`, `[1]=0x2312`, `[7]=0x8408`, `[8]=0x19d8`,
  `[15]=0x8ccc`, `[16]=0x5adc`, `[23]=0x0cec`, `[1935]=0x721c`). The full
  1936-entry table was validated against `syndrom.h` offline.
- **Correction:** compute `acars_crc(block)`; a non-zero residue that matches a
  table entry localizes the flipped bit; XOR it and the block returns to CRC
  residue 0. Verified by flipping a known bit in the ARINC 618 §2.2.10 "K7"
  block (0xCB 0x37 0x3E 0x6B, residue 0) and confirming exact recovery —
  grounded on the published parity polynomial, not an encode/decode loopback.
- Source: acarsdec `syndrom.h` + `acars.c`
  (https://github.com/TLeconte/acarsdec).

## ACARS-3.2 — Generic sublabel / MFI extraction (`src/sublabel.rs`)

Extends the H1 sublabel/MFI grammar to other sublabel-bearing labels (H2)
without modifying the shared `xng-acars` crate (which only handles H1).

- **Grammar oracle:** libacars 2.2.1 `acars.c`
  `la_acars_extract_sublabel_and_mfi` — downlink `#xxB`, uplink `- #xx`,
  optional MFI `/yy ` (space-terminated). Our port mirrors libacars's exact
  index arithmetic; libacars gates the grammar on `label == "H1"`, we apply
  the same grammar to the wider `#`-sublabel family.
- **Label set (spec-derived extension):** ARINC 620-4 Appendix C maps
  label+sublabel → SMI for the supervisory/SMT families; H2 is the documented
  structural twin of H1. We only add a sublabel when the text actually
  presents the sentinel, and never shadow H1 (owned upstream by `xng-acars`).
- Sources: libacars `acars.c` + `doc/PROG_GUIDE.md`
  (https://github.com/szpajder/libacars); ARINC Specification 620-4 App C.
