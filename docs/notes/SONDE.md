# Radiosondes — implementation notes

Native Vaisala **RS41** radiosonde frame decoder (`crates/xng-mode-sonde`).
RS41-SG / RS41-SGP is the most widely flown operational radiosonde
worldwide. This crate takes an RS41 *frame* (de-whitened bytes) and runs the
full byte-domain chain — data de-whitening → interleaved Reed-Solomon
RS(255,231) FEC → `ID | LEN | DATA | CRC16` sub-block parse — emitting
structured STATUS / GPS / PTU fields. Clean-room: every protocol fact
(whitening mask, CRC variant, RS field/interleave, sub-block offsets, the
ECEF→geodetic formula) is sourced from **rs1729/RS** (the de-facto open RS41
reference) and verified in tests against that project's *published worked
example* — two real sample frames with the per-sub-block CRC breakdown and
an RS decoder input/output for a frame with two correctable byte errors.
No code was copied; the algorithms are re-implemented and checked against
the reference's *outputs*.

**Status: DECODE-CORE only.** This is a byte-frame decoder, not a live
mode. There is **no `--mode` wiring**, no `xng_types::Message` mapping, no
DSP front-end: the crate is a workspace member (compiled via the `crates/*`
glob) but is **not** a dependency of `xng-cli` / the app, and the GFSK
IQ→bits demodulator is a documented TODO (see Limitations). It decodes from
a post-FEC (or pre-FEC, RS-correctable) frame buffer of the form the
rs1729/RS sample frames take. Source: `crates/xng-mode-sonde/src/`.

## Pipeline

de-whitened on-air bytes → `whitening::dewhiten_frame` (XOR 64-byte mask,
header un-whitened) → `rs::Rs41Rs::correct_frame` (two interleaved
RS(255,231) codewords, ≤12 byte errors each) → `frame::decode_frame`
(sub-block CRC gate → STATUS / GPS-INFO / GPS-POS / PTU fields). Two entry
points in `lib.rs`:

| Entry | Input | Does |
|---|---|---|
| `decode_on_air(&[u8])` | whitened stream (`10 B6 CA 11 …`) | de-whiten, then ↓ |
| `decode_dewhitened(&[u8])` | de-whitened stream (`86 35 F4 40 …`) | RS-correct (on a copy), then `decode_frame` |

Both return `Decoded { rs: RsResult, frame: Rs41Frame }`. Frame length is
320 (standard) or up to 518 (extended / aux-xdata); a frame below 320 bytes
is rejected (`DecodeError::TooShort`).

## Data whitening (`whitening.rs`)

Vaisala scrambles every byte **after** the 8-byte header against a fixed
64-byte XOR mask: `frame[pos] = xframe[pos] ^ MASK[pos % 64]`. The mask is
the published `mask[]` from rs1729/RS `rs41mod.c` (also derivable from the
data-whitening notes in `rs41.txt`). `xor_mask(buf, start)` applies the mask
at absolute phase `(start + i) % 64` and is its own inverse (whitens =
de-whitens); `dewhiten_frame` passes the first 8 bytes through unchanged and
de-whitens the rest at their natural phase. The de-whitened header is the
RS41 sync constant `86 35 F4 40 93 DF 1A 60`.

## Reed-Solomon FEC (`rs.rs`, `gf256.rs`)

Two **interleaved RS(255,231)** codewords over GF(2^8), 24 parity bytes
each, correcting up to 12 byte errors per codeword. Systematic **parity
first**: `c[0..24]` parity, `c[24..255]` message; `c[n]` is the coefficient
of `x^n`, so the 24 roots are `alpha^0 .. alpha^23` (`b = 0`).

- **Field** (`gf256.rs`): GF(2)[x] / 0x11D (x⁸+x⁴+x³+x²+1), primitive
  element `alpha = 0x02`; antilog/log tables (`exp[512]`, `log[256]`) built
  once. Matches `bch_ecc_mod.c::GF256RS = { f: 0x11D, alpha: 0x02 }`.
- **Frame interleave** (`rs.rs::gather`/`scatter`): parity byte `8 + i` →
  cw1[i], `8 + 24 + i` → cw2[i]; message byte `56 + 2·i` → cw1 message,
  `56 + 2·i + 1` → cw2 message. Short (320-byte) frames are **zero-padded**
  out to the full 462 message bytes (2×231) before decoding, exactly as the
  reference does; corrected bytes are scattered back only to positions
  inside the actual frame.
- **Decoder**: syndromes → Berlekamp-Massey (error-locator) → Chien search
  (roots → error positions) → Forney (magnitudes), all in `decode_codeword`.
  Returns `Some(0)` when syndromes are already zero, `Some(n)` after
  correcting `n` errors, `None` if uncorrectable. A degree/roots mismatch or
  a post-correction syndrome recheck failure both yield `None` — corrections
  are **verified** to produce a valid codeword before being accepted.
- `RsResult { errors1, errors2 }` with `ok()` (both decoded) and
  `total_corrected()` helpers.

## CRC-16 (`crc.rs`)

Each variable-length sub-block (`ID | LEN | DATA[LEN] | CRC16`) carries a
**CRC-16/CCITT-FALSE** (poly 0x1021, init 0xFFFF, no reflect, no xorout),
matching `rs41mod.c::crc16()`. The stored CRC is little-endian. A sub-block
is accepted only when `crc16(body) == stored`.

## Frame / sub-block decode (`frame.rs`)

The de-whitened, post-FEC frame is the 8-byte sync header followed by a
positionally-fixed chain of `ID | LEN | DATA | CRC16` sub-blocks. Offsets
and packet IDs are the `pos_*` / `pck_*` defines from `rs41mod.c`; all
multi-byte integers are little-endian. **`decode_frame` requires the STATUS
sub-block CRC to pass** (else `DecodeError::StatusCrcFailed`); each other
sub-block is decoded only when its own CRC checks, leaving the field `None`
rather than emitting garbage.

| Sub-block | Pos | Pkt ID | Fields decoded |
|---|---|---|---|
| **STATUS** | 0x039 | 0x79 | frame number (u16 @0x03B); sonde serial (8 ASCII @0x03D, NUL-trimmed); battery (byte @0x045 ÷ 10 → volts); per-frame calibration sub-frame (counter @0x052 + 16 config bytes) |
| **PTU** | 0x065 | 0x7A | 12 raw 24-bit channels (@0x067, LE); signed-16 `p_aux` (@0x08D); cal index + cal bytes (shared with STATUS region @0x052) |
| **GPS-INFO** (RXM-RAW) | 0x093 | 0x7C | GPS full week (u16 @0x095); time-of-week ms (u32 @0x097) |
| **GPS-POS** (NAV-SOL) | 0x112 | 0x7B | ECEF position + velocity → lat/lon/alt + speed/course/climb; #SVs (@0x126) |

- **STATUS** → `serial`, `frame_num`, `battery_v` on `Rs41Frame` (always
  present once the frame is accepted).
- **GPS-INFO** → `Option<GpsTime { week, tow_ms }>`.
- **GPS-POS** → `Option<GpsPos>`. ECEF X/Y/Z are 3×i32 centimetres (@0x114,
  +4, +8) → metres → `ecef_to_geodetic` (WGS-84 Bowring closed-form,
  reproducing `ecef2elli()`). ECEF velocity is 3×i16 cm/s (@0x120) rotated
  into local North/East/Up → `speed_ms`, `course_deg` (0 = N, CW), `climb_ms`
  (up positive). `num_sv` from @0x126.
- **PTU** → `Option<Ptu { raw[12], p_aux, cal_index, cal_bytes[16] }>`. The
  12 channels are grouped [0..3] main-temperature ratio, [3..6] humidity,
  [6..9] humidity-sensor temperature, [9..12] pressure (sensor-dependent;
  zero on RS41-SG without the pressure transducer).
- `CrcStatus { status, ptu, gps_info, gps_pos }` is surfaced for
  diagnostics. `DecodeError` is `TooShort(n)`, `BadHeader`,
  `StatusCrcFailed`.

Note the standard frame also carries a **GPS2 sub-block (pkt 0x7D @0x0B5)**
between GPS-INFO and GPS-POS — its CRC is asserted in the FEC oracle test
but the crate does not yet break out its fields.

## Calibrated PTU — documented scope boundary

The RS41 transmits its temperature/humidity/pressure **calibration table**
spread across 51 sub-frames (calibration counter 0x00..0x32), one 16-byte
sub-frame per radio frame. Producing calibrated °C / %RH / hPa therefore
requires reassembling ~51 consecutive frames before the coefficients are
complete — impossible from a single frame. This crate emits the **raw
24-bit PTU channels** plus the **one calibration sub-frame** carried in each
frame; calibrated-value reconstruction (the `get_T` / `get_RH` / `get_P`
polynomials in `rs41mod.c`, which consume the assembled table) is a
documented follow-up, **not silently faked**.

## Validation / oracles

**rs1729/RS is the oracle**, via its published worked example in
`rs41/rs41.txt` — fetched with `gh api repos/rs1729/RS/contents/<path>`.
These are external published vectors, never an encode→decode loopback.

- **FEC layer** (`tests/fec_oracle.rs`): the two `rs41.txt` sample frames
  are pasted as hex.
  - `clean_frame_decodes_with_zero_errors` — the 320-byte K1930293 frame
    decodes with cw1 = 0, cw2 = 0 errors (matching rs1729) and is unchanged
    by correction.
  - `corrects_two_errors_to_oracle_frame` — the 518-byte K4020244 frame
    (received with two byte errors) corrects **exactly 2** errors in
    codeword 2 (rs1729: `errors: 2, pos: 234 252`) and the corrected frame
    equals rs1729's RS-decoder output **byte for byte**.
  - `subblock_crcs_pass_on_oracle_frame` — every sub-block (STATUS, PTU,
    GPS-INFO, GPS2, GPS-POS) CRC-checks, matching rs41.txt's CRC listing.
- **Field decode** (`tests/frame_decode.rs`): both sample frames decode to
  the oracle field values — serial `K1930293` / `K4020244`, frame numbers
  5910 / 5014, battery 2.6 / 2.8 V, GPS week 1800 / 1869 and TOW, ECEF →
  lat/lon/alt (Zagreb 46.05/16.11/28410 m, 8 SVs; 52.44/0.46/10022 m,
  9 SVs), and the 12 raw PTU channels + cal index. The errored extended
  frame is decoded **after** RS correction. Negative tests reject a
  short frame and an uncorrectable (STATUS region overwritten) frame rather
  than emitting a fabricated serial.
- **Unit anchors against the reference**: the whitening mask is anchored by
  de-whitening the published on-air header `10 B6 CA 11 22 96 12 F8` →
  `86 35 F4 40 93 DF 1A 60` (real on-air bytes, not self-consistency); CRC
  against the standard `crc16("123456789") == 0x29B1`; the RS generator is
  rebuilt from the field and matched against the degree-24 polynomial
  printed in rs41.txt (`1 7a 76 a9 … d9 90 75`); GF(2^8) log/exp roundtrip
  and inverse.

No count-style head-to-head benchmark — the decoder is pinned to rs1729's
published vectors (field-exact on two real frames, byte-exact RS output).

## Known limitations / intentional gaps

- **No GFSK demodulator.** The RS41 air interface is GFSK at 4800 baud, one
  frame per second. The IQ→bits demod (and bit→byte framing / sync search)
  is **not** implemented; the crate decodes from a frame buffer. Documented
  TODO and a deliberate non-goal of the verified decode layer.
- **No `--mode` wiring / no live integration.** The crate has no
  `xng_types::Message` mapping, is not a dependency of the CLI / app, and is
  not in `docs/REFERENCES.md`. It is a standalone decode core only.
- **Calibrated PTU deferred** — raw 24-bit channels + per-frame calibration
  sub-frame only; physical °C/%RH/hPa needs the 51-sub-frame table assembled
  across frames (see above).
- **RS41 only.** Other operational sonde types — **RS92, DFM (Graw),
  M10/M20 (Meteomodem), iMet, MRZ, …** — are not implemented. Each has its
  own modulation, framing and FEC; a follow-up.
- **GPS2 sub-block (0x7D) not decoded** — CRC-verified in tests, fields not
  yet broken out.
- **Aux / xdata payload** in extended (518-byte) frames is not parsed beyond
  the standard sub-blocks.

## Gotchas

1. Header is **un-whitened**; the mask phase is absolute (`pos % 64`), so
   de-whitening a body slice must pass `start = 8`.
2. RS layout is **parity-first** (`b = 0`, roots `alpha^0..alpha^23`), and
   short 320-byte frames must be **zero-padded to 462 message bytes** before
   RS decode — both matching the reference.
3. STATUS CRC is a **hard gate**: no STATUS CRC, no frame (avoids fabricated
   serials).
4. ECEF position is i32 **centimetres**, velocity i16 **cm/s** — both
   divided by 100 before use.
5. PTU pressure channels [9..12] are zero on the non-pressure RS41-SG.

## Key references

- **rs1729/RS** (Zilog80) — the de-facto open RS41 reference; protocol facts
  and published vectors (facts only, no code copied):
  `rs41/rs41.txt` (worked example, sample frames, CRC + RS breakdown),
  `demod/mod/rs41mod.c` + `rs41/rs41.c` (offsets, packet IDs, whitening
  `mask[]`, `crc16()`, `ecef2elli()`), `demod/mod/bch_ecc_mod.c`
  (`GF256RS = { f: 0x11D, alpha: 0x02 }`).
- Vaisala RS41-SG / RS41-SGP radiosonde.
- `crates/xng-mode-sonde/PROVENANCE.md` — sourcing policy and per-component
  oracle notes.
