# Provenance — xng-mode-sarsat

Decoder for the COSPAS-SARSAT 406 MHz First-Generation Beacon (FGB) message,
per C/S T.001: the 112-bit short / 144-bit long distress-beacon message. This
crate is the **message/frame decoder** (hex/bits → structured fields); the IQ
demodulator and a spec-faithful modulator are deliberately out of scope (see
the TODO at the foot of `src/lib.rs`).

## Sources

Standards / protocol facts:

- **C/S T.001** (COSPAS-SARSAT Specification for First-Generation 406 MHz
  Distress Beacons, freely published): the bit layout of the short/long
  message (format flag bit 25, protocol flag bit 26, country code bits 27-36,
  protocol code, the protocol-specific identification fields, the encoded
  position fields), and the two BCH error-correcting codes — BCH(21,15) PDF-1
  over bits 25-85 with parity in bits 86-106, and BCH(12,7) PDF-2 over bits
  107-132 with parity in bits 133-144.

Reference decoder used as the field-layout + arithmetic + verification oracle:

- **`amsa-code/fgb-decoder`** (Apache-2.0, the Australian Maritime Safety
  Authority's open-source FGB decoder). Used for the exact bit offsets of each
  protocol's identification fields, the two BCH generator polynomials, the
  default-location substitution that produces the position-independent 15-hex
  beacon ID, the coarse/offset position arithmetic, the Return Link Service
  (RLS) TAC/ID layout, and the 5-bit modified-Baudot character table for
  aircraft-operator designators.

No code was copied. The Java source was read to recover the protocol facts
(offsets, polynomials, the order of operations in the position arithmetic);
the Rust implementation is independent.

## Verification (oracle-anchored, not loopback)

The test suite in `tests/oracle.rs` asserts every decode against real entries
from the `amsa-code/fgb-decoder` **compliance kit**
(`src/test/resources/compliance-kit/<HEX>.json`). In that kit the file name is
the input hex and the JSON body is the reference decoder's output. The tests
pin the exact 15-hex beacon ID, country code, protocol type, the
protocol-specific identification (C/S type approval, beacon serial, 24-bit ICAO
aircraft address in hex+octal, aircraft operator designator + serial, RLS TAC
number + id), the coarse and refined positions, and the BCH(21,15) / BCH(12,7)
verification flags — all copied from the reference JSON.

Vectors covered (compliance-kit filenames):

- `8DA41A02C17FDFF83B4235FFFFFFFF` — Standard Location ELT, serial protocol.
- `8E8628D187874181D738F700000000` — Standard Location EPIRB, coarse position
  (southern hemisphere).
- `A3E7B10016150D364D8B3689C09437` — Standard Location PLB, coarse + offset
  position + PDF-2 BCH(12,7).
- `ADA5B61C8C7FDFFBE89AF7FFFFFFFF` — Standard Location Aircraft Operator
  (5-bit Baudot designator + serial).
- `1C66738928FFBFF`, `3EE6F80D1AFFBFF` — 15-hex Aircraft Address (24-bit ICAO,
  hex + octal).
- `1D0E4E9142FFBFF` — 15-hex PLB serial.
- `8E0D0990014710021963C85C7009F5`, `96ED09900149D4D467EE0851A3B2E8` — Return
  Link Service Location, TAC/ID + coarse + fine position + PDF-2 (eastern and
  western hemispheres).
- `4CB31E0C02A82608F011BE00000000` — User Aviation (short).
- `4E86A265C600146DBC407600000000` — User Serial Maritime Float-Free (short).

The BCH(21,15) generator polynomial used (`1001101101100111100011`) and
BCH(12,7) (`1010100111001`) were confirmed by recomputing the parity for these
vectors and matching the parity bits transmitted in each message (and the
reference decoder's reported `errorCorrectingCode1`/`errorCorrectingCode2`).
`bch1_detects_corrupted_parity` additionally corrupts a parity nibble and
asserts the BCH check flags it — this is error *detection*, not an encode→decode
loopback.

## Scope and deliberate omissions

- Modelled in full (oracle-verified): message type / format, country code, the
  protocol classification, the 15-hex / 22-hex beacon ID (with the
  default-location substitution for location protocols), BCH(21,15) and
  BCH(12,7), coarse + offset position (Standard Location), absolute position
  (User Location), RLS coarse + fine position, and the protocol-specific
  identification fields listed above.
- **Not** modelled field-by-field: the full set of 35 sub-protocols in the
  reference decoder (e.g. Ship MMSI digit packing, radio call-sign, every
  national/test variant, the nature-of-distress and emergency-code flags). For
  those families the crate still returns the verified common fields (hex ID,
  country code, protocol type, BCH); detailed sub-fields are left to a
  follow-up rather than shipped unverified.
- **IQ demodulator (IQ → bits)**: implemented (`src/demod.rs` +
  `SarsatChannelDecoder` in `src/lib.rs`). FGB modulation is biphase-L
  (Manchester) phase modulation at ±1.1 rad, 400 bps on the 406 MHz carrier,
  preceded by an unmodulated carrier, a 15-bit bit-sync `1` run and a 9-bit
  frame sync. The channel decoder owns an `xng_dsp::Ddc` (mix by the channel
  offset + decimate the capture IQ down to `CHANNEL_RATE` = 8 kHz), recovers the
  residual carrier with a one-pole complex average (the role the unmodulated
  carrier preamble plays in a real receiver — the ±1.1 rad deviation leaves a
  non-zero mean carrier component to lock to), slices the derotated phase into
  biphase half-symbols with a zero-crossing timing loop, correlates the
  bit-sync + frame-sync preamble, pairs the half-symbols into data bits, and
  hands the assembled hex to the existing oracle-anchored `decode_hex`.
- **Modulator (bits → IQ)**: `src/modulate.rs` exists **only** as a
  self-generated test-signal source for the demod loopback (it is not a
  spec-compliance encoder and is not used by the decode core).
- **Demod validation (self-generated loopback, not an IQ oracle)**: there is no
  public COSPAS-SARSAT IQ reference vector, so the demod is validated
  self-consistently in `tests/demod_synth.rs`: a KNOWN-GOOD beacon hex (one of
  the `amsa-code/fgb-decoder` compliance vectors already pinned in
  `tests/oracle.rs`) is modulated at the biphase-L ±1.1 rad / 400 bps waveform,
  run through the real `SarsatChannelDecoder::process` (including the
  DDC mix+decimate and a carrier offset in one case), and the recovered
  beacon's decoded fields are asserted equal to the oracle-known values. The
  modulate→demod path is therefore self-generated; the **decode core remains
  oracle-anchored** by `tests/oracle.rs`. The tests are suffixed `_synth_iq`.
- **Second-generation beacons (C/S T.018, SGB)**: not decoded (no public oracle
  vectors were used).

## Workspace integration

The crate exports the channelized decode interface the `xng` binary wires
uniformly across modes: `CHANNEL_RATE`, `CHANNEL_PASSBAND_HZ`,
`SarsatChannelDecoder::{new, process, level_dbfs}`, and `to_message`, which
emits `xng_types::MessageBody::Sarsat { kind, details }` with `mode =
Mode::Sarsat`. The `Mode::Sarsat` / `MessageBody::Sarsat` variants live in
`xng-types`. The final bin/runtime/CLI hookup (instantiating the channel
decoder from the capture plan) is a separate integration step.
