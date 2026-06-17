# Digital Selective Calling (DSC) — implementation notes

Native maritime DSC (ITU-R M.493 / M.541, on the CCIR 493 10-unit
alphabet) decode core for `xng-mode-dsc`. DSC is the calling and
distress-alerting layer of the GMDSS, carried by FSK on MF/HF (170 Hz
shift, 100 Bd) and VHF (1300/2100 Hz AFSK, 1200 Bd). Clean-room: protocol
facts come from ITU-R M.493/M.541; the bit→symbol and symbol→message
layers are pinned to the published off-air unit-test vectors of the MIT
`TAOSW.DSC_Decoder` reference decoder — no decoder code was copied (see
PROVENANCE.md).

**Status: DECODE-CORE.** The symbol layer (10-bit CCIR 493 decode +
embedded zero-count check + DX/RX time-diversity de-interleave) and the
message layer (format-driven field parse to a structured `DscMessage`
→ JSON) are implemented and oracle-validated against real off-air
sequences. The IQ→bits FSK front end (`demod.rs`) is a documented,
typed stub — there is **no demod and no `--mode dsc`** dispatch yet, and
DSC has no entry in `xng_types::Message` or the mode registry. The crate
is self-contained: `decode_from_bits(&[u8])` is the intended entry point
once a demod lands. Source: `crates/xng-mode-dsc/src/`.

## Pipeline

synchronised FSK bit stream → `symbol::decode_bitstream` (slice into
10-bit CCIR 493 symbols, verify the embedded check, emit value or
`ERASURE`) → `symbol::deinterleave_dx_rx` (split DX/RX, drop phasing,
recover erased DX symbols from the time-shifted RX repeat) →
`message::decode` (format specifier → per-format field parse) →
`DscMessage` → `DscMessage::to_json`. The convenience wrapper
`decode_from_bits` runs this whole chain with the standard diversity
geometry (6 leading DX phasing characters; RX repeat trailing by 2).

There is **no IQ stage**: `demod::demodulate_iq` always returns `None`
and carries a placeholder `Complex32` so it compiles without an IQ
dependency, exactly so the verified decode layers can stand alone.

## Symbol layer (`symbol.rs`)

A DSC character is a **10-bit symbol**: 7 information bits **B1..B7 sent
LSB-first**, then a **3-bit check field sent MSB-first**. The check field
is the **count of "B" (binary-0) elements among the 7 information bits**
(`zero_count`), so every symbol carries its own integrity check — a
received symbol is valid iff `received_check == zero_count(value)`. This
is the CCIR 493 10-unit error-detecting code; it is a per-symbol detect,
not a correct.

- `decode_symbol(&[u8]) -> (u8, bool)` — packs B1..B7 LSB-first into the
  7-bit value, reassembles the 3 check bits MSB-first, returns the value
  and whether the zero-count check passed.
- `decode_bitstream(&[u8]) -> Vec<i32>` — walks the stream 10 bits at a
  time; symbols that fail the check are emitted as `ERASURE` (`-1`)
  rather than a guessed value.

### DX/RX time diversity (`deinterleave_dx_rx`)

Every character is transmitted twice with time diversity: the **DX**
(data) stream carries each character once; the **RX** (repetition) stream
repeats it later, so a character corrupted in one stream can be recovered
from the other. On the wire the streams interleave DX, RX, DX, RX, …
(DX = even index, RX = odd). The decoder splits them, skips `dx_skip`
leading DX phasing/format-setup characters, then takes the DX data
character by character — **falling back to the time-shifted RX repeat at
`rx_idx = k + rx_offset` only when the DX character is an erasure**. If
both the DX character and its RX repeat are erased, the position stays an
`ERASURE` (never guessed). The standard geometry used by `decode_from_bits`
is `dx_skip = 6`, `rx_offset = 2`.

## Message layer (`message.rs`)

`decode(&[i32]) -> DscMessage` reads the **format specifier** (leading
symbol, sent twice — uses the first copy, or the second if the first is
erased) and dispatches by format. Every field is read positionally by
symbol index; erasures in addressing/position fields surface as
placeholders rather than being dropped.

### Format specifier (leading symbol)

| Symbol | Format | Decoded? |
|---|---|---|
| 112 | `distress_alert` | yes — full body |
| 116 | `all_ships_call` | yes — full body |
| 120 | `individual_station_call` | yes — full body |
| 102 | `geographic_area_group_call` | yes — full body |
| 114 | `group_call` | format + self-id MMSI only; `status = "Unsupported"` |
| 123 | `automatic_service_call` | format + self-id MMSI only; `status = "Unsupported"` |
| other | `unknown` | format only; `status = "Unsupported"` |

### Field layouts by format (symbol indices into the recovered stream)

The format specifier occupies indices 0–1 (sent twice). Subsequent
indices below are into the de-interleaved symbol vector.

| Format | To | Category | From | TC1 | TC2 | Position / Freq / Nature | Time | EOS | ECC |
|---|---|---|---|---|---|---|---|---|---|
| **distress** (112) | "ALL SHIPS" | forced `distress` | MMSI @2 (5 syms) | — | — | nature @7; position @8 (5 syms) | @13 (2 syms) | @16 | @17 |
| **all-ships** (116) | "ALL SHIPS" | @2 | MMSI @3 | @8 | @9 | freq @10 if TC1=J3E-TP | — | @16 | @17 |
| **individual** (120) | MMSI @2 | @7 | MMSI @8 | @13 | @14 | switch on sym @15 (below) | — | @21 | @22 |
| **area** (102) | area @2 (5 syms) | @7 | MMSI @8 | @13 | @14 | freq @15 if TC1=J3E-TP | — | @21 | @22 |

Individual-call body switch on symbol @15:
`55` → position field at @16; `126` → `nature_description = "Position
Requested"` (no position/freq); anything else → frequency/channel field
at @15.

### Enumerations (symbol → variant)

- **Category** (`Category::from_symbol`): 100 routine, 108 safety,
  110 urgency, 112 distress, else unknown.
- **Nature of distress** (distress format only, 11 values): 100 fire/
  explosion, 101 flooding, 102 collision, 103 grounding, 104 listing/
  capsizing, 105 sinking, 106 disabled & adrift, 107 undesignated,
  108 abandoning ship, 109 piracy/armed-robbery, 110 man overboard,
  else unknown.
- **First telecommand TC1** (14 named): 100 all-modes-TP, 101 duplex-TP,
  103 polling, 104 unable-to-comply, 105 end-of-call, 106 data,
  109 J3E-TP, 110 distress-acknowledgement, 112 distress-alert-relay,
  113 TTY-FEC, 115 TTY-ARQ, 118 test, 121 ship-position/registration,
  126 no-information, else unknown.
- **Second telecommand TC2** (21 named): 100 no-reason-given through
  the channel/mode/comply reasons and the medical-transports /
  pay-phone / facsimile values, plus the ACS sequential-transmission
  remaining-count set (120–125 = 0..5 times) and 126 no-information,
  else unknown.
- **End-of-sequence EOS** (`extract_eos`): 117 Acknowledge-RQ (requires
  ack), 122 Acknowledge-BQ (answer to such a call), 127 other-calls,
  else unknown. EOS is read from the DX/RX positions of the 4-symbol EOS
  field, taking the first non-erased of symbols 1, 3, 4 of the field.

### Address / coordinate field decoding

- **MMSI** (`extract_mmsi`, 5 symbols): two decimal digits per symbol,
  concatenated to 10 digits, then the **trailing 10th digit is dropped**
  (it is filler), giving the 9-digit MMSI. Erased symbols become `__`,
  so a partly-recovered MMSI reads e.g. `2491____0`.
- **Position** (`extract_position`, 5 symbols → 10 digits): quadrant
  digit + lat (deg, min) + lon (deg, min). Quadrant 0/1/2/3 →
  N/E, N/W, S/E, S/W. Rendered `"DD MMx DDD MMx"` (e.g.
  `45 26N 013 07E`). Any erasure → `--error--`.
- **Geographic area** (`extract_geographic_area`, 5 symbols): quadrant +
  reference-point lat/lon + rectangle vertical/horizontal extents,
  rendered as a human-readable string (e.g. `North-East (NE), Reference
  point: 44°, 3°, Vertical side: 5°, Horizontal side: 8°`). Any erasure
  → `--error--`.
- **Time** (`extract_time`, 2 symbols): UTC `HH:MM`; `None` if either
  half is erased or out of range (HH > 23 / MM > 59).

### Frequency / working-channel field (`extract_frequencies`, 6 symbols)

The first digit selects the encoding:

| First digit | Encoding | Status |
|---|---|---|
| 0 / 1 / 2 | MF/HF frequency in 100 Hz multiples (one or two frequencies) | decoded, oracle-pinned |
| 9 then 0 | VHF channel pair | decoded, oracle-pinned |
| 3 | MF/HF working channel | `--not implemented--` (M.493-defined, not externally pinned) |
| 4 | 10 Hz multiples | `--not implemented--` |
| 8 | VHF automated systems | `--not implemented--` |
| erased | — | `--error--` |

- MF/HF (`mf_hf_100hz`): 12 digits → `DDDDD.D` MHz-style rendering; a
  second frequency whose 3 symbols are all > 99 (the 126/126/126
  "absent" marker) means "same as / no second frequency" and only one is
  shown — e.g. `04101.0/04393.0` (pair) vs `08414.5` (single).
- VHF (`vhf_channels`): channel-type nibble (1/2 → simplex, 0 → duplex)
  + 3-digit channel for each of two channels — e.g. `Duplex channel 749
  - Duplex channel 225`.

### Error-check character (ECC) — expansion / vertical parity

The ECC is the last information character. Its 7 information bits are the
**modulo-2 sum (even vertical parity)** of the corresponding bits of all
information characters. `validate_ecc` XORs together the bits of symbols
**`[1 .. ecc_pos)`** — i.e. it **excludes the duplicate leading format
specifier** (index 0) **and the ECC itself** — and compares the 7-bit
result against the received ECC (`ecc & 0x7f`). `status` is `"OK"` on a
match, `"Error"` otherwise. An **erased information character or an
erased ECC makes the result unverifiable → `"Error"`** (never asserted
OK). `ecc` is reported as the raw received value (`-1` when erased); it
is a 7-bit field so values can legitimately exceed 99.

## Output

`DscMessage` is a serde struct serialized by `to_json` to a compact
object: `symbols` (the recovered stream), `format`, `category`, optional
`to` / `from` / `tc1` / `tc2` / `nature` / `nature_description` /
`position` / `time` / `frequency`, `eos`, `ecc` (i32), and `status`
(`"OK"` / `"Error"` / `"Unsupported"`). Enum variants serialize
`snake_case`. There is **no `xng_types::Message` variant** for DSC yet —
output is the crate-local JSON only.

## Validation / oracles

The oracle is **`alemassimo/TAOSW.DSC_Decoder`** (MIT), a .NET decoder of
off-air HF DSC audio. Its unit-test vectors are real off-air sequences
(timestamped 2025-03..04, MF/HF 2187.5 / 8414.5 kHz) with a human-verified
decode written alongside each symbol stream. These are an **external
oracle, not an encode→decode loopback** — no TAOSW source was copied, only
its published vectors and the M.493 field layout it implements.

- **Symbol level** (`src/symbol.rs` tests, from
  `GMDSSDecoderTests.RetriveDataByteTest1..4`): four 10-bit symbols with
  their expected 7-bit values (2, 122, 127, 43), each also satisfying the
  embedded zero-count check — confirming the 3-bit field is the zero
  count. Plus DX/RX recovery and unrecoverable-erasure tests, and a
  deliberately-corrupted-check → erasure test.
- **Message level** (`tests/oracle_vectors.rs`, from
  `SymbolsDecoderTests`): ~16 vectors covering distress alert (incl. an
  ECC-corrupted variant → `status = "Error"`), individual-station calls
  (ack/test, J3E with frequency pair and single frequency, position-
  requested, position-follows, VHF channel pair, ship-to-ship with
  position), all-ships safety calls, geographic-area calls, and
  error/partial-recovery cases (truncated stream, TC1 erased but TC2
  recovered, MMSI with erasures → `_` placeholders). Each asserts
  Format / Category / To / From / TC1 / TC2 / Nature / Position / Time /
  Frequency / EOS / ECC / status against the human-verified decode, plus
  a JSON serialize/round-trip check.

Every decoded fact above is fixed by one of these external vectors; the
fields marked `--not implemented--` / `Unsupported` are precisely those
the oracle does not exercise.

## Known limitations / intentional gaps

- **No IQ demod, no `--mode dsc`.** `demod.rs` is a typed stub returning
  `None`; the FSK front end (MF/HF 100 Bd ±85 Hz; VHF 1200 Bd
  1300/2100 Hz; tone detection, bit-timing recovery, dot/phasing
  acquisition to align 10-bit symbol boundaries) is unwritten. Verifying
  a demod needs recorded IQ with an independently known decode; per the
  project's "never commit unverified code" rule, none is committed. The
  crate is not registered in the mode dispatch or `xng_types::Message`.
- **Frequency sub-fields 3 / 4 / 8** (MF/HF working channel, 10 Hz
  multiples, VHF automated) are M.493-defined but return
  `--not implemented--` rather than a guessed value — no external vector
  pins them.
- **Group call (114)** and **automatic service call (123)** bodies are
  not decoded (the reference decoder does not decode them either); the
  format and any recoverable self-id MMSI are surfaced and the message is
  marked `Unsupported`.
- **Per-symbol detect only.** The CCIR 493 check and ECC parity are
  detection codes; bad characters become erasures recovered (if at all)
  by DX/RX diversity — there is no symbol-level error correction beyond
  the time-diversity repeat.

## Gotchas

1. Symbol bit order is split: 7 info bits **LSB-first (B1 first)**, 3
   check bits **MSB-first**.
2. The check field is the **zero-bit count**, not parity — a symbol is
   valid iff `check == zero_count(value)`.
3. MMSI is 5 symbols → 10 digits, then **drop the trailing digit** → 9.
4. DX/RX recovery only fires on an erased DX character; a clean DX
   character is taken as-is and its RX repeat ignored.
5. ECC parity spans **`[1 .. ecc_pos)`** — skip the duplicate leading
   format specifier and exclude the ECC; an erased info char or ECC →
   `"Error"`, never `"OK"`.
6. The 126/126/126 second-frequency marker means "absent", rendered as a
   single frequency.

## Key references

- **ITU-R M.493** (DSC system for the maritime mobile service) —
  symbol alphabet, formats, category, telecommands, MMSI/address
  construction, distress nature/position/time, ECC definition.
- **ITU-R M.541** (operational procedures) — DX/RX time diversity,
  phasing, end-of-sequence.
- **CCIR 493** 10-unit error-detecting code (7 info + 3 zero-count check
  bits).
- **`alemassimo/TAOSW.DSC_Decoder`** (MIT) — external off-air oracle:
  symbol-byte and message-level unit-test vectors reproduced verbatim in
  `src/symbol.rs` and `tests/oracle_vectors.rs` (facts/vectors only, no
  code).
- `crates/xng-mode-dsc/PROVENANCE.md` — sourcing policy and per-layer
  oracle notes.
