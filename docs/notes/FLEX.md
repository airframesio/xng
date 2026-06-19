# FLEX (Motorola FLEX one-way paging) — implementation notes

Native Motorola **FLEX** / FLEX-NEXT radio-paging decode core for
`xng-mode-flex`. FLEX is a one-way paging air interface: binary (2-level)
**2-FSK** at **1600 bps** (with 4-level 3200/6400 bps variants), structured
into 1.875-second **frames**. Each frame opens with **Sync 1** (BS1 dotting |
A | B | inverted-A, where the fixed middle field B = `0xA6C6AAAA`), a
BCH-protected **Frame Information Word (FIW)**, then **Sync 2**, then 11 blocks
of 8 words (= 88 32-bit words, one "phase" at 1600 bps). Every word is a
**BCH(31,21)** codeword plus an even-parity bit. The first data word is the
**Block Information Word (BIW)** giving the address- and vector-field offsets;
address words carry capcodes; each address word's **Vector Information Word
(VIW)** selects the page type (tone / numeric / alphanumeric / …); message
words carry the body (7-bit alphanumeric or 4-bit numeric). The crate splits
into a spec-anchored DECODE/framing core (BCH, FIW/BIW/VIW layout, capcode,
text tables) and a 2-FSK IQ front end. The DECODE core is verified against
hand-built, spec-cited words; the DEMOD is verified ONLY by a synthetic
modulate→AWGN→demod test — no off-air IQ exists.

Status: **WIRED + REAL-OFF-AIR VALIDATED.** Runtime mode `Mode::Flex`,
`MessageBody::Flex`, `--mode flex` (also accepts `flex-next` / `flexnext`), and
a `FlexChannelDecoder` that owns an `xng_dsp::Ddc`. Wired through the runtime
(opens FLEX with **`baud = 0` = auto rate-detect**), `scan`, and console output.

## Rates & auto-detection (1600 / 3200 / 6400)

FLEX carries Sync 1 + the Frame Information Word at **1600 bps 2-level** always,
but the **data phase** runs at the rate encoded in the Sync-1 **A-code**: 1600
(2-FSK), or **3200 / 6400 bps 4-level** (the rate most real US paging actually
uses). The decoder supports all three:

- `FlexChannelDecoder::new(rate, offset, baud)` — `baud` ∈ {1600, 3200, 6400}
  forces a rate; **`baud = 0` auto-detects** it. Auto runs the candidate-rate
  lanes; each lane self-gates on the Sync-1 A-code (`from_a_code`), so a burst
  only decodes in the lane whose rate its A-code names — no cross-rate false
  decodes. The 4-level data phase uses an off-air two-clock symbol recovery.
- The 1600-only path (`decode_bits`) below is one lane; 4-level uses
  `decode_symbols` (4-level slicer + de-interleave → the same BCH/word/page
  core).

**Real off-air validation:** a 929.6125 MHz US paging capture (6400 bps,
4-level) is decoded by the auto path in `tests/offair.rs` — it detects 6400,
recovers 50+ alphanumeric pages with sane capcodes and printable text (real
hospital/alert paging), where a forced-1600 decode yields zero alpha pages
(garbage). The test skips cleanly when the capture file is absent, so CI stays
green; the synthetic modulate→AWGN→demod tests cover all three rates. (FLEX is
the first of the paging/rail cores with a real-RF check — the others remain
synthetic-only.) The 1600-specific detail below documents that lane.

### Off-air garbage rejection (live RF hardening)

Loopback tests never exercise the failure mode that dominates a real capture:
the demod producing thousands of *plausible-looking but wrong* frames out of
noise and partial bursts. Validating against the live 929 MHz capture surfaced
four classes of junk, each now gated (so the dashboard shows clean pages, not a
flood of fake capcodes):

- **Idle / fill words** (all-ones / repeating `0x….` station fill) are skipped,
  not read as addresses — these were minting `0xFFFFxxxx` capcodes.
- **Address bounds**: long-address wraparound (`aw1 − 0x8000` underflow) and
  out-of-range capcodes are rejected before a page is emitted.
- **Block-structure validation**: the FIW mod-16 checksum, BIW address/vector
  offsets (must point inside the phase), and the VIW message-word window are all
  range-checked; a frame whose offsets don't self-consistently frame the page is
  dropped.
- **BCH-quality gate**: words needing more correction than the code can trust are
  treated as unrecoverable rather than fed downstream.

### Alpha header / signature handling

Real FLEX alphanumeric vectors prepend a **signature/header word** (message-
fragment number, mail-drop / retrieval flags) before the 7-bit text. Emitting it
verbatim produced visible junk leaders (`□Subj`, `:1:34`, `H2.KEN`). The off-air
alpha path (`decode_alpha_offair`) strips the leading signature byte, terminates
at `0x03` (ETX), and applies a garble gate (`alpha_is_garble`) so a fragment that
decodes to mostly non-printable bytes is dropped instead of shown. Live result:
clean pages like `KEN NAG 2 #160888` and full hospital/logistics dispatch text.

`alpha_is_garble` carries four tells, any of which condemns a page (tuned
against the 929 MHz capture + live soak so spaced human text always survives):

1. **junk-symbol fraction** over a threshold (rare symbols `^ > < ? \\ ~` … that
   real pages almost never carry);
2. **structureless**: a long no-space body with *any* junk symbol;
3. **control character** — any non-whitespace control byte (a BCH-false-correct
   that lands on e.g. `0x05`) is never a real alpha page;
4. **run-density**: a spaceless body whose letters/digits shatter into many
   short upper/lower/digit runs (avg run < 3 alnum chars) is machine garble —
   this catches the zero-junk-symbol cases the others miss, e.g. random
   mixed-case (`gMgUDJLa[7FRJc>m81JL92`) and pure-hex `u…v`-wrapped noise
   (`uC000F7038F08015D5C64v`). A genuine no-space token (phone, long ID, URL
   host, hex serial) runs in long same-class spans and survives; spaced text is
   exempt from tells 2 and 4 entirely.

## Pipeline

```
wideband capture IQ
  → Ddc                     mix by freq_offset_hz, decimate to CHANNEL_RATE (64 kS/s)
  → demod::FskDemod         freq discriminator + DC tracker + 1600 Bd timing → 1 bit/symbol
recovered NRZ bit history (any polarity)
  → demod::find_sync        locate Sync 1 marker 0xA6C6AAAA (≤3 errs), resolve polarity
  → step past marker(32) + C field(16) → FIW
  → demod::word_at_lsb      read 32 bits LSB-first per FLEX word
  → bch::correct            BCH(31,21) syndrome correction + even parity per word
  → frame::parse_fiw        cycle / frame number + mod-16 checksum
  → frame::parse_biw        address-field + vector-field offsets (phase word 0)
  → frame::decode_short_address   capcode = aw1 − 0x8000 + long-address flag
  → PageType::from_viw      vector type (tone/numeric/alpha/…)
  → frame::decode_alpha / frame::decode_numeric   message words → text
  → FlexFrame               (capcode, long_address, cycle, frame, baud, kind,
                             page_type, text, fec_corrected, raw)
  → to_message              → xng_types::Message bus form
```

Two entry points:

- `FlexChannelDecoder::new(input_rate, freq_offset_hz, baud)` — channelized IQ
  entry (mirrors the POCSAG `PocsagChannelDecoder` contract). `input_rate` is
  any capture rate ≥ `CHANNEL_RATE` (a non-integer multiple is resampled by the
  DDC); `freq_offset_hz` is the FLEX channel center relative to the capture
  center; `baud` must be **1600** (the only rate this 2-level core supports) or
  `new` returns `Err`. `process(iq)` feeds the DDC + demod, accumulates the
  channel's bit history (re-scanning from a small overlap before the last scan
  point so a Sync 1 straddling a chunk boundary is still found), and emits
  `FlexFrame`s as frames decode. Dedups by `capcode|cycle|frame|kind|
  long_address|text` so a growing buffer does not re-emit the same page. When
  `input_rate == CHANNEL_RATE` and offset is 0 the DDC is skipped (IQ is already
  channelized).
- `decode_bits(bits, baud)` — the verified bit→message core (in `lib.rs`): finds
  the Sync 1 marker, fixes polarity, steps past the marker + 16-bit C field to
  the FIW, parses the FIW, reads the 88-word phase, walks the
  BIW → address → vector → message structure, and emits one `FlexFrame` per
  address word that carried a page.

`to_message(f, frequency_hz, level_dbfs, source)` normalizes a `FlexFrame` into
the bus `Message`: `mode = Mode::Flex`, body `MessageBody::Flex { kind, details }`
where `kind` is `"alpha"` / `"numeric"` / `"tone"` and `details` is a JSON object
with `capcode`, `long_address`, `frame`, `cycle`, `baud`, `text`.
`decode.crc_ok = true` (every emitted word passed BCH + parity, possibly after
correction), `decode.fec_corrected = Some(total bits flipped by BCH across the
page's words)`, RSSI from the channel level, and the raw 32-bit words (FIW +
address + vector + message, big-endian bytes) travel as `raw`.

The public IQ constants:

- `CHANNEL_RATE = 64_000.0` S/s — 40·1600, a whole number of samples per bit at
  1600 Bd, comfortably carrying the ±4.8 kHz FSK deviation (Nyquist 32 kHz).
- `CHANNEL_PASSBAND_HZ = 9_000.0` (one-sided) — passes both ±4.8 kHz FSK tones
  plus realistic carrier tuning offset while staying well inside the channel
  rate.

`SYNC_MAX_ERR = 3` is the maximum Hamming distance tolerated when matching the
32-bit Sync 1 marker.

## IQ front end (`demod.rs`)

The 2-FSK NRZ demodulator, structured after the POCSAG / NAVTEX FSK demod for a
1600 Bd, ~±4.8 kHz-deviation binary signal:

- per-sample frequency discriminator `arg(x · conj(x_prev))`;
- a **slow DC tracker** (`FREQ_ALPHA = 0.0003`) that absorbs residual
  carrier/tuning offset so only the FSK swing remains;
- per-bit **integrate-and-dump** at 1600 Bd (`samples_per_bit = CHANNEL_RATE /
  baud = 40`) with a zero-crossing timing nudge (`TIMING_GAIN = 0.10`);
- hard slice → one bit per symbol: a positive (higher-frequency) tone slices to
  1, negative to 0.

NRZ polarity is ambiguous on air (it depends on the receiver's sideband), so the
demod does NOT try to fix absolute polarity — the channel decoder relies on the
Sync 1 hunt to resolve it.

- `word_at_lsb(bits, start)` assembles 32 bits **LSB-first** (FLEX on-air bit
  order: first bit received is bit 0) into a `u32` word (or `None` if fewer than
  32 bits remain). `word_at_msb` (MSB-first) also exists and is used only for the
  Sync 1 marker hunt (the marker is matched MSB-first on the wire).
- `find_sync(bits, max_err)` scans every bit offset, reads a 32-bit word
  MSB-first, and tests it **and its inversion** against the Sync 1 marker
  `SYNC_MARKER_B = 0xA6C6AAAA` within `max_err` bit errors (the decoder uses
  `SYNC_MAX_ERR = 3`). Returns `Some((bit_offset, inverted))` for the first
  match, where `bit_offset` is the index of the first bit of the marker and
  `inverted` means the whole bit stream's polarity must be flipped to read words.
- `level_dbfs()` reports smoothed channel power.

`BAUDS = [1600.0]` (single-element; this core implements 1600 bps 2-FSK only)
and `BAUD_1600 = 1600.0` are the public baud constants. `FskDemod::new` asserts
`samples_per_bit ≥ 4`.

## Word integrity: BCH(31,21) + parity (`bch.rs`)

Each 32-bit FLEX word, **as oriented for the error check**, is `data(21, bits
0..=20) | BCH check(10, bits 21..=30) | even-parity(1, bit 31)` (FLEX protocol
error control). The 31 low bits form a **BCH(31,21)** code generated by the
primitive polynomial

```
g(x) = x^10 + x^9 + x^8 + x^6 + x^5 + x^3 + 1   →  coefficients 0x769
```

This is the **identical BCH(31,21,2)** generator used by POCSAG (CCIR
Radiopaging Code No.1 / ITU-R M.584); FLEX reuses it. It corrects up to **2**
bit errors over the 31 protected bits, detecting more. The polynomial and code
are quoted by the public FLEX references (e.g. multimon-ng `demod_flex.c`).

**Bit-ordering note:** FLEX words are transmitted **LSB-first** on air and the
FLEX literature presents words with data in the LOW bits (bit 0 = first/least-
significant data bit) — the opposite of the POCSAG convention (flag in the MSB).
This module keeps the FLEX-native orientation: data in bits 0..=20, check in
21..=30, parity in bit 31. To reuse a single MSB-first long-division kernel it
internally **reflects** the low 31 bits (data bit 0 ↔ polynomial degree 30) for
the syndrome/encode arithmetic.

- `syndrome(word)` — BCH syndrome over the 31 protected bits (reflected, then
  MSB-first reduction by `g(x)`). Zero ⇔ valid BCH word.
- `parity_bit(word_31)` — even parity over the low 31 bits.
- `is_valid(word)` — syndrome zero **and** overall even parity (all 32 bits).
- `encode(data21)` — systematic encode: 21 data bits in the low positions, BCH
  check bits in 21..=30, even parity in bit 31. Top 21 bits of the result equal
  the input data. Re-exported by `frame::encode_word`.
- `correct(word)` — brute-force syndrome decoding. Returns `Some((corrected_word,
  bits_flipped))` if a valid word is within Hamming distance 2 (including the
  parity bit), else `None`. It handles 0 errors, a lone parity-bit error, then
  exhaustive 1- and 2-bit error patterns over the 31 protected bits (1 + 31 +
  31·30/2 = 497 candidates) — cheap, exhaustive, and maximum-likelihood within
  the code's distance.

Spec constants: `BCH_POLY = 0x769`, `DATA_BITS = 21`.

## Frame / phase framing (`frame.rs`)

Per the public FLEX protocol description (constants cited to multimon-ng
`demod_flex.c`, reproduced from Motorola's *FLEX Protocol — Technical Summary*):

A FLEX **frame** lasts 1.875 s and is structured as:

```text
  Sync 1   : BS1 dotting | A (32b) | B (16b) | inverted-A (32b)
  FIW      : 32-bit Frame Information Word (BCH(31,21)+parity protected)
  Sync 2   : bit/frame fine sync
  Data     : 11 blocks, each 8 words of 32 bits  (= 88 words / "phase")
```

At 1600 bps 2-level FSK there is a single phase of 88 words per frame
(`WORDS_PER_PHASE = 88`, `BLOCKS_PER_FRAME = 11`, `WORDS_PER_BLOCK = 8`).

- **Frame Information Word** (`parse_fiw`, multimon-ng `decode_fiw`, after
  masking `fiw & 0x001FFFFF`): `cycle` = bits 4..=7 (0..=14), `frame` = bits
  8..=14 (0..=127). A FLEX **mod-16 checksum** verifies: the sum of the 4-bit
  nibbles `[0..=3] [4..=7] [8..=11] [12..=15] [16..=19]` plus bit 20, taken mod
  16, must equal `0xF` (`checksum_ok`).
- **Block Information Word** = phase word 0 (`parse_biw`, multimon-ng
  `decode_biw`): `address_offset = ((biw >> 8) & 0x03) + 1` (first word index of
  the address field), `vector_offset = (biw >> 10) & 0x3F` (first word index of
  the vector field).
- **Address words** run from `address_offset` up to (but not including)
  `vector_offset`. Each is decoded by `decode_short_address(aw1)`: `capcode =
  aw1 − 0x8000`, and a `long` flag set when `aw1 < 0x0000_8001 || aw1 >
  0x001E_0000` (the documented long-address window).
- **Vector Information Word** for address `i` lives at `vector_offset + (i −
  address_offset)`. `PageType::from_viw` reads the 3-bit type field `(viw >> 4)
  & 0x7`. For numeric/alphanumeric vectors the VIW also carries the message
  body's location into the phase: **start word = bits 7..=13**, **word count =
  bits 14..=20**.
- **Message words** at the VIW-pointed range carry the page body.

### Page (vector) types

`PageType::from_viw` (multimon-ng `FLEX_PAGETYPE_*`, VIW bits 4..=6):

| Value | `PageType` | bus `kind` |
|---|---|---|
| 0 | `Secure` | tone |
| 1 | `ShortInstruction` (group-message header) | tone |
| 2 | `Tone` (tone-only) | tone |
| 3 | `StandardNumeric` | numeric |
| 4 | `SpecialNumeric` | numeric |
| 5 | `Alphanumeric` (7-bit chars) | alpha |
| 6 | `Binary` | alpha |
| 7 | `NumberedNumeric` | numeric |

`PageType::kind_str()` collapses these into the three bus classes; `FlexKind`
(`Alpha` / `Numeric` / `Tone`, with `as_str()` → `"alpha"`/`"numeric"`/`"tone"`)
is derived from it. `Tone` / `Secure` / `ShortInstruction` produce a `Tone` page
with empty text; `Alphanumeric` / `Binary` decode as alpha; the three numeric
variants decode as numeric.

### Alpha / numeric body decode

**Alphanumeric** (`decode_alpha`): 7-bit ASCII characters packed **LSB-first**
into the 21 data bits of consecutive message words, 3 chars per word (char0 =
bits 0..=6, char1 = bits 7..=13, char2 = bits 14..=20; multimon-ng
`parse_alphanumeric`). `0x03` (ETX) bytes are FLEX message-segment terminators
and are skipped; trailing control/NUL padding (< 0x20) is trimmed so the visible
text is clean.

**Numeric** (`decode_numeric`): 4-bit groups packed **LSB-first** into the low
21 bits of each message word (up to 5 nibbles / 20 bits of digits per word),
mapped via the 16-entry FLEX numeric table (multimon-ng `parse_numeric`):

```
0 1 2 3 4 5 6 7 8 9 (space) U (space) - ] [
```

(index 11 = `U`, 13 = `-`, 14/15 = `]`/`[`). Trailing pad spaces are trimmed.

## Validation / oracles

The DECODE/framing layer verifies against **spec-cited** ground truth —
hand-built words assembled from the FLEX field layout (cited to multimon-ng
`demod_flex.c`), not an encode→decode self-loopback hidden behind the modulator.
The DEMOD front end is validated **only** by a synthetic modulate→AWGN→demod
path. There is **no real off-air FLEX IQ** anywhere in this crate. All tests are
inline `#[cfg(test)]` modules in each source file (there is no `tests/`
directory).

| Layer | Fact | Spec cite | How verified |
|---|---|---|---|
| BCH(31,21,2) | generator `g(x)=0x769`, syndrome correction, even parity, systematic encode | FLEX protocol error control (also ITU-R M.584 / multimon-ng `demod_flex.c`) | `encode_produces_valid_words`, `corrects_single_bit_error` (all 32 bit positions), `corrects_double_bit_error`, `rejects_uncorrectable_triple_error`, `reflect_is_involution` |
| FIW | cycle bits 4..=7, frame bits 8..=14, mod-16 checksum = 0xF | multimon-ng `decode_fiw` | `fiw_fields_and_checksum_roundtrip` (cycle=5, frame=42; corrupted frame breaks checksum) |
| BIW | address offset `((biw>>8)&3)+1`, vector offset `(biw>>10)&0x3F` | multimon-ng `decode_biw` | `biw_offsets_match_spec_layout` |
| Address | `capcode = aw1 − 0x8000`, long-address window | multimon-ng `demod_flex.c` | `short_address_capcode` (capcode + long flag inside / below / above the window) |
| Alpha table | 7-bit ASCII LSB-first, 3 chars/word | multimon-ng `parse_alphanumeric` | `alpha_decode_lsb_first_7bit` ("Hi!"), `alpha_decode_two_words` ("HELLO") |
| Numeric table | 4-bit LSB-first FLEX BCD table | multimon-ng `parse_numeric` | `numeric_decode_4bit_table` ("12345") |
| Page type | VIW bits 4..=6 → page type, kind mapping | multimon-ng `FLEX_PAGETYPE_*` | `page_type_from_viw` |
| Sync hunt | MSB-first marker read, ≤N-error match, polarity inversion | multimon-ng `FLEX_SYNC_MARKER 0xA6C6AAAA` | `word_at_orderings`, `find_sync_locates_marker_with_offset`, `find_sync_handles_inverted_polarity`, `find_sync_tolerates_bit_errors` |

End-to-end **DECODE** tests (in `lib.rs`) build a full FLEX frame from
spec-constructed, BCH-encoded words (via `build_alpha_frame`) and run the
bit-level decoder:

- `decode_bits_recovers_spec_alpha_page` — hand-built FIW (cycle 7, frame 33) +
  BIW + address (capcode 1,234,567) + VIW (type 5, alphanumeric) + message words
  for `"HELLO WORLD"`; asserts `FlexKind::Alpha`, cycle, frame,
  `PageType::Alphanumeric`, and the recovered text. Spec ground truth, not a
  modulator round trip.
- `decode_bits_recovers_spec_numeric_page` — numeric page `"12345"` with the
  4-bit LSB-first nibble layout and VIW type 3 (standard numeric); asserts
  capcode, `FlexKind::Numeric`, leading digits.
- `decode_bits_recovers_spec_tone_page` — VIW type 2 (tone), no message body;
  asserts `FlexKind::Tone`, `PageType::Tone`, empty text.
- `to_message_emits_flex_body` — confirms `Mode::Flex`, `MessageBody::Flex {
  kind, details }` with `capcode`/`long_address`/`frame`/`cycle`/`baud`/`text`,
  `crc_ok = true`, `fec_corrected`.
- `channel_rate_is_integer_bit_multiple` — every supported baud yields an integer
  samples/bit and the channel rate carries the two-sided passband.

**SYNTHETIC DEMOD** validation (explicitly reported as synthetic — NO real RF),
in `lib.rs`, using `modulate.rs`:

- `demod_recovers_page_synth_iq` — modulate a real spec-built alpha frame
  (capcode 1,234,567, `"PAGE ME"`) to 2-FSK IQ at 1600 Bd, add complex AWGN at
  **16 dB SNR**, run the full `FlexChannelDecoder` (DDC → discriminator → timing
  → Sync 1 → BCH → text), and require the spec page recovered intact.
- `demod_raw_ber_synth_iq` — modulate a deterministic LFSR-generated payload of
  valid FLEX words at 1600 Bd, add complex AWGN at **14 dB SNR**, demod, align on
  the Sync 1 marker, and assert the raw (pre-FEC) BER is **< 5 %** so
  BCH(31,21,2) can clean up the residual (> 1000 bits compared). A synthetic AWGN
  figure, not a real-RF claim.

The modulator (`modulate.rs`) is a self-generated reference, NOT an external
oracle: its waveform parameters (1600 Bd, ±4.8 kHz deviation — `DEVIATION_HZ`,
alternating dotting preamble, Sync 1 marker MSB-first then 16-bit C field, then
data words LSB-first) are the published FLEX 2-level PHY, but it only proves the
demod inverts this modulation. The DECODE core stays spec-anchored by its own
word/FIW/BIW tests.

## Known limitations / deferred (skip-don't-fake)

- **No real off-air validation.** The entire DEMOD chain is validated only by the
  synthetic modulate→complex-AWGN→demod path; no recorded FLEX capture exists in
  this crate. The crate header states this explicitly. All BER/SNR figures
  (16 dB, 14 dB, < 5 % raw BER) are synthetic AWGN, not real RF.
- **1600 bps 2-FSK only — 4-level 3200/6400 bps NOT implemented.** `BAUDS =
  [1600.0]`; `FlexChannelDecoder::new` returns `Err` for any other baud and the
  runtime hard-codes 1600. The 4-level (4-FSK) PHY and its multi-phase
  structure are intentionally skipped, not faked. (Runtime note: 4-FSK
  3200/6400 and per-session baud are flagged as follow-ups.)
- **Long-capcode fusion SKIPPED.** `decode_short_address` reports the documented
  `long` flag and the `aw1 − 0x8000` capcode (the first word of a long pair).
  The full TWO-word long-address reconstruction is *commented out / "Don't ask"*
  in the public reference and is not reliably specified, so fusing the second
  long-address word is deliberately skipped rather than faked. Long-address
  pages therefore carry the short-form capcode with `long_address = true`.
- **Advanced vector types not expanded.** Secure, binary, special/numbered
  numeric (beyond table decode), group-message expansion (short-instruction
  headers → member pages), and fragment reassembly across frames are NOT
  implemented. Secure / short-instruction VIWs are emitted as tone pages;
  binary decodes through the alpha path; the numeric variants share the standard
  numeric table.
- **Operator-known offset; no carrier search.** `FlexChannelDecoder::new`
  requires the caller to supply `freq_offset_hz`; there is no automatic
  channel/carrier acquisition. The demod's slow DC tracker absorbs only residual
  tuning error, not a coarse offset.
- **Single phase per frame.** Only the 1600 bps 88-word single phase is read;
  the multi-phase interleave of the high-rate modes is not handled (a consequence
  of the 2-FSK-only scope).
- **No position output.** FLEX carries no fix, so a decoded page has no map
  location; it surfaces as a text/message record only (not on the dashboard
  "beacons" layer).

## Gotchas

1. **Two bit orders in one mode.** The Sync 1 marker is read/matched **MSB-first**
   (`word_at_msb`, `find_sync`); every FLEX *data* word is read **LSB-first**
   (`word_at_lsb`). Alpha chars (7-bit) and numeric nibbles (4-bit) are then
   packed LSB-first within the word's low 21 bits. Do not assume one order.
2. **BCH orientation is data-in-low-bits**, the opposite of POCSAG (flag in the
   MSB). The `bch` module internally reflects the 31 bits for its long-division
   kernel; treat `encode`/`syndrome`/`correct` as FLEX-native (data in bits
   0..=20) at the call sites.
3. **Sync 1 is 64 bits but only the middle 32 are matched.** `decode_bits` locks
   the 32-bit marker `0xA6C6AAAA`, then steps `+32 + 16` (marker + 16-bit C /
   inverted-A field) to reach the FIW. The leading dotting and the trailing
   inverted-A are not used for the lock.
4. **NRZ polarity is ambiguous**; `find_sync` tests both the word and its
   inversion and returns an `inverted` flag. `read_word` flips every word when it
   is set. There is no separate polarity-resolution pass.
5. **Phase indices must stay aligned.** Unreadable (uncorrectable) phase words
   are pushed as a `0` sentinel so the BIW's address/vector-offset indexing stays
   correct; a `0` address word is skipped.
6. **The VIW points at the body.** Message-word location is `start = (viw >> 7) &
   0x7F`, `count = (viw >> 14) & 0x7F` into the phase — not a fixed position
   after the address word.
7. `FlexChannelDecoder::new` returns `Err` for any baud not in `BAUDS =
   [1600]`; baud is per-decoder, fixed at construction (the runtime always
   passes 1600).
8. The channel decoder dedups on `capcode|cycle|frame|kind|long_address|text`
   and re-scans with overlap across `process` chunks — do not assume one frame
   per call or that a repeated identical page is re-emitted.
9. BCH `correct` returns `None` for > 2 errors; such words become `0` sentinels
   and are skipped, so `crc_ok` is `true` for every *emitted* frame by
   construction (only correctable words ever contribute to a `FlexFrame`).
10. `CHANNEL_RATE = 64000` is deliberately `40·1600` so 1600 Bd has an integer
    samples/bit (asserted by `channel_rate_is_integer_bit_multiple`).

## Key references

- **multimon-ng** `demod_flex.c` (the de-facto open FLEX reference) — Sync 1
  marker `0xA6C6AAAA`, 88-words-per-phase framing, `decode_fiw` / `decode_biw`,
  address-word capcode formula and long-address window, VIW page-type field,
  alphanumeric / numeric body tables, BCH(31,21) generator (facts only; no code
  copied).
- **Motorola FLEX Protocol — Technical Summary** / TIA-EIA FLEX references — the
  authoritative air-interface definition: frame timing (1.875 s), Sync 1/FIW/
  Sync 2/data structure, BCH(31,21)+parity word format, address/vector/message
  word layout, page-vector types.
- **ITU-R Recommendation M.584** (CCIR Radiopaging Code No.1) — the source of the
  shared BCH(31,21,2) generator `g(x) = 0x769` that FLEX reuses.
- `docs/notes/POCSAG.md` — sibling paging mode whose `*ChannelDecoder` / DDC
  front-end structure and shared BCH(31,21) code this crate mirrors.
- `docs/notes/NAVTEX.md` — sibling FSK mode and the `*ChannelDecoder` template
  pattern.
