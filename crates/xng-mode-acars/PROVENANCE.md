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
