# NAVTEX (SITOR-B / CCIR 476) — implementation notes

Native NAVTEX message decode core for `xng-mode-navtex`. NAVTEX is the
international maritime safety-information broadcast on 518 kHz (English),
490 kHz (national) and 4209.5 kHz (tropical/HF); on air it is 100-baud
narrow-shift (±85 Hz) FSK carrying the **CCIR 476** seven-bit
constant-ratio code in collective B-mode (**FEC-B**): every character is
sent twice with time diversity. Clean-room: no decoder was copied or
ported — only protocol facts, code tables, and one published worked
example, each cited (see `PROVENANCE.md`). This crate is the **verified
decode layer only** — symbols → message. The IQ→symbols FSK front end is
a documented, deliberately-unimplemented TODO (it can't be verified
without a published IQ-plus-ground-truth pair). All tests are anchored to
an **external** oracle, never an encode→decode self-loopback.

Status: **DECODE-CORE.** The crate is a standalone library (a `crates/*`
workspace member) exposing `decode_symbols` → `NavtexMessage`. It is
**not** wired into the runtime mode registry: there is no `Mode::Navtex`,
no `xng_types::Message` variant, and no `--mode navtex` CLI path yet. It
does not consume IQ — see Limitations.

## Pipeline

```
(IQ → symbols)        ← TODO, demod_fsk() returns DemodNotImplemented
interleaved CCIR 476 symbol stream (one 7-bit code per element)
  → fec::find_phase        locate the first DX slot
  → fec::recover_stream    DX/RX time-diversity per character
  → fec::codes_to_text     LTRS/FIGS shift tracking, drop phasing/idle
  → message::parse         ZCZC B1B2B3B4 header / body / NNNN end
  → message::NavtexMessage (serde JSON)
```

`decode_symbols(symbols, first_dx)` (in `lib.rs`) is the one entry point:
each element of `symbols` is one packed 7-bit CCIR 476 code. If
`first_dx` is `None` it phase-locks via `find_phase`; returns `None` only
when the stream is too short to lock.

`params` carries the on-air constants for a future front end: `BAUD =
100.0`, `SHIFT_HZ = 85.0`, `BITS_PER_SYMBOL = 7`, and the three carrier
frequencies (518/490/4209.5 kHz). They are informational today.

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

This crate verifies against **external** references only — no
encode→decode self-loopback. Three independent oracles back the facts:

| Layer | Fact / table | Oracle | How verified |
|---|---|---|---|
| CCIR 476 alphabet | `CODE_TO_LTRS`/`CODE_TO_FIGS`, control codes, LSB-first packing, 4-of-7 parity | **fldigi** `src/navtex/navtex.cxx` (`code_to_ltrs`/`code_to_figs`/`bytes_to_code`/`check_bits`) **and** **pd0wm/navtex** `navtex.py` (`ALPHABET_LTRS`/`ALPHABET_FIGS`) | two independent tables compared programmatically — agree on **every** printable glyph; unit tests pin exact hex (`known_letter_codes`/`known_figure_codes`) and re-assert every glyph code is 4-of-7 (`glyph_codes_are_constant_ratio`) |
| FEC-B diversity | RX-first / DX-five-chars-later interleave, DX-preferred recovery, `FEC_DISTANCE = 5` | **fldigi** (`process_bytes`/`find_alpha_characters`/`fec_offset = pos − 35`) + **arachnoid.com/JNX** SITOR-B doc | `fec::decodes_nautical_example` feeds the published **NAUTICAL** interleave (DX 'N' at slot 9, RX 'N' at slot 4 — five slots apart) and asserts the output is `"NAUTICAL"` |
| Frame layout | `ZCZC B1B2B3B4 … NNNN`, B1/B2/B3B4 fields, B2 subject table | **IMO NAVTEX Manual** (MSC.1/Circ.1403) via fldigi `ccir_message` (`detect_header`/`detect_end`/`msg_type`) | `subject_categories_match_imo_table` and header/end-marker parse tests |

- **End-to-end** (`tests/end_to_end.rs`): assembles a full on-air-shaped
  interleaved DX/RX stream for `ZCZC CA23 … NAVAREA WARNING … NNNN` from
  (1) the oracle CCIR 476 code per char and (2) the externally-documented
  interleave (RX at slot 2k, DX at slot 2k+5), then decodes it through the
  crate's **independent** table and diversity logic — asserting station
  `C`, subject `A`, number 23, body `NAVAREA WARNING`, and the JSON shape.
  Because the stream is built from external facts and the decode path is
  independent, this is **spec-anchored, not a private-encoder loopback**
  (documented as such in the test header and `PROVENANCE.md`).
- **FEC-B proof** (`fec_b_recovers_corrupt_dx_via_rx`): smashes **every**
  DX copy to an invalid 3-of-7 code so only the time-diverse RX copies can
  reconstruct the message — proving the diversity is actually doing the
  work, not a clean DX pass-through.
- **Auto-phase** (`auto_phase_lock_decodes_message`): prepends extra
  phasing symbols and lets `find_phase` locate the alignment.
- **FIGS shift** (`figures_shift_in_body`): a body with digits exercises
  the LTRS↔FIGS state machine end-to-end (`LAT 50 LON 10`).

No public NAVTEX symbol-stream-plus-ground-truth vector was found, so the
full-message vector is spec-derived (and documented as such); the
NAUTICAL example is the externally-published worked case that anchors the
diversity logic.

## Known limitations / deferred

- **No IQ front end.** `demod_fsk(iq, sample_rate)` is a documented
  placeholder that returns `NavtexError::DemodNotImplemented`. The
  intended contract (100-baud ±85 Hz FSK discriminator + bit-timing
  recovery → one CCIR 476 code per 7 bits) is described but unbuilt: an IQ
  demod cannot be verified without a published IQ capture paired with
  ground-truth text, so per the crate's verification rules it is left a
  TODO rather than shipped unverified. The decode layer above is fully
  testable from a symbol stream and is the verified deliverable.
- **Not wired into the runtime.** No `Mode::Navtex`, no
  `xng_types::Message` variant, no `--mode navtex`. The crate stands alone
  and emits its own `NavtexMessage`; integrating it requires the IQ front
  end first.
- **Single-error detection only.** The CCIR 476 constant-ratio code
  *detects* a single bit flip (population count ≠ 4) but cannot correct
  within one copy; correction comes only from the FEC-B time diversity
  (the other copy). A position lost in both copies is dropped (or rendered
  `*`).
- **No off-air fixture in CI.** Validation is the spec-derived
  end-to-end vector plus the published NAUTICAL example; there is no
  vendored IQ recording (none with ground truth was found).

## Gotchas

1. Bit packing is **LSB-first** (`code |= bit_i << i`), matching fldigi.
2. A valid code word is **exactly** four mark bits — not "at least".
3. FEC distance is **five 7-bit chars** = 35 bit periods; in the
   interleaved slot lattice that is `FEC_DISTANCE = 5` slots, DX five
   slots after its RX copy.
4. `find_phase` excludes phasing pairs (`ALPHA`/`REP`) from its match
   score, or it would false-lock on the idle preamble.
5. The IQ demod is intentionally a TODO — `demod_fsk` errors by design.

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
- `crates/xng-mode-navtex/PROVENANCE.md` — sourcing policy and per-table
  oracle notes.
