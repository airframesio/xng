# Provenance — xng-mode-navtex

Clean-room implementation of the NAVTEX (SITOR-B / CCIR 476) message and
FEC-B decode layer. No decoder code was copied or ported; only protocol
facts, code tables, and a published worked example were used, each cited
below. Every test is anchored to an **external** reference — none is an
encode→decode self-consistency loopback.

## What this crate is

NAVTEX is the international maritime safety-information broadcast (518 kHz
English, 490 kHz national, 4209.5 kHz tropical), 100-baud ±85 Hz FSK
carrying the CCIR 476 seven-bit constant-ratio code in collective B-mode
(FEC-B): each character is transmitted twice with time diversity.

This crate implements the **verified decode layer** — symbols → message:

- `ccir476` — the 4-of-7 constant-ratio alphabet (LTRS/FIGS), LSB-first
  bit packing, constant-ratio parity check.
- `fec` — FEC-B time diversity (DX preferred, RX fallback five chars
  earlier) and phasing sync.
- `message` — `ZCZC B1B2B3B4` header parse, body, `NNNN` end, JSON.

The IQ→symbols FSK front end (`demod_fsk`) is a **documented TODO**: it
cannot be externally verified without a published IQ capture paired with
ground-truth text, so per the verification rules it is left unimplemented
rather than shipped unverified.

## Sources (protocol facts / tables / worked example only)

### CCIR 476 alphabet — TWO independent oracles, cross-checked

The `CODE_TO_LTRS` / `CODE_TO_FIGS` tables and the control codes
(`LTRS=0x5a`, `FIGS=0x36`, `ALPHA=0x0f`, `REP=0x66`, `BETA=0x33`,
`CHAR32=0x6a`) and the LSB-first bit packing (`code |= (bit_i) << i`) are
taken verbatim from, and cross-verified between:

- **fldigi** `src/navtex/navtex.cxx` (GPL) — `code_to_ltrs`,
  `code_to_figs`, `bytes_to_code`, `check_bits` (4-of-7 validity).
  Fetched via `gh api repos/w1hkj/fldigi/contents/src/navtex/navtex.cxx`.
- **pd0wm/navtex** `navtex.py` (MIT) — `ALPHABET_LTRS`, `ALPHABET_FIGS`.
  Fetched via `gh api repos/pd0wm/navtex/contents/navtex.py`.

The two tables were compared programmatically: they agree on **every**
printable letter, digit, symbol, and space. The crate's
`glyph_codes_are_constant_ratio` test re-asserts that every glyph code is a
valid 4-of-7 word; `known_letter_codes` / `known_figure_codes` pin the
exact hex values to the oracle tables.

### FEC-B time diversity — fldigi + arachnoid.com/JNX

The diversity rule (RX/rep copy sent first; DX/alpha copy of the same
character five characters later) and the recovery policy (use DX if valid,
else fall back to the RX copy) follow:

- **fldigi** `navtex_implementation::process_bytes` /
  `find_alpha_characters` / `fec_offset(pos) = pos - 35` (35 bits = five
  7-bit chars).
- **arachnoid.com/JNX** SITOR-B documentation, which gives the canonical
  worked example: the word **NAUTICAL** interleaves as
  `rep alpha rep alpha N alpha A alpha U N T A I U C T A I L C _ A _ L`.

The `fec::decodes_nautical_example` test feeds exactly that published
interleave (DX 'N' at slot 9, RX 'N' at slot 4 — five slots apart) and
asserts the output is "NAUTICAL". This anchors the FEC distance, the
DX/RX selection, and the shift-aware text builder to an external example,
not to this crate's own encoder.

### NAVTEX frame layout — IMO NAVTEX Manual (via fldigi)

The `ZCZC B1B2B3B4 <CR><LF> ... <CR><LF> NNNN` structure, the B1 (station)
/ B2 (subject indicator) / B3B4 (two-digit serial) header fields, the
`NNNN` end marker, and the B2 subject-category table (A = navigational
warning, B = meteorological warning, ... Z = no message) follow the IMO
NAVTEX Manual (MSC.1/Circ.1403) as transcribed in fldigi
`ccir_message::detect_header`, `detect_end`, and `msg_type`.

## End-to-end test vector — spec-derived, documented as such

No public symbol-stream-plus-ground-truth NAVTEX vector was found, so the
`tests/end_to_end.rs` full-message vectors are **spec-derived**: the
interleaved DX/RX symbol stream is assembled from (1) the oracle CCIR 476
code for each character and (2) the externally-documented interleave rule
(RX first, DX five slots later). The decoder then re-derives the text via
its own independent table and diversity logic. Because the stream is built
from external facts and the decode path is independent, this is anchored
to the spec rather than being a private-encoder loopback. The
`fec_b_recovers_corrupt_dx_via_rx` test additionally smashes every DX copy
so only the time-diverse RX copies can reconstruct the message — proving
the FEC-B diversity is actually doing the work.
