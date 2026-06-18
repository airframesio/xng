# Inmarsat Classic Aero — implementation notes

Native Inmarsat Classic Aero decode core (`crates/xng-mode-aero`),
**ported from JAERO** (Jonathan Olds, MIT — the only open Classic Aero
implementation; porting permitted with attribution, see `PROVENANCE.md`).
JAERO source is the structural reference *and* the off-air oracle. This
note is the as-built state; numbers are what the code does and the tests
assert (`cargo test -p xng-mode-aero` — 49 tests, 0 ignored).

Three front-ends spanning the P/R/T/C channel model, all feeding one
SU/ACARS layer:

| Decoder | Channel | Modulation | Mode tag | `--mode` |
|---|---|---|---|---|
| `AeroChannelDecoder` | L-band P-channel | A-BPSK 600/1200 bps + OQPSK 10.5 kbps | `aero-l` | `aero` |
| `AeroBurstDecoder` | C-band R/T feeder bursts | A-BPSK 600/1200 bps | `aero-c` | `aero-c` |
| `CChannelDecoder` | L-band C-channel voice circuit | OQPSK 8 400 bps | `aero-l` | (call-assigned) |

Each `AeroEvent` carries two orthogonal tags (AERO-8.1/8.2): a
`Mode::AeroL`/`Mode::AeroC` propagated by `to_message` (before this,
C-band feeder bursts mislabelled as `aero-l`) **and** an `AeroChannel`
(`PChannel`/`RChannel`/`TChannel`, surfaced in `details` as
`channel` = `p-`/`r-`/`t-channel`). A C-band feeder burst emits
`TChannel` for a reserved/TDMA T burst (6-byte AES/GES header + P-style
SUs) or `RChannel` for a random-access R burst (single 19-byte SU),
mirroring JAERO's `RTChannelDeleaveFECScram` OK_T/OK_R split. The
physical frame/burst rate rides in a distinct `line_bit_rate` key so it
never clobbers a decoded protocol `bit_rate` (e.g. the Pd carrier rate
in a 0x40 control ISU).

## Pipeline

Per L-band P-channel (`lib.rs::AeroChannelDecoder`): wideband IQ →
`xng_dsp::Ddc` → channel IQ → demod → 32-bit UW hunt → **16-bit header
parse** → deinterleave + Viterbi + descramble → 12-byte SUs → CRC →
reassembly + P-SU classification → ACARS via `xng_acars::block` →
`xng_types::Message`. Every structured SU is also fed to a
**self-configuring satellite/beam resolver** so each emitted message can
be tagged with the serving satellite (AERO-2).

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
OQPSK-decomposition MSK demod: simpler, ~2 dB less sensitive; the coherent
upgrade below (AERO-6) is now wired in as a burst-path fallback.

**Coherent A-BPSK fallback** (`coherent.rs::CoherentMskDemod`, AERO-6).
A decision-directed (Costas-style) carrier-phase recovery path for the
600/1200 bps burst demod. It shares the discriminator's front end (same
rate-matched LPF, same zero-crossing timing loop), but replaces the FM
discriminator with a carrier-coherent per-bit detector: it keeps a running
absolute phase reference `θ` (carrier + accumulated data phase — continuous
phase makes `θ` at each bit boundary known once earlier bits are decided),
correlates each bit's matched-filtered samples against the +90° and −90°
phase ramps anchored on `θ`, picks the larger in-phase energy, then advances
`θ` by the decided ±90° plus a small decision-directed phase-error term (a
Costas-style carrier loop, gain 0.05) so the reference tracks residual CFO
and phase drift. On the C-band burst path (`lib.rs`) the discriminator runs
first (robust on the burst preamble) and the coherent detector is a
**fallback** for marginal bursts: a second pass only when no UW/CRC matched,
so it cannot double-feed the cross-burst reassemblers. Validated by a
**synthetic** modulate → complex-AWGN → demod BER-vs-SNR sweep
(`tests/coherent_ber.rs::coherent_beats_discriminator_ber_vs_snr`): the
coherent path reaches a given BER at a ~1 dB lower Eb/N0 than the
discriminator through the identical front end. This is synthetic
(noise-test) validation only — it has **not** been run against real off-air
IQ.

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
from a 240 kHz wideband capture at +15 kHz offset).

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

**FEC-correction count** (AERO-6): `decode.fec_corrected` is now populated
on the P-channel (`frame.rs`), 10.5k OQPSK (`oqpsk.rs`), and C-band burst
(`burst.rs`) paths. It is the *real* number of coded-bit errors the Viterbi
fixed, derived by **re-encoding** the decoded bit stream with the same K=7
rate-1/2 encoder and counting how many coded bits disagree with the received
hard decisions over the frame's coded region (the P-channel counts from the
overlap-carry offset so the re-encoder state has converged before the
counted span). This costs an extra encode pass per frame — a Viterbi-side
correction count would be cheaper, but the re-encode keeps the count
honest without touching the decoder. `frame.rs` test
`fec_corrected_counts_real_corrections` injects a known set of coded-bit
flips and asserts the reported count equals exactly the injected (and
corrected) flips, with zero for a clean frame.

### P-channel 16-bit frame header (`frame::FrameHeader`, AERO-4)

The 16 bits immediately following the 32-bit UW carry frame sequencing
metadata. The framer assembles them MSB-first and `FrameHeader` splits
the word into four JAERO nibbles (oracle: JAERO `aerol.cpp`
`AeroL::Decode` `frameinfo`):

| Field | Bits | JAERO name |
|---|---|---|
| `format_id` | 15..12 | `formatid` (frame content/format selector) |
| `superframe` | 11..8 | `supfrmaker` (superframe-position marker) |
| `frame_counter1` | 7..4 | `framecounter1` |
| `frame_counter2` | 3..0 | `framecounter2` |

`FrameHeader::from_soft_bits` parses the header off the soft bits already
collected after the UW; `from_u16`/`to_u16` round-trip the word (the
`FrameEncoder` writes the header through `to_u16` so the wire word and
the decoder's parse share one definition). The parsed header is latched
per frame (`Framer::last_header`) and surfaced in the message `details`
as a nested `frame_header` object. On the **10.5k OQPSK** path the same
header is parsed from the first 16 bits of the 16+178-bit skip region
(`oqpsk.rs::HrFramer`). The state machine that *consumes* these fields
(superframe lock + AFC/DCD) is a deferred follow-up — see limitations.

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

**R-channel user-data SUs** (`su.rs::RIsuReassembler`): 19-byte SUs (CRC
over 17), up to 3 per message (SEQINDICATOR nibble → k-of-n), 11 user
bytes each except the last (SUTYPE = user bytes; 0/15 = signalling,
skipped). The SEQINDICATOR → (k, n) mapping (1→(1,1), 2→(1,2), 3→(2,2),
4→(1,3), 5→(2,3), 6→(3,3); JAERO's 0-based SUindex → k = SUindex+1) is now
**verified against JAERO's `RISUData::update` switch**
(`seq_indicator_matches_jaero_switch`).

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
  from byte7/8 and byte9/10 — Psmc2 reported only when non-zero, Psmc1
  spot-beam flag = high bit of byte6); `0x05`
  GES Psmc/Rsmc channels (seqno/lsu from byte3, GES from byte4, three
  16-bit channels; Rsmc transmit carriers offset +101.5 MHz — naming by
  `lsu`: Psmc(RX)+Rsmc0,1 for lsu≤1, Rsmc2..4 / Rsmc5..7 for lsu 2/3);
  `0x07` GES_beam_support and `0x0A` broadcast_index (named by JAERO with
  no further field decode, surfaced as named events). byteN = our su[N-1]
  (JAERO 1-based octet indexing). These three drive the AERO-2 resolver
  (below).
- **Remaining control / user-data types** (AERO-1.4, JAERO `AEROTypeP`).
  Only `0x40` carries fields JAERO decodes: `0x40` P/R_channel_control_ISU
  — the GES advertises a Pd (packet-data) carrier: GES = octet 5, bit-rate
  code = (byte8>>4)&0xF through JAERO's table (0→600 … 6→10500, 7→8400,
  9→21000; 8/≥10 reserved → field omitted), Pd channel =
  ((byte9&0x7F)<<8)|byte10 → ×0.0025 + 1510.0 MHz, spot-beam = byte9 bit 7
  (`pd_mhz`, `bit_rate`, `spotbeam`, `ges_id`). `0x28`
  EIRP_table_broadcast, `0x41` T_channel_control_ISU, `0x61`
  Request_for_acknowledgement (RQA), `0x62` Acknowledge (RACK/TACK):
  JAERO names these and decodes no further fields → named events. `0x74`
  / `0x76` short-LSDU RLS user-data: JAERO names but does not run through
  the ISU/SSU reassembler → `short-lsdu` event carrying the LSDU octet
  length (3 / 4).

**R-channel control SUs** (`su.rs::parse_r_su`, AERO-3): a 19-byte
R-channel SU is a *control* SU when JAERO's user-data flag is clear
(`infofield[1] & 0x08 == 0`, our su[1] bit 3); otherwise it routes to the
ISU/SSU reassembler (`RIsuReassembler`, which now enforces the same flag).
For a control SU the message type is the **third** byte (su[2] — the same
byte the user-data path uses for the AES high octet, so AES/GES do not
apply); xng surfaces the named event only (`su_type`, `su_type_hex`,
optional `request_kind`). Types are JAERO's `AEROTypeR`: `0x20` general
access-request (telephone), `0x23` abbreviated access-request (telephone),
`0x22` access-request (data, R/T channel), `0x61`
request-for-acknowledgement, `0x62` acknowledgement, `0x12`
log-on/log-off control, `0x30` call-progress, `0x15` log-on/log-off
acknowledgement, `0x17` log-control ready-for-reassignment, `0x60`
telephony-acknowledge. `parse_p_su` runs on P-channel SUs and the P-style
SUs inside T bursts; `parse_r_su` classifies the R-channel control set.

User-data ISU/SSU (`0x71`/`0xC0|seq`) and fill (`0x01`) are not
classified (handled by the reassembler); types JAERO does not enumerate
are framed but not interpreted.

## Satellite / beam resolution (`satellite::SatelliteResolver`, AERO-2)

The L-band analogue of the HFDL system table
(`xng-mode-hfdl::systable`): a **self-configuring** resolver that learns
which satellite serves the channel purely from the AES system-table
broadcast SUs already decoded in AERO-1.3, then tags every emitted
message with the resolved satellite + beam. There is no scan plan and no
preset table — like HFDL re-learning its table, a later 0x0C
re-resolves. The resolver is fed every structured SU on the **P-channel
framer, the 10.5k OQPSK framer, and the C-band burst decoder** (T-burst
P-style SUs can carry system-table broadcasts).

| Input SU | `su_type` | What it contributes |
|---|---|---|
| `0x0C` satellite_identification | `satellite-id` | authoritative: `satellite_id`, `longitude_deg`, `longitude_dir`, Psmc1 spot-beam → beam |
| `0x07` GES_beam_support | `ges-beam-support` | sets `ges_beam_support` presence flag |
| `0x05` GES Psmc/Rsmc channels | `smc-channels` | latches `resolved_ges_id` (context only) |

- **Satellite identity** comes verbatim from the 0x0C broadcast. JAERO
  (`AES_system_table_broadcast_satellite_identification_COMPLETE`)
  decodes `satid` and the orbital longitude (`byte6 × 1.5°`, `>180 ⇒ W`)
  and only *displays* them — it has **no satellite-name table** — so the
  resolved identity is the numeric `satellite_id` plus its measured
  `longitude_deg`/`longitude_dir`. No name is invented.
- **Beam** (global vs spot) is read from the Psmc1 spot-beam flag the
  0x0C handler surfaces (`psmc1_spotbeam`, high bit of the carrier's high
  octet). Reported as `beam`: `"spot"` / `"global"` / `"unknown"`.
- **Ocean region** is a *nominal best-effort* hint only (JAERO names no
  regions): `OceanRegion::classify` picks the nearest classic Inmarsat
  slot by orbital longitude within a ±35° tolerance — AOR-W ≈ 54°W,
  AOR-E ≈ 15.5°W, IOR ≈ 64°E, POR ≈ 178°E (Inmarsat-3 F5 documented at
  54°W, F3 at 178°E; AOR-E/IOR are the classic centres). A satellite far
  from every slot is left **unclassified rather than guessed**; longitude
  wraps at ±180°. The measured longitude is ground truth; the region is
  secondary.
- **Output**: `details()` emits `resolved_satellite` (`satellite_id`,
  `longitude_deg`, `longitude_dir`, optional `region`) plus `beam`, and
  optional `ges_beam_support` / `resolved_ges_id`. `annotate` /
  `enrich_details` merge these into the message `details` **without
  overwriting existing keys**. An event with no structured SU but a
  resolved satellite or parsed header still emits, on an `aero-frame`
  body, so the AERO-2 tag and AERO-4 header reach `details` even for
  otherwise-bare frames.

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
  voice + SU), `burst_e2e.rs` (T-burst ACARS, R-burst user-data, and an
  R-burst control SU), `mode_label.rs` (AeroL/AeroC tagging),
  `p_su.rs` + `su.rs` unit tests (every P-SU type incl. the AERO-1.4 0x40
  control-ISU frequency arithmetic, the `parse_r_su` control set, and the
  JAERO-verified SEQINDICATOR switch), `frame.rs`/`cchannel.rs`
  interleaver and FEC roundtrips. The OQPSK demod's
  `locks_and_demodulates_with_cfo` asserts BER < 0.001 at CFO
  0/±120/−250 Hz.

**AERO-4 frame header** (`frame.rs` tests): `frame_header_splits_jaero_nibbles`
checks the four-nibble split against JAERO's `frameinfo` shifts
(`0x1234` → 1/2/3/4, `0xFFFF` → all-ones, MSB-first soft-bit parse of
`0xABCD`); `frame_header_roundtrips_through_encoder` recovers the
encoder's header (format id 1, superframe 0, both counters = the running
frame counter) by re-parsing bits 32..48 of the framed output.

**SU type enumerators** (VERIFY-8, `su.rs` test
`aero_type_enumerators_match_jaero_aerol_h`): the `AEROTypeP` /
`AEROTypeR` / `AEROTypeC` enumerator hex values this crate dispatches on
were verified verbatim against the JAERO source (`aerol.h`, fetched from
`github.com/jontio/JAERO` master) before relying on the SU-type table. No
mismatches were found — every type byte already matched JAERO; the test
locks that in and fails if a handler's type byte ever drifts from the JAERO
enumerator it claims to decode.

**AERO-2 resolver** (`satellite.rs` tests, oracle = JAERO 0x0C field
layout): `ocean_region_classifies_classic_slots` pins the four slot
centres, near-slot tolerance, ±180° wrap, and the "far from every slot →
unclassified" rule; `resolver_learns_satellite_from_0x0c_broadcast`
feeds the same JAERO-layout 0x0C SU the AERO-1.3 oracle test pins
(satid 20, longitude index 200 → 60.0°W → AOR-W, global beam) and checks
`details()` plus the non-clobbering `annotate`;
`resolver_reconfigures_and_tracks_beam_support` checks self-reconfigure
on a second 0x0C and the 0x07 beam-support flag.

xng's Aero is **oracle-validated field-exact** with no count-style
benchmark vs JAERO yet (captures too large to vendor; cf.
[BENCHMARKS.md](BENCHMARKS.md), where Aero/STD-C/Iridium are fenced by
exact-result fixtures rather than CI count gates).

## Known limitations / intentional gaps

- **Superframe-lock / AFC-DCD state machine deferred (AERO-4).** The
  16-bit header (`format_id` / `superframe` / `frame_counter1/2`) is now
  parsed and exposed, but the state machine that *consumes* it — JAERO's
  `FreqOffsetEstimateSlot`, which locks the superframe and drives the
  channel AFC/DCD — is a documented follow-up. xng channels are DDC-tuned
  and rely on the unlocked-only coarse CFO for reacquisition.
- **No satellite-name table (AERO-2).** JAERO has no satid→name map and
  Inmarsat publishes no public stable satid→spacecraft registry, so the
  resolved identity stays numeric (`satellite_id`) + measured longitude.
  The ocean region is a nominal longitude hint only (±35° to a classic
  slot); a precise satid→spacecraft mapping would need an external
  registry we cannot ground.
- **600/1200 P-channel demod is a discriminator, not coherent** — ~2 dB
  below JAERO's coherent MSK demod. Intentional v1 simplification. A
  coherent (decision-directed) detector now exists (`coherent.rs`,
  AERO-6) and is wired into the **C-band burst path** as a marginal-burst
  fallback, but the streaming P-channel chain still runs the discriminator;
  the coherent path's ~1 dB gain is **synthetic-only** (a modulate→AWGN→demod
  BER sweep), not yet confirmed against off-air IQ.
- **No off-air OQPSK fixture in CI**. The 10.5k and C-channel chains are
  exercised by RF loopback (and the 10.5k full chain has run against
  JAERO's `10.5k_sample.ogg`), but no OQPSK capture is vendored.
- **C-channel scrambler alignment unconfirmed off-air**. JAERO delays
  decoded bits by 2714−6 before the descrambler (`dl2`) for off-air
  alignment; loopback is self-consistent without it — flagged for when an
  off-air C-channel capture is available.
- **AMBE voice not decoded** — voice frames surfaced as raw 12-byte
  chunks (proprietary codec).
- **Named-only SU types**: many SUs are surfaced as named events with
  addressing/raw bytes but no further field decode — because **JAERO
  itself decodes no further fields** for them: T_channel_assignment
  (`0x51`), GES_beam_support (`0x07`), broadcast_index (`0x0A`), EIRP-table
  (`0x28`), T_channel_control_ISU (`0x41`), RQA (`0x61`), Acknowledge
  (`0x62`), short-LSDU (`0x74`/`0x76`), and the whole R-channel control set
  (`parse_r_su`). 0x07 still contributes its presence flag to the AERO-2
  resolver. This is parity with JAERO, not a deferred gap.

## References

- **JAERO** (Jonathan Olds, MIT) — structural port + off-air oracle:
  `aerol.cpp/.h` (incl. `AeroL::Decode` `frameinfo` and
  `AES_system_table_broadcast_satellite_identification_COMPLETE`),
  `mskdemodulator.cpp`, `oqpskdemodulator.cpp`, `coarsefreqestimate.cpp`,
  `burstmskdemodulator.cpp`, `jconvolutionalcodec.cpp`. See
  `crates/xng-mode-aero/PROVENANCE.md`.
- Inmarsat Classic Aero system (P/R/T/C channel model); Inmarsat-3
  operational orbital slots (region-centre hints, `docs/REFERENCES.md`).
- ARINC 618 ACARS (carriage), handled by `xng-acars`.
- Shared DSP: `xng_dsp::{Ddc, Fir, scramble::Lfsr15, viterbi::Viterbi,
  checksum::HDLC_FCS}`.
