# EOT / HOT (AAR S-9152 rail End/Head-of-Train) — implementation notes

Native rail End-of-Train / Head-of-Train telemetry decode core for
`xng-mode-eot`. North-American freight rail uses a short two-way
telemetry link between the locomotive (HOT, Head-of-Train, at the front)
and the rear-car device (EOT, End-of-Train): the EOT periodically reports
brake-pipe pressure, motion, marker-light and battery status to the head
end, and the head end can command the EOT (e.g. emergency brake). On air
it is narrowband **1200-baud binary FSK with Manchester line coding**
(two opposite chips per data bit), on two channels:

- EOT → HOT (rear-to-front telemetry): **457.9375 MHz**
- HOT → EOT (front-to-rear command):   **452.9375 MHz**

Direction (telemetry vs command) is **not** a wire field — it is chosen
by the **receive frequency**, and surfaces as the `kind` of the emitted
message (`"eot"` vs `"hot"`).

Reverse-engineered: there is **no public formal AAR standard** for this
link. The bit layout, field semantics, and the BCH check are anchored to
the field map shared byte-for-byte by two independent public decoders
(ereuter/PyEOT and russinnes/EOTDecode) plus the on-air RF facts from
SIGIDWIKI; each is cited inline below and in the source. The **decode /
framing layer** (74-bit packet → fields + BCH) is verified against a
hand-built spec packet laid out exactly per that documented field map.
The **demod** (IQ → chips) is validated **only** by a synthetic
`modulate → AWGN → demod` test — **no off-air IQ is available**, so no
real-RF claim is made.

Status: **WIRED, SYNTHETIC-ONLY.** Runtime mode `Mode::Eot`, body
`MessageBody::Eot { kind, details }`, and an `EotChannelDecoder` that owns
an `xng_dsp::Ddc`. The framing core is spec-anchored; the IQ front end is
exercised only by self-generated waveforms (no recorded capture exists).

## Pipeline

```
wideband capture IQ
  → Ddc                      mix by freq_offset_hz, decimate to CHANNEL_RATE (24 000 S/s)
  → demod::FskDemod          freq discriminator + DC tracker + 2400-chip timing → 1 chip/symbol
  → demod::manchester_decode pair chips [1,0]→1 / [0,1]→0 (both pairing phases tried)
logical bit stream
  → scan_bits                hunt 101010 + 11-bit frame sync, slice 74-bit packets
  → frame::parse_packet      frame-sync check, LSB-first field map, BCH verify
  → frame::EotFrame          (serde JSON) → to_message → xng_types::Message bus form
```

Two entry points:

- `EotChannelDecoder::new(input_rate, freq_offset_hz)` — channelized IQ
  entry (mirrors the NAVTEX `NavtexChannelDecoder` contract). `process(iq)`
  feeds the DDC + demod, accumulates the channel's Manchester **chip**
  history (EOT bursts are short, so chips are buffered and re-scanned), and
  emits an `EotDecodedFrame` per recovered packet. It tries **both**
  Manchester pairing phases (0 and 1) and lets the frame-sync hunt reject
  the wrong one. Dedups by raw packet-bit identity so a growing buffer does
  not re-emit the same packet. When `input_rate == CHANNEL_RATE` and offset
  is 0 the DDC is skipped (IQ is already channelized).
- `scan_bits(bits)` — the verified bit-stream → frames core (in `lib.rs`):
  scans a logical bit stream for the hunt pattern and decodes every aligned
  74-bit packet that follows. Returns the parsed `EotFrame`s.

`to_message(frame, frequency_hz, level_dbfs, is_hot, source)` normalizes
an `EotDecodedFrame` into the bus `Message`: `mode = Mode::Eot`, body
`MessageBody::Eot { kind, details }` where `kind` is `"hot"` when
`is_hot` else `"eot"` (caller picks it from the receive frequency),
`details` is the `EotFrame` JSON, `decode.crc_ok = frame.bch_ok`, RSSI
from the channel level, and the 74 packed packet bits travel as `raw`
(one byte per bit).

`params` carries the on-air constants (informational, SIGIDWIKI EOTD):
`BAUD = 1200.0`, `FREQ_EOT_TO_HOT = 457_937_500`,
`FREQ_HOT_TO_EOT = 452_937_500`. `CHANNEL_RATE = 24_000.0` S/s
(10 samples per Manchester chip at the 2400-chip rate) and
`CHANNEL_PASSBAND_HZ = 4_000.0` (one-sided; passes the ±FSK tones plus
the Manchester chip-rate sidebands and a realistic tuning offset while
staying inside the ~8 kHz EOT channel).

## IQ front end (`demod.rs`)

The 1200-baud Manchester-FSK demodulator, structured after the same
narrow-shift FSK chain the NAVTEX core uses but for a Manchester-coded
2-FSK signal. Input is complex channel IQ at `CHANNEL_RATE`, already mixed
to baseband by the crate's `Ddc`.

- per-sample **frequency discriminator** `arg(x · conj(x_prev))`;
- a **slow DC tracker** (`FREQ_ALPHA = 0.0005`) that absorbs residual
  carrier/tuning offset so only the FSK swing remains;
- per-**chip** integrate-and-dump at the Manchester chip rate
  (`CHIP_RATE = 2 · BAUD = 2400`) with zero-crossing timing recovery
  (`TIMING_GAIN = 0.10`); Manchester coding makes chip transitions dense,
  so the loop gets plenty of edges;
- mark (positive discriminator) / space slicing → **one chip decision per
  chip period** (1 = mark / positive freq, 0 = space / negative).

`FskDemod::new()` asserts ≥ 4 samples/chip for timing (24000/2400 = 10).
`level_dbfs()` reports smoothed channel power (`LEVEL_ALPHA = 0.002`).

`manchester_decode(chips, phase)` pairs the chip stream into logical bits
starting at chip offset `phase` (0 or 1): `[1,0] → 1`, `[0,1] → 0`. An
**ambiguous** pair (`[0,0]` or `[1,1]`, e.g. from noise) decodes to the
first chip's value as a soft fallback so the sync hunt can still slide
over it. The channel decoder runs both phases because the demod has no
absolute chip-pair alignment; only the frame-sync hunt distinguishes the
correct pairing.

## Frame sync and packet hunt (`lib.rs` / `frame.rs`)

After Manchester decode the logical stream is hunted for a **17-bit run**
`10101011100010010` (cited PyEOT/EOTDecode search):

- `BIT_SYNC_TAIL = 101010` — the 6-bit tail of the alternating bit-sync
  (clock) preamble;
- `FRAME_SYNC = 11100010010` — the 11-bit frame sync word that opens every
  packet.

The cited decoders take `buffer[6:]` after the match, so the frame sync
becomes `packet[0:11]`. `scan_bits` reproduces this: a match at index `i`
means the packet starts at `i + BIT_SYNC_TAIL.len()`; it slices the
following `PACKET_BITS = 74` bits, calls `parse_packet`, and on success
skips past the packet so it does not re-match inside it.

## AAR S-9152 packet layout (`frame.rs`)

Both cited decoders model a **74-bit packet** after Manchester decode +
bit sync:

```text
  packet[ 0:11]  frame sync word  = 11100010010   (11 bits)
  packet[11:56]  data block       (45 bits, BCH-protected)
  packet[56:74]  BCH check word    (18 bits, ciphered)
```

`DATA_START = 11`, `DATA_END = 56`, `PACKET_BITS = 74`. `parse_packet`
returns `None` if the slice is short or the frame sync word is wrong.

### Data-block field map

Bit indices into the 74-bit packet, sliced exactly as the cited decoders
slice them. **Multi-bit fields are stored LSB-first on the wire**: the
decoders reverse each slice before `int(..., 2)`, which equals reading the
original slice LSB-first — reproduced here by `field_rev`.

| Bits | Field | Notes |
|---|---|---|
| `[11:13]` | chaining | 2 bits, surfaced **raw** as `chaining` (`(p[11]<<1)\|p[12]`); meaning undocumented publicly |
| `[13:15]` | battery condition | 2 bits, reversed; 11 OK, 10 Low, 01 Very Low, 00 Not Monitored |
| `[15:18]` | message type | 3 bits; `0b111` ⇒ status/arm message |
| `[18:35]` | unit address | 17 bits, reversed → integer (the device's programmed ID) |
| `[35:42]` | brake-pipe pressure | 7 bits, reversed → psig integer |
| `[42:49]` | battery charge | 7 bits, reversed → `raw/127·100` percent (rounded) |
| `[49]` | spare | 1 bit |
| `[50]` | valve circuit / disconnect | 1 bit |
| `[51]` | conf / arm indicator | 1 bit; with message type `0b111`: 0 Arming, 1 Armed |
| `[52]` | turbine (charger running) | 1 bit |
| `[53]` | motion | 1 bit; 1 = EOT detects train motion |
| `[54]` | marker-light battery | 1 bit |
| `[55]` | marker-light status | 1 bit (on/off) |

`EotFrame` carries every field above plus `battery_condition_text`,
`bch_ok`, and (only for message type `0b111`) `arm_status`
(`"Arming"`/`"Armed"`). `arm_status` is `None` and skipped in JSON for
every other message type.

The 2 **chaining** bits (`[11:13]`) fall inside the BCH-protected data
block, but **neither cited decoder names their meaning**, so they are
surfaced raw and the gap is noted (see Known limitations).

## BCH(63,45) ciphered check (`bch.rs`)

The frame uses a systematic binary BCH(63,45) code narrowed in the field
decoders to an **18-bit check word**: modulo-2 polynomial division of the
45-bit data block by a 19-bit generator, with the remainder XOR-ed against
a fixed 18-bit "cipher" key. Both decoders agree byte-for-byte:

- `GENERATOR = 1111001101000001111` (19 bits → degree-18 generator);
- `CIPHER = 101011011101110000` (18 bits);
- `CHECK_BITS = 18`.

Verify path (`ciphered_check` / `verify`), reproducing the decoders'
arithmetic exactly:

1. reverse `packet[11:56]` to LSB-first;
2. `checkbits` = mod-2 long-division remainder by `GENERATOR` (append
   `len(gen)-1` zeros, XOR the generator in whenever the leading bit is 1,
   keep the trailing 18 bits — standard CRC-style systematic remainder,
   mirroring `helpers.checkbits` / `helpers.mod2div`);
3. XOR the remainder with `CIPHER`;
4. a frame is valid when that equals `packet[56:74]` (sets `bch_ok`).

This is **error detection only** — the data block is not corrected.

## Bus message mapping

`to_message` produces:

- `mode = Mode::Eot`;
- `body = MessageBody::Eot { kind, details }`, `kind` ∈ {`"eot"`,`"hot"`}
  from `is_hot`, `details` = `EotFrame` JSON;
- `decode.crc_ok = frame.bch_ok` (`fec_corrected`/`errors` are `None` —
  no correction is performed);
- `signal.rssi_db` from the channel `level_dbfs`;
- `raw` = the 74 packet bits, one byte per bit, rebuilt by
  `packet_bits_of` (which re-slices the parsed fields LSB-first and
  recomputes the ciphered check so `raw` round-trips through BCH verify).

## Validation / oracles

The framing/decode layer is anchored to the **documented field map**
shared by the two independent public decoders; the demod is validated
**only synthetically**. There is **no off-air IQ and no real-RF result.**

| Layer | Fact / table | Oracle | How verified |
|---|---|---|---|
| Packet layout + field map | frame sync `11100010010`, `[0:11]`/`[11:56]`/`[56:74]` split, every data-block slice, battery-condition table, arm-status, LSB-first field convention | **ereuter/PyEOT** `eot_decoder.py` and **russinnes/EOTDecode** `eot_decoder.py` (byte-for-byte identical) | `decodes_spec_field_map_and_bch` hand-builds the exact 74 bits per the documented layout and asserts the decoder recovers **every** field; `battery_condition_table_matches_cited_decoders`; `arm_status_for_status_message_type` |
| BCH check | `GENERATOR`, `CIPHER`, 18-bit ciphered remainder, the reverse→mod2div→XOR verify path | **PyEOT/EOTDecode** `helpers.checkbits`/`mod2div`; classic CRC worked examples | `generator_and_cipher_match_cited_decoders` re-derives both bit strings; `checkbits_matches_reference_mod2div` anchors the GF(2) division against the cited `mod2div` and the Wikipedia "Computation of CRC" example (`11010011101100` ÷ `1011` → `100`); `decodes_spec_field_map_and_bch` asserts the spec packet's documented check **verifies**; `bch_detects_corrupted_data_bit` flips one data bit and requires `bch_ok` to drop |
| Hunt / sync | the `10101011100010010` search run, packet offset after the 6-bit bit-sync tail | **PyEOT/EOTDecode** (`buffer[6:]` after match) | `hunt_pattern_is_cited_search_run`; `scan_finds_packet_after_bit_sync_preamble` prepends an alternating clock run and requires exactly one decoded packet |
| RF facts | 1200-baud FSK, EOT→HOT 457.9375 MHz, HOT→EOT 452.9375 MHz | **SIGIDWIKI** "End of Train Device (EOTD)" | `params_are_documented_values`; `channel_rate_is_integer_chip_multiple` |

**Spec-cited framing ground truth.** `build_spec_packet` assembles a
plausible EOT→HOT report (unit `0x1A2B3`, 75 psig, moving, marker light on,
turbine charging, battery OK) per the documented field map, computes the
documented ciphered BCH check, and asserts the crate's **independent**
field extraction and BCH verify pass together — spec-cited ground truth,
**not** a self-modulator round-trip.

**Synthetic modulate → AWGN → demod (the only demod proof).** The
`*_synth_iq` tests in `tests/end_to_end.rs` build the on-air
Manchester-FSK waveform (`modulate.rs`) for a known spec-built packet and
run it through the **real** `EotChannelDecoder` (DDC + discriminator +
chip timing + Manchester pairing + sync hunt + the verified framing core):

- `channel_decoder_recovers_clean_synth_iq` — clean IQ at `CHANNEL_RATE`,
  offset 0; recovers a BCH-clean frame, fields exact.
- `channel_decoder_recovers_with_carrier_offset_via_ddc_synth_iq` — a
  96 kS/s capture with a +12 kHz carrier offset; the DDC mixes it down and
  the DC tracker absorbs the residual tuning error.
- `synthetic_awgn_ber_frame_recovery` — modulate → complex AWGN at a
  controlled SNR → demod, over 12 LCG-seeded trials. At the chosen
  per-component `sigma = 0.18` against amplitude `0.8` (≈ 10 dB SNR) it
  requires **≥ 75 %** of trials to recover a BCH-clean, field-exact frame.
  Because the BCH check is strict, a single residual bit error fails a
  trial. These SNR/threshold numbers are **synthetic-test parameters, not
  measured off-air performance.**
- `to_message_emits_eot_body_from_synth_iq` — confirms the bus mapping:
  `Mode::Eot`, `MessageBody::Eot { kind: "eot", details }`, `crc_ok` set,
  RSSI present, `raw` present, and the `details` JSON fields.

The **modulator is not an external reference** (`modulate.rs`): it only
proves the demod inverts this modulation. The FSK `SHIFT_HZ = 1800` is an
explicit **modulator choice** for the synthetic test (a conventional
~1.5 modulation index near the 1200-baud rate that sits inside the ~8 kHz
channel), **not** a claimed spec value; the on-air shift is not documented
by the cited sources.

## Known limitations / deferred

- **No off-air validation. No real-RF result.** No recorded EOT/HOT IQ is
  available; the entire demod chain is proven only on self-generated
  synthetic waveforms. Every demod tolerance (SNR, offset, timing) is
  untested against a real capture.
- **Modulation parameters are not fully pinned.** The FSK shift is a
  modulator choice (`SHIFT_HZ = 1800`); the cited sources give the baud
  rate and channels but not the deviation, so `CHANNEL_PASSBAND_HZ` and
  `SHIFT_HZ` are educated, not spec-confirmed.
- **Reverse-engineered, no formal standard.** There is no public AAR
  S-9152 document; field semantics follow the two cited open decoders. If
  they share a mistake, this crate inherits it.
- **Chaining bits unpinned.** `packet[11:13]` are BCH-protected but
  unnamed by either decoder; they are surfaced raw with no interpretation.
- **Detection, not correction.** The ciphered BCH check only *detects*
  errors in the 45-bit data block; there is no correction and no time
  diversity, so any corrupted packet is simply dropped (no `fec_corrected`).
- **No carrier/channel search.** Acquisition relies on the caller's DDC
  `freq_offset_hz` plus the demod's slow DC tracker; there is no automatic
  EOT carrier acquisition.
- **Direction is out-of-band.** `eot` vs `hot` is decided by the caller
  from the receive frequency (`is_hot`), not by any wire field; a capture
  tuned to the wrong channel would still parse but be mislabeled.
- **HOT command semantics not modeled.** Only the (telemetry-style) field
  map is decoded; HOT→EOT command packets are parsed with the same 74-bit
  layout, and command-specific field meanings are not separately documented.
- **No position output.** EOT carries no fix, so a decoded frame has no map
  location; it surfaces as a telemetry/message record only.

## Gotchas

1. The packet hunt runs on the **Manchester-decoded logical bit stream**,
   not on chips; the demod emits chips and the channel decoder pairs them.
2. Manchester pairing has **two phases** — the channel decoder tries both
   (0 and 1) and lets the frame-sync hunt reject the wrong one. Do not
   assume a fixed chip-pair alignment.
3. The hunt key is **17 bits** (`101010` + the 11-bit sync); the packet
   begins at the match index **+ 6** (the frame sync becomes `packet[0:11]`).
4. Multi-bit fields are **LSB-first on the wire** (`field_rev` reverses the
   slice); single status bits are read directly.
5. The BCH path reverses the data block, divides by `GENERATOR`, **then
   XORs `CIPHER`** — omitting the cipher XOR makes every frame fail verify.
6. `bch_ok` is **detection only**; `crc_ok`/`fec_corrected` reflect that
   (no correction is ever applied).
7. `CHANNEL_RATE = 24000` is exactly **10 samples/chip** at the 2400-chip
   rate; `FskDemod::new` asserts ≥ 4 samples/chip.
8. Frames are deduped by **raw packet-bit identity**; `packet_bits_of`
   recomputes the ciphered check when rebuilding `raw`, so `raw`
   round-trips through BCH verify.

## Key references

- **ereuter/PyEOT** (`eot_decoder.py`, `helpers.py`,
  https://github.com/ereuter/PyEOT) — the reverse-engineered 74-bit field
  map, battery-condition / arm-status semantics, the hunt run, and the
  ciphered BCH(63,45) generator/cipher and verify path (facts only).
- **russinnes/EOTDecode** (`eot_decoder.py`, `helpers.py`,
  https://github.com/russinnes/EOTDecode) — the second independent decoder;
  agrees byte-for-byte with PyEOT on the field map and BCH constants.
- **SIGIDWIKI** "End of Train Device (EOTD)" — on-air RF facts: 1200-baud
  FSK, EOT→HOT 457.9375 MHz, HOT→EOT 452.9375 MHz.
- **Wikipedia** "Computation of CRC" — the GF(2) long-division worked
  example used to anchor `checkbits` independently of the EOT key.
- `docs/notes/NAVTEX.md` — the sibling narrow-shift FSK channel-decoder
  template this crate's IQ front end and `*ChannelDecoder` contract follow.
