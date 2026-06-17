# Inmarsat Classic Aero — implementation notes

Native Inmarsat Classic Aero decode core (`crates/xng-mode-aero`),
**ported from JAERO** (Jonathan Olds, MIT — the only open Classic Aero
implementation; porting permitted with attribution, see `PROVENANCE.md`).
JAERO source is the structural reference *and* the off-air oracle. This
note is the as-built state; numbers are what the code does and the tests
assert (`cargo test -p xng-mode-aero` — 32 tests, 0 ignored).

Four physical channels, four front-ends, all feeding one SU/ACARS layer:

| Decoder | Channel | Modulation | Mode tag | `--mode` |
|---|---|---|---|---|
| `AeroChannelDecoder` | L-band P-channel | A-BPSK 600/1200 bps + OQPSK 10.5 kbps | `aero-l` | `aero` |
| `AeroBurstDecoder` | C-band R/T feeder bursts | A-BPSK 600/1200 bps | `aero-c` | `aero-c` |
| `CChannelDecoder` | L-band C-channel voice circuit | OQPSK 8 400 bps | `aero-l` | (call-assigned) |

`Mode::AeroL` vs `Mode::AeroC` is carried per-`AeroEvent` and propagated
by `to_message` (AERO-8.1) — before that, C-band feeder bursts
mislabelled as `aero-l`.

## Pipeline

Per L-band P-channel (`lib.rs::AeroChannelDecoder`): wideband IQ →
`xng_dsp::Ddc` → channel IQ → demod → 32-bit UW hunt → header skip →
deinterleave + Viterbi + descramble → 12-byte SUs → CRC → reassembly +
P-SU classification → ACARS via `xng_acars::block` → `xng_types::Message`.

Both low rates run in parallel on a 24 kHz channel (600 and 1200 bps
chains; whichever locks wins). The 10.5 kbps OQPSK chain runs on its own
48 kHz channel when the input rate can carry it (a second DDC, or no-DDC
when fed exactly 48 kHz). The C-channel is call-assigned (frequency from
P-channel setup, not a scan plan), so `CChannelDecoder` is a separate
entry point.

## PHY / demod

**A-BPSK 600/1200 bps** (`demod.rs::MskDemod`). A-BPSK is BPSK with
sinusoidal transitions — an MSK-class signal at ±fb/4 deviation. **Data
maps directly onto the deviation sign** (bit 1 = +90° phase advance over
the bit), so a frequency discriminator with per-bit integration yields
the data bits with no differential step. Per chain: rate-matched lowpass
(cutoff 0.6·fb, 101 taps) → conjugate-product discriminator → slow
freq-offset EMA → square-transition timing recovery (gain 0.1) →
per-bit integrate → running-|integral| normalize → soft bit in [-1,1].
This is a deliberate divergence from JAERO's coherent
OQPSK-decomposition MSK demod: simpler, ~2 dB less sensitive; a coherent
upgrade is the planned improvement (`PROVENANCE.md`).

**OQPSK 10.5 kbps and 8.4 kbps** (`oqpsk.rs::OqpskDemod`, ported from
JAERO `oqpskdemodulator.cpp` + `coarsefreqestimate.cpp`). Coherent: RRC
matched filter (β=1 at 10.5k, β=0.6 at 8.4k, 55 taps) → AGC (2.84 clip)
→ non-data-aided square-law symbol timing (1-sample power differentiator,
T/4+T/4 delay-difference detector, narrow IIR resonator — JAERO's 48 kHz
coefficients per rate — strobed timing oscillator, ±0.1 Hz pull; the
clock acquires independently of the carrier) → strobes alternate rails at
the bit rate, consecutive strobes pair into de-offset QPSK points →
carrier tracked by JAERO's tanh cross-product discriminator
`tanh(I_d)·Q_d − tanh(Q)·I` through a 2nd-order loop filter, with a slow
moving-average bias rotation. Coarse CFO: squaring OQPSK yields spectral
lines at 2f0 ± symbol-rate; a two-tone matched search over the smoothed
2^14 squared-signal spectrum locates 2f0, applied only while unlocked.
Lock = low constellation MSE (< 0.5) **and** a stationary 4th-power
statistic (a spinning constellation has deceptively low MSE). The IIR
coefficients are designed at 48 kHz, so the OQPSK chains are always fed
48 kHz. **The 10.5k demod decodes end-to-end** (BER 0 at CFO
0/±120/−250 Hz; ACARS recovered through RF loopback at 120 Hz CFO and
from a 240 kHz wideband capture at +15 kHz offset) — the earlier "no
carrier lock" caveat in `PROVENANCE.md` is stale; see Validation.

**C-band R/T bursts** (`burst.rs`). A `BurstGate` collects samples while
energy is present (8× noise-floor trigger, 10 dB-drop end relative to a
slow in-burst power average), then `demod_burst` re-processes the whole
burst: locate signal start by power, measure CFO on the leading carrier
section (conjugate-product over a 30-symbol window), mix it down, run the
discriminator demod. Burst layout: unmodulated carrier → alternating
1010 → UW within ~300 bits.

## Framing / FEC

P-channel frame (`frame.rs`): **UW `0xE15AE893`** (32 bits, MSB-first) +
16-bit header + 1152 coded bits = 1200-bit frame → 576 decoded bits =
72 bytes = 6 SUs. UW hunt tolerates ≤2 bit errors (off-air bits are not
clean; a false trigger costs one frame and dies at the SU CRCs).

- **Interleaver**: 64 rows × N cols (6 at 600 bps, 9 at 1200), row visit
  order (27·i) mod 64, column-major; deinterleave per 64×N block.
- **Convolutional code**: K=7, rate 1/2, polynomials 0o171/0o133;
  **0o133 output first** in each coded pair on air (libcorrect's 109/79
  order in JAERO) — with 171-first off-air frames are pseudorandom.
  Continuous across frames; decoded here per-frame with a 62-coded-bit
  overlap carry instead of JAERO's streaming libcorrect decode.
- **Scrambler**: the VDL2/HFDL shared 15-stage LFSR (x^15+x+1,
  `xng_dsp::scramble::Lfsr15`), applied to the *decoded* bits, **reset at
  each UW**. Bits pack **LSB-first**.

10.5 kbps OQPSK framing (`oqpsk.rs::HrFramer`): **64-bit dual-rail UW**
(the same 32-bit UW carried on each rail, bits interleaved; per-rail
polarity resolved by the UW search over all hypotheses) + 16-bit header +
178 dummy bits + 4992 coded bits (one 64×78 interleaver block), then the
shared Viterbi/descramble/SU path.

C-channel frame (`cchannel.rs`, ~500 ms superframe at 8400 bps): **112-bit
UW** = two 52-bit rail patterns (JAERO `setPreamble`
216866263330005 / 3012071630031408), each detector trying both patterns
and their complements (the OQPSK 180° ambiguity resolves per rail) +
4096 coded bits. FEC = the P-channel K=7 rate-1/2 code **punctured 3/4**
(depuncture inserts a neutral bit after every 3rd, last source bit
dropped), interleaved as 16 × (64×4) blocks with the (27·i) mod 64 row
permute; decoded 2730 → first 2714 kept; LFSR15 descramble.

C-band burst framing (`burst.rs`): after the UW, one 64×5 interleaver
section (→ 20 bytes) then 64×3 sections (→ 12 bytes each). The first
section holds either one 19-byte R-channel SU or a 6-byte T-burst header
(AES 3 + GES 1 + CRC 2) followed by 12-byte P-style SUs.

## Signal Units, reassembly, and decoded types

All SU CRCs are **CRC-16/X-25** (`xng_dsp::checksum::HDLC_FCS`), LE
trailer. The all-zero SU is accepted (JAERO rule).

**P-channel user data** (`su.rs`): 12-byte SUs, CRC over the first 10.
**ISU `0x71` + SSU `0xC0|seq`** reassembly keyed by AES/GES/QNO/REFNO
with SEQNO counting down and NOOCTLESTINLASTSSU tail handling; stale
partials age out after 10 SUs. Completed user data = `FF FF` +
SOH-prefixed ACARS block, extracted by `parse_acars` and parsed by
`xng_acars::block` (ARINC 618: label, BCS, applications — ADS-C, CPDLC,
etc.; multi-block defragmentation lives in `xng-acars`).

**R-channel SUs** (`su.rs::RIsuReassembler`): 19-byte SUs (CRC over 17),
up to 3 per message (SEQINDICATOR nibble → k-of-n), 11 user bytes each
except the last (SUTYPE = user bytes; 0/15 = signalling, skipped). The
k-of-n SEQINDICATOR mapping is flagged in `PROVENANCE.md` for off-air
verification.

**Structured (non-user-data) P-channel SUs** (`su.rs::parse_p_su`,
surfaced as `MessageBody::Aero { kind, details: JSON }`; type table =
JAERO `AEROTypeP`, field layouts = JAERO `aerol.cpp` handlers):

- **Log-on/log-off control `0x10`–`0x17`** (AERO-1.1): `0x10`
  log_on_request, `0x11` log_on_confirm, `0x12` log_off_request, `0x13`
  log_on_reject, `0x14` log_on_interrogation, `0x15`
  log_on/log_off_acknowledge, `0x16` log_on_prompt, `0x17`
  data_channel_reassignment. AES id = octets 2–4, GES id = octet 5
  (JAERO `SendLogOnOff`). Emitted as structured session events with an
  inferred direction (AES-initiated request/log-off; GES-issued
  confirm/reject/interrogation/prompt/reassignment; acknowledge either
  way). JAERO only *names* these; xng adds the structured event.
- **C-channel assignment `0x31`–`0x34`** (distress / flight-safety /
  other-safety / non-safety): RX channel octets 7/8 → ×0.0025 + 1510.0
  MHz, TX octets 9/10 → ×0.0025 + 1611.5 MHz, spot-beam flag = high bit
  of each high octet (JAERO `CreateCAssignmentItem`).
- **Call_announcement `0x21`** (AERO-1.2): GES announces an incoming
  call; reuses the C-assignment octet layout (RX/TX channel pair,
  spot-beam flags).
- **T_channel_assignment `0x51`** (AERO-1.2): reservation T channel for
  burst data return. JAERO decodes no fields beyond AES/GES, so xng
  surfaces the named event with addressing only.
- **AES system-table broadcasts** (AERO-1.3): `0x0C`
  satellite_identification (seqno, satid split across byte3/byte4,
  orbital longitude = byte6 × 1.5° with >180 ⇒ west, Psmc1/Psmc2 carriers
  from byte7/8 and byte9/10 — Psmc2 reported only when non-zero); `0x05`
  GES Psmc/Rsmc channels (seqno/lsu from byte3, GES from byte4, three
  16-bit channels; Rsmc transmit carriers offset +101.5 MHz — naming by
  `lsu`: Psmc(RX)+Rsmc0,1 for lsu≤1, Rsmc2..4 / Rsmc5..7 for lsu 2/3);
  `0x07` GES_beam_support and `0x0A` broadcast_index (named by JAERO with
  no further field decode, surfaced as named events). byteN = our su[N-1]
  (JAERO 1-based octet indexing).

User-data ISU/SSU (`0x71`/`0xC0|seq`) and fill (`0x01`) are not
classified (handled by the reassembler); other types are framed but not
yet interpreted.

**C-channel** (`cchannel.rs`, decoded 2714 info bits = 25 sub-blocks of
1 + 96 + 12 bits):

- **AMBE voice frames**: the 96-bit chunks → 12-byte frames (20 ms of
  compressed audio), surfaced as `CChannelEvent::Voice` for external
  decoding (the AMBE codec is proprietary and not decoded here).
- **Sub-band signal units**: the 12-bit chunks accumulate (LSB-first)
  into 12-byte SUs (CRC-16/X.25), surfaced as
  `CChannelEvent::SignalUnit`. Named types (JAERO `AEROTypeC`): `0x01`
  fill, `0x30` call-progress (AES/GES), `0x60` telephony-acknowledge.

## Validation / oracles

**JAERO is the oracle**, both structurally and off-air. Two layers:

- **Off-air (the conventions loopback can't see)**. JAERO ships real
  off-air recordings. `tests/offair.rs` decodes a 12 s slice of JAERO's
  `600bps_sample.ogg` (vendored as `tests/data/600bps_offair_12s.i16`,
  MIT-attributed, P-channel ~1066 Hz) through the full native chain and
  asserts CRC-valid ACARS including tail `HL8217`. Full-recording results
  (`PROVENANCE.md`): 600 bps sample → 11 CRC-valid ACARS (B-16333 METAR,
  HL8217 ADS, B-HNF PDC clearance, …); `10.5k_sample.ogg` → 188 events /
  144 CRC-valid ACARS through the OQPSK demod (A7-AEE CPDLC among them).
  These pinned the off-air conventions: **direct deviation-sign bit
  mapping** (no differential layer), **0o133-output-first** coded pair
  order, per-frame LFSR15 reset, LSB-first packing, X-25 SU CRC.
- **Loopback (full chain, bit-exact)**. `end_to_end.rs` (600/1200 A-BPSK
  → waveform → decode), `hr_e2e.rs` (10.5k OQPSK incl. rail inversion and
  a 240 kHz wideband capture at +15 kHz), `cchannel_e2e.rs` (8.4k OQPSK
  voice + SU), `burst_e2e.rs` (R/T feeder bursts), `mode_label.rs`
  (AeroL/AeroC tagging), `p_su.rs` + `su.rs` unit tests (every P-SU type
  and frequency-arithmetic case), `frame.rs`/`cchannel.rs` interleaver
  and FEC roundtrips. The OQPSK demod's `locks_and_demodulates_with_cfo`
  asserts BER < 0.001 at CFO 0/±120/−250 Hz.

xng's Aero is **oracle-validated field-exact** with no count-style
benchmark vs JAERO yet (captures too large to vendor; cf.
[BENCHMARKS.md](BENCHMARKS.md), where Aero/STD-C/Iridium are fenced by
exact-result fixtures rather than CI count gates).

## Known limitations / intentional gaps

- **600/1200 demod is a discriminator, not coherent** — ~2 dB below
  JAERO's coherent MSK demod. Intentional v1 simplification; coherent
  upgrade planned (`PROVENANCE.md`).
- **No off-air OQPSK fixture in CI**. The 10.5k and C-channel chains are
  exercised by RF loopback (and the 10.5k full chain has run against
  JAERO's `10.5k_sample.ogg`), but no OQPSK capture is vendored.
- **No channel AFC / DCD state machine** (JAERO's
  `FreqOffsetEstimateSlot`); channels are DDC-tuned and the unlocked-only
  coarse CFO covers reacquisition.
- **C-channel scrambler alignment unconfirmed off-air**. JAERO delays
  decoded bits by 2714−6 before the descrambler (`dl2`) for off-air
  alignment; loopback is self-consistent without it — flagged for when an
  off-air C-channel capture is available.
- **AMBE voice not decoded** — voice frames surfaced as raw 12-byte
  chunks (proprietary codec).
- **R-channel SEQINDICATOR k-of-n mapping** flagged for off-air
  verification.
- **Partially-decoded SU types**: T_channel_assignment (`0x51`),
  GES_beam_support (`0x07`), broadcast_index (`0x0A`) are named with
  addressing/raw bytes only — JAERO itself decodes no further fields.

## References

- **JAERO** (Jonathan Olds, MIT) — structural port + off-air oracle:
  `aerol.cpp/.h`, `mskdemodulator.cpp`, `oqpskdemodulator.cpp`,
  `coarsefreqestimate.cpp`, `burstmskdemodulator.cpp`,
  `jconvolutionalcodec.cpp`. See `crates/xng-mode-aero/PROVENANCE.md`.
- Inmarsat Classic Aero system (P/R/T/C channel model).
- ARINC 618 ACARS (carriage), handled by `xng-acars`.
- Shared DSP: `xng_dsp::{Ddc, Fir, scramble::Lfsr15, viterbi::Viterbi,
  checksum::HDLC_FCS}`.
