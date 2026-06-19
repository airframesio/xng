# Provenance — xng-mode-uat

UAT (Universal Access Transceiver, 978 MHz, RTCA DO-282B) decode core.

This crate implements the UAT **message/frame decode layer** (bytes/bits →
structured fields) for the 978 MHz ADS-B downlink and the FIS-B uplink. Every
protocol fact below is anchored to an external reference, and every test
asserts against a real reference decoder's output — not an encode→decode
loopback.

## Sources

- **RTCA DO-282B** — UAT MOPS. The authority for the message structure: the
  short (18-byte) and long (34-byte) downlink payloads; the HDR element
  (MDB/payload type bits 1–5, address-qualifier bits 6–8, 24-bit address);
  the State Vector, Mode Status, Auxiliary State Vector, and Target State
  elements; the 432-byte uplink ground message and FIS-B information-frame
  framing; and the Reed-Solomon FEC parameters. The 1-based `(byte, bit)`
  addressing used throughout matches DO-282B's field tables.
- **FAA AC 00-63B / RTCA DO-358** — FIS-B product list and the DLAC 6-bit
  text products (METAR/SPECI, TAF, PIREP, Winds & Temps Aloft, NOTAM, etc.).
- **FlightAware dump978** (`github.com/flightaware/dump978`) — used as the
  bit-layout oracle and the test oracle:
  - `uat_protocol.h` — the FEC parameters: poly `0x187`, first consecutive
    root `α^120`, nroots 12/14/20, pads `255 − {30,48,92}`, and the six
    byte-interleaved RS(92,72) uplink blocks.
  - `fec.cc` — `init_rs_char(8, 0x187, 120, 1, nroots, pad)` and the uplink
    deinterleave (`raw[i*6 + block]`).
  - `uat_message.cc` / `uat_message.h` (`AdsbMessage`) — the maintained
    downlink decoder. The field offsets, scaling, payload-type → element-set
    mapping, base-40 callsign alphabet, and the "emit only present fields"
    JSON shape this crate reproduces.
  - `legacy/uat_decode.c` — the uplink ground-MDB layout
    (`uat_decode_uplink_mdb`), the FIS-B APDU header and time-option parsing
    (`uat_decode_info_frame`), the DLAC alphabet and step machine
    (`decode_dlac`), and the product-id table (`get_fisb_product_name`).
    The legacy decoder is the reference that actually decodes FIS-B contents.

dump978 is BSD-2-clause (modern) / GPL-2 (legacy). No dump978 source was
copied; the layouts were re-expressed in Rust. The legacy `uat2text` and the
modern `uat_message.cc` were *built and run as oracles* to generate the
expected test values — they are not vendored.

## FEC

The codec is `xng_dsp::rs::ReedSolomon` (GF(2^8), Berlekamp–Massey + Forney),
parameterised with UAT's poly `0x187` and first root `α^120` — identical
field/root parameters to dump978's libfec. UAT's three codes are *shortened*
RS codes (RS(30,18), RS(48,34), RS(92,72)); a shortened code is the full
255-symbol code with the leading high-degree symbols held at zero, so encode
feeds only the real data bytes and correct virtual-zero-fills the front to
255. The uplink frame is six RS(92,72) blocks byte-interleaved.

## Test vectors (all externally verified)

- **RS parity** — the 12/14 check octets this crate produces for two real
  sample payloads equal the octets `encode_rs_char` emits for the same
  payloads (libfec built and run on this machine). This pins the FEC against
  the reference implementation, not against itself.
- **Downlink** — two real frames from dump978's published `sample-data.txt`
  (off-air GA traffic near KPAO): a short type-0 frame and a long type-1
  frame (callsign N5130E). The full decoded JSON is asserted equal to the
  output of dump978's `uat_message.cc` `AdsbMessage::ToJson()` for the same
  payloads.
- **Uplink / FIS-B** — a real 432-byte uplink MDB from `sample-data.txt`.
  Site position, slot/site id, the NOTAM (product 8) APDU framing and product
  time, and three product-413 (Generic Textual, DLAC) Winds-Aloft reports
  (RKS/BAM/PRC, 250000Z) are asserted against `legacy/uat2text` output.
- **DLAC** — a `METAR` word and a TAB run-length sequence are asserted equal
  to dump978's `decode_dlac` output for the same bytes.

`sample-data.txt` carries only airborne GA downlinks, so the SV "on ground"
branch (ground speed / track-or-heading / aircraft dimensions / GPS offsets)
and the Target State element are ported faithfully from `uat_message.cc` but
are **not** covered by a pinned vector — no fabricated or loopback test was
added for them.

## IQ demodulator (wideband front-end)

`demod.rs` adds the UAT receive front-end and `lib.rs` exposes the wideband
channel decoder (`UatChannelDecoder`, mirroring the ADS-B interface:
`new(input_rate)` with offset 0, `process(&[Complex<f32>]) -> Vec<UatFrame>`,
`level_dbfs()`, and `to_message`). The chain:

- **DDC** — `xng_dsp::Ddc` conditions the 978-centered capture (offset 0) to
  `CHANNEL_RATE = 2 × 1.041667 MHz` (~2 samples/bit) with a ±625 kHz passband
  (covers the h≈0.6 CPFSK ±312.5 kHz deviation). An exact-rate capture skips
  the DDC.
- **Discriminator** — per-sample `arg(x·conj(prev))` with a slow DC tracker
  for carrier offset (the same primitive used by `xng-mode-ais`'s GFSK demod;
  there is no shared FSK primitive, so the pattern is reproduced locally).
- **Sync hunt** — the buffered discriminator stream is searched at sample
  resolution over a half-sample timing grid for the 36-bit sync words
  (downlink `0xEACDDA4E2`, uplink `0x153225B1D`; ≤4 bit errors tolerated).
  The half-sample grid is what makes 2-samples/bit robust to arbitrary burst
  arrival phase.
- **Slice + RS** — at a sync hit the symbol period is known, so message bits
  are integrate-and-dumped at the matched phase, packed MSB-first, and handed
  to the existing `decode_frame` (the RS-FEC decode core is unchanged). A
  downlink burst offers both the long (48 B) block and its short (30 B)
  prefix; the RS gate validates the correct one. Soft-bit uplink deinterleave
  is still a possible refinement, but the hard-decision interleave handled by
  `fec::correct_uplink` already recovers clean uplinks.

### Demod validation — self-generated modulate→demod path

`modulate.rs` CPFSK-modulates a **known** with-parity frame into IQ, and the
`*_synth_iq` tests in `tests/vectors.rs` run that IQ through
`UatChannelDecoder` and assert the recovered decode equals the
dump978-pinned known-good fields (short type-0 and long type-1 N5130E). This
modulate→demod loop is **self-generated** — there is no public UAT IQ oracle
vector — but the **decode core remains oracle-anchored** by the dump978 JSON
vectors above; the synthetic tests validate only the new front-end (CPFSK
discriminator + sync correlation + bit slicing), including a through-DDC run
at 8 MS/s and an additive-noise run. They are clearly suffixed `_synth_iq`.

## Scope

The decode layer here also still takes corrected payload bytes (or raw
with-parity frames via `decode_frame`) directly, independent of how the bits
were recovered. The shared-file integration (bin / runtime / CLI wiring) is a
deliberate separate step; the `xng_types::Mode::Uat` and `MessageBody::Uat`
variants this crate now emits were added in the wiring stage-1 commit.
