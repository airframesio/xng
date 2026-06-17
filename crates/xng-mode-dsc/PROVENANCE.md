# Provenance — xng-mode-dsc

Clean-room implementation of a Digital Selective Calling (DSC) decoder.
Protocol facts come from the published standards; the decode is *verified*
against an external open-source reference decoder's vectors (no code from any
decoder was copied — only its documented test vectors and the field layout it
implements were used as the oracle).

## Standards (protocol facts only)

- **ITU-R M.493** (Digital selective-calling system for use in the maritime
  mobile service) and **ITU-R M.541** (operational procedures): the CCIR 493
  10-bit symbol alphabet, DX/RX time diversity, format specifiers, category,
  telecommands, end-of-sequence, MMSI/address construction, distress
  nature/position/time, and the error-check character (ECC) definition.
- **CCIR 493** 10-unit error-detecting code: each symbol is 7 information
  bits (B1 sent first = least significant) followed by 3 check bits sent
  most-significant first. The check bits carry the count of "B" (binary 0)
  elements among the 7 information bits, so every symbol carries an inherent
  integrity check.
- **MMSI**: built from 5 symbols, two decimal digits per symbol; the 10th
  digit (the trailing half of the 5th symbol) is filler and is dropped, giving
  the 9-digit MMSI.

## External verification oracle

The bit→symbol and symbol→message layers are pinned to the published unit-test
vectors of **TAOSW.DSC_Decoder** (alemassimo/TAOSW.DSC_Decoder, MIT licensed),
a .NET DSC decoder that decodes off-air HF DSC audio. Its test suite captures
real off-air sequences (timestamped 2025-03..04, MF/HF 2187.5 / 8414.5 kHz)
with the human-verified decode written out next to each symbol stream.

- **Symbol level** — `GMDSSDecoderTests.RetriveDataByteTest1..4`: four 10-bit
  symbols with their expected 7-bit values (2, 122, 127, 43). Reproduced
  verbatim in `src/symbol.rs` tests; they also confirm the 3-bit field is the
  zero-bit count (each vector satisfies the embedded check).
- **Message level** — `SymbolsDecoderTests` (distress alert, all-ships,
  individual station, geographic-area, acknowledgements, requests, error
  cases): each test gives the recovered symbol stream and the asserted
  Format / Category / To / From (MMSI) / TC1 / TC2 / Nature / Position / Time /
  Frequency / EOS / ECC / status. Reproduced verbatim in
  `tests/oracle_vectors.rs`.

These are off-air sequences with an externally-known decode — not an
encode→decode loopback. No TAOSW source was copied; only its vectors and the
ITU-R M.493 field layout it implements were used.

## IQ → bits demod (MF/HF) — SYNTHETICALLY validated

The MF/HF FSK front end is now implemented (`src/demod.rs`, `src/lib.rs`):

- **`demod::FskDemod`** — 100 Bd binary FSK, ±85 Hz shift. Per-sample frequency
  discriminator → slow discriminator-DC tracker (residual carrier offset) →
  per-bit integrate-and-dump with zero-crossing timing recovery → hard bit
  decision (upper tone Y = 1, lower tone B = 0). This reuses the
  frequency-discriminator + timing-recovery pattern of the ACARS `MskDemod` and
  AIS `GmskDemod` (no shared FSK primitive exists in `xng-dsp`); no `xng-dsp`
  code was modified.
- **`demod::DscBitSync`** — hunts the M.493 DX phasing character (`125`) to find
  the 10-bit symbol boundary, confirmed by the next DX character being a valid
  symbol; the standard DX/RX geometry (`deinterleave_dx_rx(.., 6, 2)`) then
  applies.
- **`DscChannelDecoder`** — owns an `xng_dsp::Ddc` (mix by `freq_offset_hz`,
  resample to `CHANNEL_RATE` = 8 kHz) and feeds the aligned bit stream to the
  EXISTING `decode_from_bits` (the decode core is untouched).

**Validation is SYNTHETIC** (`tests/demod_synth.rs`): a self-generated
modulate→demod loopback. A KNOWN symbol stream — taken from the external oracle
vectors in `tests/oracle_vectors.rs` (real off-air HF DSC sequences) — is
modulated as 100 Bd ±85 Hz FSK IQ by `src/modulate.rs`, pushed through
`DscChannelDecoder::process` (directly and via the DDC with a carrier offset),
and the recovered `DscMessage` is asserted equal to the known-good decode. The
modulator and demodulator are independent implementations of the same M.493
conventions, so a convention error on either side shows up as a loopback
mismatch. This validates ONLY the IQ→bits demod + phasing/symbol sync; the
symbol→message decode core remains anchored to the external oracle by
`tests/oracle_vectors.rs`. No recorded off-air IQ vector is vendored, so the
modulate→demod path is self-generated rather than pinned to captured RF.

## What is NOT verified here (documented TODO)

- **VHF FFSK variant** (1200 Bd, 1300/2100 Hz over FM): a documented
  follow-up. Only the MF/HF 100 Bd binary-FSK path is implemented and validated.
- **Frequency/channel sub-fields** for MF/HF working-channel (first digit 3),
  10 Hz multiples (4) and VHF automated systems (8) are defined in M.493 but
  are not covered by an external vector here, so they return a
  `--not implemented--` sentinel rather than a guessed value. The MF/HF 100 Hz
  pair (0/1/2) and VHF channel pair (90…) forms are pinned to oracle vectors.
- **Group call (114)** and **automatic service call (123)** formats: the
  reference decoder does not decode their bodies; we surface the format and any
  recoverable self-id MMSI and mark them unsupported rather than fabricate
  fields.
