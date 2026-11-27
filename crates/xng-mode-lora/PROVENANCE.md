# Provenance — xng-mode-lora

Clean-room implementation. The sources below were read for protocol facts
(field layouts, constants, algorithms as described); no code was copied or
ported from any of them.

## LoRa physical layer

- Semtech LoRa is a proprietary chirp spread spectrum PHY. The coding chain
  (inverse Gray mapping, diagonal interleaver, Hamming 4/5..4/8, whitening
  LFSR, explicit header with its 5-bit checksum, CRC-16 0x1021 with the
  final XOR against the last two payload bytes, low data rate optimization
  and the reduced-rate header block) follows the published reverse
  engineering: Tapparel et al., "An Open-Source LoRa Physical Layer
  Prototype on GNU Radio" (EPFL, 2020) and the gr-lora_sdr documentation.
- The whitening sequence was checked against the table published with
  gr-lora_sdr (`tables.h`); the first bytes are a unit test here.
- Sync word to symbol mapping (two symbols, nibble × 8) and the preamble /
  2.25 down-chirp SFD structure come from Semtech's SX127x/SX126x data
  sheets and application notes.
- The demodulator (dechirp + FFT, carrier and timing estimate from the up
  and down chirps, fractional refinement from the phase between identical
  chirps) is textbook CSS receiver design.

## Meshtastic

- Packet header layout, flags, AES-CTR nonce, the default channel key,
  the channel hash, the preset table and the frequency slot hash (djb2 of
  the preset name modulo the channel count) are from the Meshtastic
  documentation (meshtastic.org) and the public protobuf definitions.
- `meshtastic_tables.rs` lists enum value names (hardware models, roles,
  port numbers) taken from the public `.proto` files as data.
- Cross-checks: LongFast's channel hash is 8 and US LongFast sits at
  906.875 MHz, both matching values published by the project.

## MeshCore

- Packet header (route type, payload type, version), transport codes,
  path length encoding, ADVERT layout with its Ed25519 signature and
  app data flags, and GRP_TXT encryption (AES-128 ECB with a 2-byte
  HMAC-SHA256 MAC, channel hash = first byte of SHA-256 of the secret)
  are from the MeshCore documentation and packet format notes.
- The Public channel key is the published default; its hash 0x11 is a
  unit test. Hashtag channel keys use the first 16 bytes of SHA-256 of
  the `#name`, as described by MeshCore clients (not yet checked against
  a real hashtag channel).

## Test signals

All IQ in the tests and `examples/gen_mesh_iq.rs` is synthesized by this
crate's own modulator. No real-RF capture has been checked yet.
