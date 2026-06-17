# NAVTEX (SITOR-B / CCIR 476) — implementation notes

Native NAVTEX message decode core for `xng-mode-navtex`. NAVTEX is the
international maritime safety-information broadcast on 518 kHz (English),
490 kHz (national) and 4209.5 kHz (tropical/HF); on air it is 100-baud
narrow-shift (±85 Hz) FSK carrying the **CCIR 476** seven-bit
constant-ratio code in collective B-mode (**FEC-B**): every character is
sent twice with time diversity. Clean-room: no decoder was copied or
ported — only protocol facts, code tables, and one published worked
example, each cited (see `PROVENANCE.md`). The **decode layer** (symbols
→ message) is anchored to **external** oracles, never an encode→decode
self-loopback. On top of it sits a channelized **IQ front end** (DDC +
narrow-shift FSK demod) that turns a wideband capture into the symbol
stream the decode core consumes.

Status: **WIRED + OFF-AIR-VALIDATED.** Full runtime mode: `Mode::Navtex`,
`MessageBody::Navtex`, `--mode navtex`, CLI/TUI/scan paths, and a
`NavtexChannelDecoder` that owns an `xng_dsp::Ddc`. The IQ→symbol front
end decodes a **real off-air USCG NAVTEX message** (SDRplay's official
`navtex.zip` IQ demo) char-identical to the fldigi/YaND ground truth —
29 frames through the built-in DDC, CI floor 25 (`bench/baselines.json`
`navtex_offair`). The front end's modulate→demod path is additionally
exercised synthetically; the DECODE core stays oracle-anchored.

## Pipeline

```
wideband capture IQ
  → Ddc                     mix by freq_offset_hz, decimate to CHANNEL_RATE (4800 S/s)
  → demod::FskDemod         freq discriminator + DC tracker + 100 Bd timing → 1 bit/symbol
  → demod::pack_codes       LSB-first 7-bit packing (all 7 alignments tried)
interleaved CCIR 476 symbol stream (one 7-bit code per element)
  → fec::find_phase         locate the first DX slot
  → fec::recover_stream     DX/RX time-diversity per character
  → fec::codes_to_text      LTRS/FIGS shift tracking, drop phasing/idle
  → message::parse          ZCZC B1B2B3B4 header / body / NNNN end
  → message::NavtexMessage  (serde JSON) → to_message → xng_types::Message bus form
```

Two entry points:

- `NavtexChannelDecoder::new(input_rate, freq_offset_hz)` — channelized
  IQ entry (mirrors the AIS `AisChannelDecoder` contract). `process(iq)`
  feeds the DDC + demod, accumulates the channel's bit history (NAVTEX
  bursts are slow and long, so bits are buffered and re-scanned), and
  emits a `NavtexFrame` once a complete `ZCZC … NNNN` message parses.
  Dedups by header identity + body text so a growing buffer does not
  re-emit the same message. When `input_rate == CHANNEL_RATE` and offset
  is 0 the DDC is skipped (IQ is already channelized).
- `decode_symbols(symbols, first_dx)` — the verified symbol→message core
  (in `lib.rs`): each element is one packed 7-bit CCIR 476 code. If
  `first_dx` is `None` it phase-locks via `find_phase`; returns `None`
  only when the stream is too short to lock.

`to_message(frame, frequency_hz, level_dbfs, source)` normalizes a
`NavtexFrame` into the bus `Message`: `mode = Mode::Navtex`, body
`MessageBody::Navtex { kind, details }` where `kind` is the B2 subject
letter (or `"?"`), `details` is the `NavtexMessage` JSON, `decode.crc_ok`
= `header_ok && end_ok`, RSSI from the channel level, and the recovered
wire symbols travel as `raw`.

`params` carries the on-air constants: `BAUD = 100.0`, `SHIFT_HZ = 85.0`,
`BITS_PER_SYMBOL = 7`, and the three carrier frequencies
(518/490/4209.5 kHz). `CHANNEL_RATE = 4800` S/s (48 samples/bit) and
`CHANNEL_PASSBAND_HZ = 250` (one-sided; passes both ±85 Hz tones plus a
realistic tuning offset, rejects the 28 kHz-distant adjacent channel).

## IQ front end (`demod.rs`)

The narrow-shift FSK demodulator, structured after the AIS `GmskDemod`
but for an un-shaped 100 Bd ±85 Hz binary FSK signal:

- per-sample frequency discriminator `arg(x · conj(x_prev))`;
- a **slow DC tracker** (`FREQ_ALPHA = 0.0005`) that absorbs residual
  carrier offset (tuning error, receiver ppm) so only the FSK swing
  remains — this is what lets a carrier sit off-center going through the
  DDC;
- per-bit **integrate-and-dump** with zero-crossing timing recovery
  (`TIMING_GAIN = 0.10`) at the 100 Bd clock;
- mark (positive discriminator) / space slicing → one bit per symbol.

`pack_codes(bits, bit_phase)` packs the bit stream into 7-bit CCIR 476
codes LSB-first (via `ccir476::pack_bits`), starting at `bit_phase`; the
channel decoder tries all seven alignments and keeps the one whose stream
decodes to the most fully-recovered framed message. `level_dbfs()`
reports smoothed channel power.

## CCIR 476 character decode (`ccir476.rs`)

The 4-of-7 constant-ratio code: every valid word has exactly four mark
(1) bits and three space (0) bits, giving single-error **detection** (any
single flip moves the population count off four).

- **Bit packing** is **LSB-first** from seven bit-decisions: bit *i* is
  set when symbol *i* is a mark (`code |= (bits[i] > 0) << i`,
  `pack_bits`). This matches fldigi `bytes_to_code`.
- **Validity / parity**: `is_valid_code(code) == (code.count_ones() == 4)`
  — fldigi `check_bits`.
- **Alphabet**: two 128-entry tables `CODE_TO_LTRS` / `CODE_TO_FIGS` map a
  code to its letters-shift or figures-shift glyph (`'_'` = no glyph in
  that shift). `\r` `\n` are real carriage-return / line-feed; FIGS has
  `\x07` BELL. Both tables are typed **verbatim** from the oracles and
  agree on every printable glyph.
- **Shift / control codes** (named constants):

  | Name | Code | Meaning |
  |---|---|---|
  | `CODE_LTRS` | `0x5A` | switch to letters shift |
  | `CODE_FIGS` | `0x36` | switch to figures/symbols shift |
  | `CODE_ALPHA` | `0x0F` | phasing signal 1 ("alpha"); DX-channel idle |
  | `CODE_REP` | `0x66` | phasing signal 2 ("rep"); RX-channel idle |
  | `CODE_BETA` | `0x33` | "beta" idle (ARQ repeat-request; idle here) |
  | `CODE_CHAR32` | `0x6A` | "char 32" / unperforated-tape idle |

- `decode(code, figs_shift) -> Decoded` returns `Ltrs` / `Figs` / `Alpha`
  / `Rep` / `Idle` (beta, char32) for controls regardless of shift,
  `Char(glyph)` for a glyph present in the active table, else
  `Unmapped(code)` (a valid 4-of-7 word with no glyph in that shift). The
  caller drives the LTRS/FIGS state machine off the `Ltrs`/`Figs`
  variants.

## FEC-B time-diversity (`fec.rs`)

SITOR-B sends every character twice over two interleaved channels. In the
received stream the two copies alternate; once phased, **DX (the "alpha"
copy) sits on odd slots and RX (the "rep" copy) on even slots**. The RX
copy is broadcast **first**; the DX copy of the *same* character follows
**five interleaved symbol-slots later** (`FEC_DISTANCE = 5`, i.e. fldigi's
`fec_offset(pos) = pos − 35` = minus five 7-bit chars).

- **Per-character recovery** (`recover(dx, rx)`, CCIR 476 §B /
  fldigi `process_bytes`):
  1. if the DX copy is a valid 4-of-7 code → use DX (`CharSource::Dx`);
  2. else if the RX copy (five slots earlier) is valid → use RX
     (`CharSource::Rx`);
  3. else the position is unrecoverable (`CharSource::Lost`, `code:
     None`).
  DX wins whenever it is valid even if RX differs.
- **Phasing sync** (`find_phase`, models fldigi `find_alpha_characters`):
  the first DX slot lies within the first `FEC_DISTANCE * 2` positions.
  Each candidate offset is scored by valid DX codes that step every other
  slot, plus a bonus for DX codes whose RX copy five slots earlier
  matches; phasing pairs (`ALPHA`/`REP`) are explicitly excluded from the
  match count to avoid a false lock. Requires at least one real repeat
  before returning an offset.
- **Stream recovery** (`recover_stream`): from `first_dx`, step the DX
  lattice by 2, pairing each DX with its RX copy at `p − 5`; emit one
  `Recovered { code, source }` per DX position.
- **Text build** (`codes_to_text`): walks the recovered codes, tracks the
  LTRS/FIGS shift, drops `Alpha`/`Rep`/`Idle` phasing, and emits glyphs.
  `Lost` and `Unmapped` positions become `*` unless `drop_lost = true`
  (the public `decode_symbols` path drops them).

## Message framing (`message.rs`)

Frame layout per the IMO NAVTEX Manual (MSC.1/Circ.1403), mirrored by
fldigi `ccir_message`:

```
ZCZC B1 B2 B3 B4 <CR><LF> ...message text... <CR><LF> NNNN
```

- `parse(stream)` finds `ZCZC`, then expects a space + `B1 B2 B3 B4` where
  **B1** (station) and **B2** (subject) are alphanumeric and **B3 B4** are
  digits (fldigi `detect_header`). On a match it sets `station`,
  `subject`, the human-readable `subject_category`, and `message_number =
  B3·10 + B4` (0..=99), and marks `header_ok`. The body begins after the
  six header chars, skipping any leading CR/LF/space.
- The `NNNN` end marker (`detect_end`) is stripped from the body and sets
  `end_ok`.
- Parsing is **tolerant**: leading phasing garbage before `ZCZC` is
  ignored; a malformed header returns the whole stream as `text` with
  `header_ok = false`; a missing `NNNN` leaves `end_ok = false`.
- `normalize_text` collapses CR/LF runs to a single `\n` and space runs to
  a single space and trims (fldigi `ccir_message::cleanup`).

**Output** `NavtexMessage` (serde, `to_json()`): `station`, `subject`,
`subject_category`, `message_number` (each skipped when `None`), `text`,
`header_ok`, `end_ok`.

### B2 subject-indicator categories

`subject_category(b2)` — the IMO table as transcribed in fldigi
`ccir_message::msg_type` (uppercased):

| B2 | Category | B2 | Category |
|---|---|---|---|
| A | Navigational warning | L | Navigational warnings (additional) |
| B | Meteorological warning | T | Test transmissions (UK only) |
| C | Ice report | V | Notice to fishermen (U.S. only) |
| D | Search & rescue info, pirate warnings | W | Environmental (U.S. only) |
| E | Meteorological forecast | X | Special services (IMO NAVTEX Panel) |
| F | Pilot service message | Y | Special services (IMO NAVTEX Panel) |
| G | AIS message | Z | No message on hand |
| H | LORAN message | I | Not used |
| J | SATNAV messages | (other) | Unknown / invalid subject |
| K | Other electronic navaid messages | | |

## Validation / oracles

The decode layer verifies against **external** references only — no
encode→decode self-loopback. Three independent oracles back the facts;
the IQ front end adds a real off-air capture plus a synthetic
modulate→demod path.

| Layer | Fact / table | Oracle | How verified |
|---|---|---|---|
| CCIR 476 alphabet | `CODE_TO_LTRS`/`CODE_TO_FIGS`, control codes, LSB-first packing, 4-of-7 parity | **fldigi** `src/navtex/navtex.cxx` (`code_to_ltrs`/`code_to_figs`/`bytes_to_code`/`check_bits`) **and** **pd0wm/navtex** `navtex.py` (`ALPHABET_LTRS`/`ALPHABET_FIGS`) | two independent tables compared programmatically — agree on **every** printable glyph; unit tests pin exact hex (`known_letter_codes`/`known_figure_codes`) and re-assert every glyph code is 4-of-7 (`glyph_codes_are_constant_ratio`) |
| FEC-B diversity | RX-first / DX-five-chars-later interleave, DX-preferred recovery, `FEC_DISTANCE = 5` | **fldigi** (`process_bytes`/`find_alpha_characters`/`fec_offset = pos − 35`) + **arachnoid.com/JNX** SITOR-B doc | `fec::decodes_nautical_example` feeds the published **NAUTICAL** interleave (DX 'N' at slot 9, RX 'N' at slot 4 — five slots apart) and asserts the output is `"NAUTICAL"` |
| Frame layout | `ZCZC B1B2B3B4 … NNNN`, B1/B2/B3B4 fields, B2 subject table | **IMO NAVTEX Manual** (MSC.1/Circ.1403) via fldigi `ccir_message` (`detect_header`/`detect_end`/`msg_type`) | `subject_categories_match_imo_table` and header/end-marker parse tests |

- **Real off-air capture (primary front-end proof).** SDRplay's official
  `navtex.zip` IQ demo (USCG, 2020-09-04), 62.5 kS/s cs16, capture center
  516 kHz / NAVTEX channel 518 kHz, run through `NavtexChannelDecoder`
  (`offset_hz = +2000`). It decodes the **real USCG message
  char-identical to the fldigi/YaND ground truth**: **29 frames**, CI
  floor 25 (`bench/baselines.json` `navtex_offair`, `bench/run.sh`). The
  fixture `navtex_62500.cs16` is a CI-gated release asset (~74 MB, not
  vendored). This capture is what proves the narrow-passband DDC + FSK
  demod actually work on air, not just on synthetic IQ.
- **Synthetic modulate→demod path** (`*_synth_iq` tests in
  `tests/end_to_end.rs`, `modulate.rs`): builds the on-air 100 Bd ±85 Hz
  FSK waveform for a known spec-derived frame and runs it through the real
  `NavtexChannelDecoder` (DDC at a carrier offset + discriminator + timing
  + packing + decode core), asserting the recovered station/subject/
  serial/body. The modulator is **not** an external reference — it only
  exercises the front end; the CCIR 476 symbol codes it carries are still
  oracle-anchored, and the waveform parameters are the published spec.
- **Spec-derived symbol-stream end-to-end** (`decodes_full_navtex_message`):
  assembles a full interleaved DX/RX stream for `ZCZC CA23 … NAVAREA
  WARNING … NNNN` from (1) the oracle CCIR 476 code per char and (2) the
  externally-documented interleave (RX at slot 2k, DX at slot 2k+5), then
  decodes it through the crate's **independent** table and diversity logic
  — asserting station `C`, subject `A`, number 23, body, and JSON shape.
  Spec-anchored, not a private-encoder loopback (documented as such in the
  test header and `PROVENANCE.md`).
- **FEC-B proof** (`fec_b_recovers_corrupt_dx_via_rx`): smashes **every**
  DX copy to an invalid 3-of-7 code so only the time-diverse RX copies can
  reconstruct the message — proving the diversity is actually doing the
  work, not a clean DX pass-through.
- **Auto-phase** (`auto_phase_lock_decodes_message`): prepends extra
  phasing symbols and lets `find_phase` locate the alignment.
- **FIGS shift** (`figures_shift_in_body`): a body with digits exercises
  the LTRS↔FIGS state machine end-to-end (`LAT 50 LON 10`).
- **Bus mapping** (`to_message_emits_navtex_body_from_synth_iq`): confirms
  `Mode::Navtex`, `MessageBody::Navtex { kind, details }`, `crc_ok` on a
  fully framed message, RSSI, and `raw` symbols.

The full-message symbol vector is spec-derived (no public NAVTEX
symbol-stream-plus-ground-truth vector exists); the NAUTICAL example is
the externally-published worked case anchoring the diversity logic; the
SDRplay capture is the real off-air ground truth for the whole chain.

## DSP dependency: narrow-passband DDC

The off-air decode is only possible because of a fix in `xng-dsp`'s DDC
(`crates/xng-dsp/src/ddc.rs`). NAVTEX's channel ratio is degenerate —
`CHANNEL_RATE / CHANNEL_PASSBAND_HZ = 4800 / 250 ≈ 19` — and with only
the anti-alias tap count the narrow 250 Hz cutoff fell inside the final
filter's own transition roll-off, attenuating the signal so the demod saw
**0 frames** through the DDC. The DDC now sizes its **final stage** so the
transition is ≤ `passband`, keeping `[0, passband]` flat, **gated to
`out ≥ 12·passband`** so it touches only these degenerate narrow modes
(NAVTEX, DSC) and leaves every already-validated wider mode's filter and
selectivity untouched.

## Known limitations / deferred

- **No NAVTEX-specific carrier search.** The front end relies on the DDC
  `freq_offset_hz` plus the demod's slow DC tracker to center the FSK
  swing; there is no automatic channel/carrier acquisition. The off-air
  fixture works with the known +2 kHz offset.
- **One vendored real fixture.** Off-air validation rests on the single
  SDRplay USCG capture (a CI-gated release asset, not vendored in-tree);
  the rest is the spec-derived end-to-end vector, the NAUTICAL example,
  and the synthetic IQ path. No second independent off-air recording is
  in CI yet.
- **Single-error detection only.** The CCIR 476 constant-ratio code
  *detects* a single bit flip (population count ≠ 4) but cannot correct
  within one copy; correction comes only from the FEC-B time diversity
  (the other copy). A position lost in both copies is dropped (or rendered
  `*`).
- **No position output.** NAVTEX carries no fix, so a decoded message has
  no map location — it does not appear on the dashboard "beacons" layer
  (that is for radiosonde/ADS-L/SARSAT/DSC positions). NAVTEX surfaces as
  a text/message record only.

## Gotchas

1. Bit packing is **LSB-first** (`code |= bit_i << i`), matching fldigi.
2. A valid code word is **exactly** four mark bits — not "at least".
3. FEC distance is **five 7-bit chars** = 35 bit periods; in the
   interleaved slot lattice that is `FEC_DISTANCE = 5` slots, DX five
   slots after its RX copy.
4. `find_phase` excludes phasing pairs (`ALPHA`/`REP`) from its match
   score, or it would false-lock on the idle preamble.
5. The channel decoder tries **all seven** 7-bit packing alignments and
   keeps the one decoding the longest framed message; do not assume a
   fixed bit phase.
6. `demod_fsk(iq, sample_rate, bit_phase)` asserts `sample_rate ==
   CHANNEL_RATE` — wideband IQ must go through `NavtexChannelDecoder`
   (which owns the DDC), not straight into the FSK demod.
7. The DDC's narrow-passband fix (final-stage taps gated to
   `out ≥ 12·passband`) is load-bearing for NAVTEX; without it the demod
   sees an attenuated signal and decodes 0 frames.

## Key references

- **fldigi** (W1HKJ, GPL) `src/navtex/navtex.cxx` — CCIR 476 alphabet,
  bit packing, 4-of-7 parity, FEC-B diversity, frame/header/end detection,
  subject-category table (facts only; no code copied).
- **pd0wm/navtex** (MIT) `navtex.py` — second independent CCIR 476
  alphabet, cross-checked against fldigi.
- **arachnoid.com / JNX** SITOR-B documentation — the published NAUTICAL
  interleave worked example.
- **ITU-R M.476 (CCIR 476)** and **ITU-R M.625** — SITOR / NAVTEX code and
  B-mode (FEC) definition.
- **IMO NAVTEX Manual** (MSC.1/Circ.1403) — `ZCZC B1B2B3B4 … NNNN` frame
  layout and B2 subject-indicator categories.
- **SDRplay** official `navtex.zip` IQ demo — the real off-air USCG
  capture used as the front-end ground truth (`bench/data/navtex_62500.cs16`,
  release asset).
- `crates/xng-mode-navtex/PROVENANCE.md` — sourcing policy and per-table
  oracle notes.
- `docs/notes/BENCHMARKS.md` — off-air decode results vs oracles.
