# POCSAG (CCIR Radiopaging Code No.1 / ITU-R M.584-2) — implementation notes

Native POCSAG message decode core for `xng-mode-pocsag`. POCSAG is the
dominant one-way radio-paging protocol: binary **2-FSK** at
**512 / 1200 / 2400 baud** with roughly ±4.5 kHz deviation, carrying the
**CCIR Radiopaging Code No.1** (ITU-R Recommendation M.584-2, Annex 1).
A transmission is a long alternating preamble (≥576 bits), then one or
more **batches**; each batch is a 32-bit frame-sync codeword
(`0x7CD215D8`) followed by **8 frames of 2 codewords each** (16
codewords). Every 32-bit codeword is a flag bit + 20 information bits +
**BCH(31,21,2)** check bits + an even-parity bit. The crate splits into a
spec-anchored DECODE/framing core (BCH, codeword layout, text tables) and
a 2-FSK IQ front end. The DECODE core is verified against hand-built,
spec-cited codewords; the DEMOD is verified ONLY by a synthetic
modulate→AWGN→demod test — no off-air IQ exists.

Status: **WIRED, SYNTHETIC-ONLY validation.** Runtime mode `Mode::Pocsag`,
`MessageBody::Pocsag`, and a `PocsagChannelDecoder` that owns an
`xng_dsp::Ddc`. The framing/decode layer is anchored to spec-cited
byte/bit vectors; the IQ→bits front end is exercised only by a synthetic
modulate→complex-AWGN→demod path. There is **no real off-air capture** in
this crate.

## Pipeline

```
wideband capture IQ
  → Ddc                     mix by freq_offset_hz, decimate to CHANNEL_RATE (38.4 kS/s)
  → demod::FskDemod         freq discriminator + DC tracker + baud-rate timing → 1 bit/symbol
recovered NRZ bit history (any polarity)
  → demod::find_sync        locate sync codeword 0x7CD215D8 (≤2 errs), resolve polarity
  → demod::word_at          read 32 bits MSB-first per codeword
  → bch::correct            BCH(31,21,2) syndrome correction + even parity per codeword
  → frame::classify         address / message / idle by flag bit + frame position
  → frame::decode_numeric / frame::decode_alpha   payload bits → text
  → PocsagFrame             (capcode, function, baud, kind, text, fec_corrected, raw)
  → to_message              → xng_types::Message bus form
```

Two entry points:

- `PocsagChannelDecoder::new(input_rate, freq_offset_hz, baud)` —
  channelized IQ entry (mirrors the AIS/NAVTEX `*ChannelDecoder`
  contract). `baud` must be one of 512 / 1200 / 2400 or `new` returns
  `Err`. `process(iq)` feeds the DDC + demod, accumulates the channel's
  bit history (re-scanning from a small overlap before the last scan
  point so a sync straddling a chunk boundary is still found), and emits
  `PocsagFrame`s as complete batches decode. Dedups by
  `capcode|function|kind|text` so a growing buffer does not re-emit the
  same message. When `input_rate == CHANNEL_RATE` and offset is 0 the DDC
  is skipped (IQ is already channelized).
- `decode_bits(bits, baud)` — the verified bit→message core (in
  `lib.rs`): finds the sync codeword, fixes polarity, then reads
  consecutive batches and returns one `PocsagFrame` per address codeword
  that carried information.

`to_message(f, frequency_hz, level_dbfs, source)` normalizes a
`PocsagFrame` into the bus `Message`: `mode = Mode::Pocsag`, body
`MessageBody::Pocsag { kind, details }` where `kind` is
`"numeric"` / `"alpha"` / `"tone"` and `details` is a JSON object with
`capcode`, `function`, `baud`, `text`. `decode.crc_ok = true` (every
emitted codeword passed BCH + parity, possibly after correction),
`decode.fec_corrected = Some(total bits flipped by BCH)`, RSSI from the
channel level, and the raw 32-bit codewords (big-endian bytes) travel as
`raw`.

The public IQ constants:

- `CHANNEL_RATE = 38_400.0` S/s — a common integer multiple of all three
  bauds (38400 = 75·512 = 32·1200 = 16·2400), so every baud has a whole
  number of samples per bit, and it comfortably carries the ±4.5 kHz FSK
  deviation (Nyquist 19.2 kHz).
- `CHANNEL_PASSBAND_HZ = 7_500.0` (one-sided) — passes both ±4.5 kHz FSK
  tones plus realistic carrier tuning offset while staying well inside
  the channel rate.

## IQ front end (`demod.rs`)

The 2-FSK NRZ demodulator, structured after the NAVTEX FSK demod but for
a wider-shift, faster signal:

- per-sample frequency discriminator `arg(x · conj(x_prev))`;
- a **slow DC tracker** (`FREQ_ALPHA = 0.0003`) that absorbs residual
  carrier/tuning offset so only the FSK swing remains;
- per-bit **integrate-and-dump** at the selected baud
  (`samples_per_bit = CHANNEL_RATE / baud`) with a zero-crossing timing
  nudge (`TIMING_GAIN = 0.10`);
- hard slice → one bit per symbol: a positive (higher-frequency) tone
  slices to 1, negative to 0.

NRZ polarity is ambiguous on air (it depends on the receiver's sideband),
so the demod does NOT try to fix absolute polarity — the channel decoder
tries both and keeps whichever locks the sync codeword.

- `word_at(bits, start)` assembles 32 bits **MSB-first** into a `u32`
  codeword (or `None` if fewer than 32 bits remain).
- `find_sync(bits, max_err)` scans every bit offset, reads a 32-bit word
  (MSB-first), and tests it **and its inversion** against
  `SYNC_CODEWORD = 0x7CD215D8` within `max_err` bit errors (the decoder
  uses `SYNC_MAX_ERR = 2`). Returns `Some((bit_offset, inverted))` for
  the first match; `inverted` means the whole bit stream's polarity must
  be flipped to read codewords.
- `level_dbfs()` reports smoothed channel power.

`BAUDS = [512.0, 1200.0, 2400.0]` (ITU-R M.584-2 §1) is the public list
of supported bauds. `FskDemod::new` asserts `samples_per_bit ≥ 4`.

## Codeword integrity: BCH(31,21,2) + parity (`bch.rs`)

Each 32-bit codeword is `flag(1) | message/data(20) | BCH check(10) |
even-parity(1)` (ITU-R M.584-2 Annex 1 §2, "Code structure"). The first
31 bits form a **BCH(31,21)** code generated by the primitive polynomial

```
g(x) = x^10 + x^9 + x^8 + x^6 + x^5 + x^3 + 1   →  coefficients 0x769
```

(ITU-R M.584-2 Annex 1 §2.2 "Check bits"; the same polynomial is quoted
by every public POCSAG reference, e.g. multimon-ng `pocsag.c` `BCH_POLY`).
BCH(31,21,2) corrects up to **2** bit errors over the 31 protected bits.

- `parity_remainder(cw)` — BCH syndrome: remainder of the top 31 bits
  (excluding the parity LSB) modulo `g(x)`, computed by MSB-first long
  division. Zero ⇔ valid BCH word.
- `parity_bit(cw_31)` — even parity over the 31 protected bits.
- `is_valid(cw)` — syndrome zero **and** overall even parity.
- `encode(data21)` — systematic encode: shift 21 data bits up by 10,
  append the BCH remainder as the 10 check bits, then the even-parity bit.
  Top 21 bits of the result equal the input data.
- `correct(cw)` — brute-force syndrome decoding. Returns
  `Some((corrected_codeword, bits_flipped))` if a valid codeword is within
  Hamming distance 2 (including the parity bit), else `None`. It handles
  0 errors, a lone parity-bit error, then exhaustive 1- and 2-bit error
  patterns over the 31 protected bits (1 + 31 + 31·30/2 = 497 candidates)
  — cheap and giving maximum-likelihood correction within the code's
  distance.

Spec constants: `IDLE_CODEWORD = 0x7A89C197` (Annex 1 §2.3, transmitted
in unused codeword positions) and `SYNC_CODEWORD = 0x7CD215D8` (Annex 1
§2.4, precedes every batch). The sync codeword is found by raw bit match,
NOT by BCH validity.

## Batch / codeword framing (`frame.rs`)

Per ITU-R M.584-2 Annex 1:

- **§2.1 Preamble** — at least 576 alternating 1/0 bits (the reversal
  sequence) for bit-clock recovery (`PREAMBLE_MIN_BITS = 576`).
- **§2.4 Batch** — sync codeword `0x7CD215D8`, then 8 frames × 2 codewords
  = 16 codewords (`CODEWORDS_PER_BATCH = 16`), 17 words including sync.
- **§2.2 Address codeword** (flag bit = 0) — bits carry the most
  significant 18 bits of the address; the receiver's **frame position**
  (0..=7, which of the 8 frames the codeword fell in) supplies the 3 least
  significant address bits, so the full pager number ("capcode") is
  `(address18 << 3) | frame_position`. Two **function bits** select one of
  four message types / tone alerts.
- **§2.3 Message codeword** (flag bit = 1) — 20 message bits. Consecutive
  message codewords (until the next address/idle/sync or end of
  transmission) are concatenated MSB-first into one bit stream, then
  decoded as numeric or alphanumeric.

`classify(cw, frame_position) -> Codeword` returns `Idle` for the idle
codeword, else reads the flag bit: flag 0 → `Address { capcode, function }`
(capcode assembled with `frame_position`), flag 1 →
`Message { payload20 }` (20 payload bits MSB-first in the low 20 bits).

**Numeric decode** (`decode_numeric`): 4 bits per digit, each 4-bit group
**bit-reversed**, mapped via the §2.3 16-entry table:

```
0 1 2 3 4 5 6 7 8 9 (space) U (space) - ] [
```

(indices 0..=15 after bit reversal; index 11 = `U` urgency, 13 = `-`,
14/15 = `]`/`[`). A trailing partial group (< 4 bits) is ignored.

**Alphanumeric decode** (`decode_alpha`): 7 bits per character,
**LSB-first**, mapped to ASCII (§2.3). A trailing partial group (< 7 bits)
is ignored, and trailing control bytes (< 0x20, the NUL/EOT padding
pagers append) are trimmed so the visible text is clean.

`message_bits(payloads)` concatenates the 20-bit payloads MSB-first into
one bit vector for the two decoders.

### kind / function selection (`lib.rs`)

The numeric/alphanumeric choice is signalled out-of-band by the function
bits / paging-operator convention; the decoder picks by function code:
**function 3 → `PocsagKind::Alpha`**, all others → `PocsagKind::Numeric`
(the de-facto convention; the spec leaves the choice to the function bits
/ paging plan). An address codeword with **no** following message
codewords is a `PocsagKind::Tone` page (empty text). Orphan message
codewords with no preceding address are dropped (cannot be attributed to a
capcode).

## Validation / oracles

The DECODE/framing layer verifies against **spec-cited** ground truth —
hand-built codewords assembled from the ITU-R M.584-2 field layout, not an
encode→decode self-loopback hidden behind the modulator. The DEMOD front
end is validated **only** by a synthetic modulate→AWGN→demod path. There
is **no real off-air IQ** anywhere in this crate. All tests are inline
`#[cfg(test)]` modules in each source file (there is no `tests/`
directory).

| Layer | Fact | Spec cite | How verified |
|---|---|---|---|
| BCH(31,21,2) | generator `g(x)=0x769`, syndrome correction, even parity | ITU-R M.584-2 Annex 1 §2.2 (also multimon-ng `pocsag.c` `BCH_POLY`) | `encode_produces_valid_codewords`, `corrects_single_bit_error` (all 32 bit positions), `corrects_double_bit_error`, `rejects_uncorrectable_triple_error` |
| Idle / sync constants | `IDLE = 0x7A89C197`, `SYNC = 0x7CD215D8` | ITU-R M.584-2 Annex 1 §2.3 / §2.4 | `idle_and_sync_are_valid_codewords` (idle passes BCH+parity; sync asserted to the spec value) |
| Codeword layout | capcode `(addr18<<3)\|frame_position`, 2 function bits, flag bit, idle | ITU-R M.584-2 Annex 1 §2.2/§2.3 | `address_codeword_roundtrips_capcode_and_function`, `idle_codeword_classifies_as_idle` |
| Numeric table | 4-bit bit-reversed digit/symbol table | ITU-R M.584-2 Annex 1 §2.3 | `numeric_decode_matches_spec_layout` (digits "12345") |
| Alpha decode | 7-bit LSB-first ASCII | ITU-R M.584-2 Annex 1 §2.3 | `alpha_decode_lsb_first_ascii` ("Hi"), `message_bits_is_msb_first` |
| Sync hunt | MSB-first word read, ≤2-error sync match, polarity inversion | ITU-R M.584-2 Annex 1 §2.4 | `word_at_is_msb_first`, `find_sync_locates_codeword_with_offset`, `find_sync_handles_inverted_polarity`, `find_sync_tolerates_bit_errors` |

End-to-end **DECODE** tests (no modulator) build full batches from
spec-constructed codewords and run the bit-level decoder:

- `decode_bits_recovers_spec_alpha_message` — hand-built address (function
  3) + alphanumeric message codewords for `"HI"`; asserts capcode,
  function, `PocsagKind::Alpha`, text. Spec ground truth, not a modulator
  round trip.
- `decode_bits_recovers_spec_numeric_message` — numeric page
  `"0123456789"` with §2.3 bit-reversed nibble layout, space padding;
  asserts capcode, `PocsagKind::Numeric`, leading digits.
- `decode_bits_recovers_tone_page` — an address codeword in the correct
  frame slot (capcode low 3 bits = frame position, per §2.2) with no
  message codewords; asserts `PocsagKind::Tone`, empty text.
- `to_message_emits_pocsag_body` — confirms `Mode::Pocsag`,
  `MessageBody::Pocsag { kind, details }` with `capcode`/`function`/`baud`/
  `text`, `crc_ok = true`, `fec_corrected`.
- `channel_rate_is_integer_bit_multiple_for_all_bauds` — every baud yields
  an integer samples/bit and the channel rate carries the two-sided
  passband.

**SYNTHETIC DEMOD** validation (explicitly reported as synthetic — NO real
RF), in `lib.rs`, using `modulate.rs`:

- `demod_ber_synth_iq` — modulate a real `"PAGE"` batch to 2-FSK IQ at
  1200 Bd, add complex AWGN at **14 dB SNR**, run the full
  `PocsagChannelDecoder` (DDC → discriminator → timing → sync → BCH →
  text), require the spec page recovered intact.
- `demod_raw_ber_synth_iq_all_bauds` — for all three bauds: modulate a
  deterministic LFSR-generated codeword payload, add complex AWGN at
  **12 dB SNR**, demod, align on sync, and assert the raw (pre-FEC) BER is
  **< 5 %** so BCH(31,21,2) can clean up the residual. A synthetic AWGN
  figure, not a real-RF claim.

The modulator (`modulate.rs`) is a self-generated reference, NOT an
external oracle: its waveform parameters (512/1200/2400 Bd, ±4.5 kHz
deviation, alternating preamble, MSB-first words) are the published spec,
but it only proves the demod inverts this modulation. The DECODE core
stays spec-anchored by its own codeword tests.

## Known limitations / deferred

- **No real off-air validation.** The entire DEMOD chain is validated only
  by the synthetic modulate→complex-AWGN→demod path; no recorded POCSAG
  capture exists in this crate. The crate's own header states this
  explicitly: "no off-air IQ is available." All BER/SNR figures (14 dB,
  12 dB, < 5 % raw BER) are synthetic AWGN, not real RF.
- **Operator-known baud and offset.** `PocsagChannelDecoder::new` requires
  the caller to supply both the baud (512/1200/2400) and the
  `freq_offset_hz`; there is no automatic baud detection and no automatic
  channel/carrier acquisition. The demod's slow DC tracker absorbs only
  residual tuning error, not a coarse offset.
- **Function-code heuristic for numeric vs alpha.** The numeric/alpha
  class is chosen solely by `function == 3 → alpha`, the de-facto
  convention; a transmission whose paging plan uses a different mapping
  would be mis-classed. (The body is decoded one way per the function
  code; both decodings are available in `frame` but only one is emitted.)
- **Orphan message codewords dropped.** Message codewords seen before any
  address codeword in a batch are discarded — they cannot be attributed to
  a capcode.
- **No ROT-1 / CCIR shift extensions, no character-set quirks.** The alpha
  table is plain 7-bit ASCII; no vendor extensions or alternate alphabets
  are handled.
- **No position output.** POCSAG carries no fix, so a decoded page has no
  map location; it surfaces as a text/message record only (not on the
  dashboard "beacons" layer).

## Gotchas

1. Codeword bits are read **MSB-first** (`word_at`, `message_bits`,
   `push_word`); the alpha character bits are **LSB-first**; the numeric
   nibble is **bit-reversed**. Three different bit orders in one mode — do
   not assume one.
2. The capcode's low 3 bits ARE the frame position — an address codeword
   MUST appear in the frame whose number equals `capcode & 7` (§2.2). The
   tone-page test relies on this; `classify` needs the correct
   `frame_position` argument or the capcode is wrong.
3. NRZ **polarity is ambiguous**; the decoder tries both and locks on
   whichever matches the sync codeword. `find_sync` returns an `inverted`
   flag and the batch read flips every word when it is set.
4. The sync codeword `0x7CD215D8` is matched by **raw bits within ≤2
   errors**, NOT by BCH validity — it is not required to be a valid BCH
   codeword. (The idle codeword IS a valid BCH+parity word.)
5. `CHANNEL_RATE = 38400` is deliberately `75·512 = 32·1200 = 16·2400` so
   every baud has an integer samples/bit; changing it breaks the
   integer-bit invariant (asserted by
   `channel_rate_is_integer_bit_multiple_for_all_bauds`).
6. `PocsagChannelDecoder::new` returns `Err` for any baud not in
   `BAUDS = [512, 1200, 2400]`; baud is per-decoder, fixed at
   construction.
7. The channel decoder dedups on `capcode|function|kind|text` and
   re-scans with overlap across `process` chunks — do not assume one
   frame per call or that a repeated identical page is re-emitted.
8. BCH `correct` returns `None` for > 2 errors; such codewords are
   skipped, so `crc_ok` is `true` for every *emitted* frame by
   construction (only correctable codewords ever reach a `PocsagFrame`).

## Key references

- **ITU-R Recommendation M.584-2** ("Codes and formats for radio
  paging"), Annex 1 ("The radiopaging code No.1") — the authoritative
  spec: §2.1 preamble (≥576 bits), §2.2 address codeword + BCH check bits +
  generator polynomial, §2.3 message codeword + numeric/alpha tables + idle
  codeword, §2.4 batch structure + sync codeword.
- **CCIR Radiopaging Code No.1** — the original POCSAG (Post Office Code
  Standardisation Advisory Group) definition standardised as M.584.
- **multimon-ng** `pocsag.c` (`BCH_POLY`) — public cross-reference for the
  `0x769` BCH generator polynomial and codeword constants (facts only; no
  code copied).
- `docs/notes/NAVTEX.md` — sibling FSK mode whose `*ChannelDecoder` / DDC
  front-end structure this crate mirrors.
