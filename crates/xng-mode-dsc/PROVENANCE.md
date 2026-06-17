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

## What is NOT verified here (documented TODO)

- **IQ → bits demod** (`src/demod.rs`): the FSK front end (MF/HF 100 Bd ±85 Hz;
  VHF 1200 Bd 1300/2100 Hz, dot/phasing acquisition, bit timing) is a typed
  stub. Verifying a demod needs recorded IQ with an independently known
  decode; until such a vector is wired in, no demod implementation is
  committed.
- **Frequency/channel sub-fields** for MF/HF working-channel (first digit 3),
  10 Hz multiples (4) and VHF automated systems (8) are defined in M.493 but
  are not covered by an external vector here, so they return a
  `--not implemented--` sentinel rather than a guessed value. The MF/HF 100 Hz
  pair (0/1/2) and VHF channel pair (90…) forms are pinned to oracle vectors.
- **Group call (114)** and **automatic service call (123)** formats: the
  reference decoder does not decode their bodies; we surface the format and any
  recoverable self-id MMSI and mark them unsupported rather than fabricate
  fields.
