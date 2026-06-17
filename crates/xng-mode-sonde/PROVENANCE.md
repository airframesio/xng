# Provenance — xng-mode-sonde (Vaisala RS41 radiosonde)

This crate decodes the Vaisala RS41 (RS41-SG / RS41-SGP) radiosonde frame —
the most widely flown operational radiosonde worldwide. Every protocol fact
is externally sourced and every assertion in the tests is anchored to a
public reference, never to an encode→decode loopback.

## Reference / oracle

- **rs1729/RS** (the de-facto open RS41 reference), files fetched via
  `gh api repos/rs1729/RS/contents/<path>`:
  - `rs41/rs41.txt` — the protocol notes and, crucially, a complete
    *worked example*: two real sample frames (a 320-byte standard frame for
    sonde `K1930293` and a 518-byte extended frame for sonde `K4020244`),
    the per-sub-block CRC breakdown, and the Reed-Solomon decoder
    input/output for a frame with two correctable byte errors.
  - `demod/mod/rs41mod.c` and `rs41/rs41.c` — the exact `pos_*` sub-block
    offsets, `pck_*` packet IDs, the 64-byte whitening `mask[]`, the
    `crc16()` routine, and the `ecef2elli()` ECEF→geodetic formula.
  - `demod/mod/bch_ecc_mod.c` — the GF(2^8) field used by the RS code
    (`GF256RS = { f: 0x11D, alpha: 0x02 }`).

These are protocol facts and published vectors. No code was copied; the
algorithms were re-implemented and checked against the reference's
*outputs*.

## What is verified, and how

### Data whitening (`whitening.rs`)
The 64-byte XOR mask is the published `mask[]` from `rs41mod.c`. The test
`dewhiten_published_header` de-whitens the published on-air header
`10 B6 CA 11 22 96 12 F8` (rs41.txt) and asserts it equals the RS41 sync
constant `86 35 F4 40 93 DF 1A 60` — anchoring the mask to real on-air
bytes, not self-consistency.

### CRC-16 (`crc.rs`)
CRC-16/CCITT-FALSE (poly 0x1021, init 0xFFFF, no reflect, no xorout), as in
`rs41mod.c::crc16()`. Checked against the standard published check value
`crc16("123456789") == 0x29B1`, and re-validated on every sub-block of the
oracle frame (`subblock_crcs_pass_on_oracle_frame`).

### Reed-Solomon RS(255,231) (`gf256.rs`, `rs.rs`)
Interleaved RS(255,231) over GF(2^8) with reducing polynomial 0x11D and
generator alpha = 0x02; two codewords, 24 parity bytes each, parity-first
systematic layout with roots alpha^0..alpha^23. Decoder is
syndromes → Berlekamp-Massey → Chien → Forney.
- `generator_poly_matches_oracle` rebuilds the degree-24 generator from the
  field and asserts it equals the polynomial printed in rs41.txt
  (`1 7a 76 a9 ... 90 75`).
- `clean_frame_decodes_with_zero_errors` decodes the clean 320-byte oracle
  frame and asserts both codewords report 0 errors (matching rs1729).
- `corrects_two_errors_to_oracle_frame` decodes the 518-byte oracle frame
  that has two byte errors; it asserts exactly 2 errors are corrected in
  codeword 2 (rs1729 reports `errors: 2, pos: 234 252`) and that the
  corrected frame equals rs1729's RS-decoder output **byte for byte**.

### Frame / sub-block decode (`frame.rs`)
Sub-block offsets and packet IDs are the `pos_*` / `pck_*` defines from
`rs41mod.c`. The ECEF→geodetic conversion reproduces `ecef2elli()`. The
test `decodes_oracle_frames` decodes both oracle sample frames and asserts:
- serial (`K1930293`, `K4020244`), frame number, battery voltage;
- GPS week / time-of-week;
- ECEF→lat/lon/alt and the derived ground speed / course / climb;
- the 12 raw PTU channels and this frame's calibration sub-frame index.
The asserted scalar values are computed directly from the published oracle
frame bytes via the documented offsets and formulas (independently
reproduced), so the test pins the decode of a real frame.

## Calibrated PTU — documented scope boundary

The RS41 transmits its temperature/humidity/pressure **calibration table**
spread across 51 sub-frames (calibration counter 0x00..0x32), one 16-byte
sub-frame per radio frame. Converting the raw PTU channels into calibrated
°C / %RH / hPa therefore requires reassembling ~51 consecutive frames'
sub-frames before the coefficients are complete. A single-frame decoder
cannot produce calibrated physical values from one frame, so this crate
emits the **raw 24-bit PTU channels** plus the **calibration sub-frame**
carried in each frame. Calibrated-value reconstruction (the `get_T` /
`get_RH` / `get_P` polynomials in `rs41mod.c`, which consume the assembled
calibration table) is a documented follow-up, not silently faked.

## GFSK demodulator — TODO

The RS41 air interface is GFSK at 4800 baud, one frame per second. The
IQ→bits demodulator (and the bit→byte / Manchester-free framing and sync
search) is **not** implemented here; this crate decodes from a post-FEC (or
pre-FEC, RS-correctable) frame buffer. The demod is a documented TODO and a
deliberate non-goal of the verified decode layer.
