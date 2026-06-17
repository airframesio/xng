# Provenance — xng-mode-aero

Ported from **JAERO** (https://github.com/jontio/JAERO), MIT license,
Copyright (c) Jonathan Olds — porting permitted with attribution, which
this file and the crate documentation provide. JAERO is the only open
implementation of Inmarsat Classic Aero.

Ported facts/structures (from `aerol.cpp/.h`, `mskdemodulator.cpp`,
`jconvolutionalcodec.cpp`):

- P-channel frame: UW 0xE15AE893 (32 bits, MSB-first) + 16-bit header +
  1152 coded bits = 1200-bit frames.
- Interleaver: 64 rows × N columns (6 at 600 bps, 9 at 1200 bps), row
  visit order (27·i) mod 64, column-major readout.
- Convolutional code: K=7 rate 1/2, polynomials 0o171/0o133 (JAERO passes
  the bit-reversed libcorrect forms 109/79), continuous across frames
  (decoded here with a 62-coded-bit overlap carry).
- Scrambler: the same 15-stage LFSR as VDL2 (x^15+x+1, shared in
  xng-dsp::scramble), applied to the *decoded* bits, reset at each UW.
- Signal Units: 12 bytes, CRC-16/X-25 over the first 10 (little-endian
  trailer; the all-zero SU is accepted); SU type table; ISU 0x71 + SSU
  0xC0 reassembly keyed by AESID/GESID/QNO/REFNO with SEQNO countdown and
  NOOCTLESTINLASTSSU tail handling.
- ACARS carriage: reassembled user data = FF FF + standard SOH-prefixed
  ACARS block (parity-bearing characters, BCS, DEL) — parsed by
  xng-acars::block; multi-block defragmentation matches on
  registration/label/mode/AES/GES with alphabetically incrementing block
  ids.

10.5 kbps OQPSK demodulator (`oqpsk.rs::OqpskDemod`) ported from
`oqpskdemodulator.cpp` + `coarsefreqestimate.cpp`:

- RRC(β=1) matched filter at 48 kHz; AGC with 2.84 clip.
- Non-data-aided square-law symbol timing: 1-sample power differentiator,
  T/4+T/4 delay-difference detector, narrow 10 500 Hz IIR resonator
  (JAERO's 48 kHz coefficients), quadrature phase detector against a
  strobed timing oscillator (±0.1 Hz pull) — the clock acquires
  independently of the carrier.
- Strobes at 10 500/s alternate rails; consecutive strobes pair into
  de-offset QPSK points; carrier tracked by JAERO's "BPSK 2x" tanh
  cross-product discriminator `tanh(I_d)·Q_d − tanh(Q)·I` through the
  2nd-order loop filter (48 kHz coefficients), with the slow
  moving-average bias rotation.
- Coarse CFO: squaring OQPSK yields spectral lines at 2f0 ± 5250 Hz; a
  two-tone matched search over the smoothed 2^14 spectrum of the squared
  signal locates 2f0 (JAERO folds the same spectrum against
  `expectedpeakbin = fb/2`). Applied only while unlocked; lock = low
  constellation MSE *and* a stationary 4th-power statistic (a spinning
  constellation has deceptively low MSE).
- Sign note: the discriminator slope w.r.t. constellation rotation is
  negative once the off-rail (transitional) component statistics are
  taken into account; the correction signs in this port reflect that and
  are verified by the locks_and_demodulates_with_cfo test (BER 0 at
  CFO 0/±120/−250 Hz).

Divergence from JAERO (documented intentionally):

- 600/1200 bps demodulator: JAERO uses a coherent OQPSK-decomposition MSK
  demod with FFT square-law coarse AFC; xng v1 uses a
  frequency-discriminator MSK demod with offset tracking (simpler, ~2 dB
  less sensitive; the differential encoding makes discriminator output
  the data bits directly). Coherent upgrade is a planned improvement.
- No AFC of the channel center / DCD interplay (JAERO's
  FreqOffsetEstimateSlot state machine); xng channels are DDC-tuned and
  the unlocked-only coarse correction covers reacquisition.
- Per-frame Viterbi with overlap instead of JAERO's streaming
  libcorrect decode (equivalent output, simpler state).

Off-air conventions (established against JAERO's real recordings,
2026-06; these are invisible to synthetic loopback because a matched
modulator/demodulator pair cancels them):

- A-BPSK data maps **directly** onto the deviation sign (bit 1 = +90°
  phase advance); there is no differential layer. The UW appears in true
  polarity at 1200-bit spacing in the discriminator's bit stream.
- The coded pair order on air is **(0o133 output, 0o171 output)** per
  data bit — libcorrect's 109/79 polynomial order in JAERO. With this
  order the off-air frames decode with zero Viterbi residual and all SU
  CRCs pass; with 171-first they are pseudorandom.
- Frame layout, 64×6 per-384-bit-block deinterleaving, the shared LFSR15
  scrambler reset per frame, LSB-first packing, and the X-25 SU CRC are
  all confirmed exactly as implemented.

Off-air validation results (JAERO samples):

- `600bps_sample.ogg` (78 s, carrier ~1066 Hz): 11 CRC-valid ACARS from
  real traffic (B-16333 METAR uplink, HL8217 ADS, B-HNF CPA509 PDC
  clearance, B-LIC, 37981S).
- `10.5k_sample.ogg` (240 s, carrier ~5761 Hz, resampled 44.1→48 kHz):
  188 events / 144 CRC-valid ACARS through the OQPSK demod (A7-AEE
  CPDLC AT1 among them).
- A 12 s slice of the 600 bps recording is vendored as a CI fixture
  (tests/data/, attributed) and guarded by tests/offair.rs.

Conformance anchors: JAERO ships real off-air samples
(`samples/600bps_sample.ogg` etc.) usable for cross-validation; loopback
tests here exercise the full chain bit-exactly.

10.5 kbps A-QPSK status: the framing layer (dual-rail UW with per-rail
inversion hypotheses, 16+178-bit header/dummy skip, 64x78 interleaver,
shared Viterbi/descrambler/SU path) is implemented and bit-level tested;
the coherent OQPSK demodulator does not yet achieve carrier lock and its
RF loopback tests are #[ignore]d pending a focused demod session
(JAERO's tanh cross-product loop is the port reference).

C-channel (8 400 bps OQPSK voice circuits) ported from
`aerol.cpp::DecodeC` + `oqpskdemodulator.cpp` (fb==8400 paths):

- Frame: 112-bit UW (two 52-bit rail patterns, JAERO `setPreamble`
  arguments 216866263330005 / 3012071630031408, each detector trying
  both patterns and complements for the OQPSK ambiguity) + 4096 coded
  bits per ~500 ms superframe.
- FEC: the P-channel K=7 rate-1/2 code punctured 3/4 (depuncture
  inserts a neutral bit after every 3rd, last source bit dropped);
  interleaving 16 × (64×4) blocks with the (27·i) mod 64 row permute;
  decoded 2730 → first 2714 kept; LFSR15 descramble.
- Payload: 25 sub-blocks of 1 + 96 + 12 bits — 96-bit AMBE voice
  frames (12 bytes, surfaced for external decoding; the codec itself
  is proprietary) and 12-bit slices accumulating into 12-byte sub-band
  signal units (CRC-16/X.25), types 0x01 fill / 0x30 call progress
  (AES, GES ids) / 0x60 telephony acknowledge.
- Demod: the ported OQPSK demodulator with RRC β=0.6 and JAERO's
  8 400-specific ~10 Hz timing-resonator coefficients.

Note: JAERO additionally delays decoded bits by 2714−6 before the
descrambler (`dl2`) for off-air scrambler alignment; our loopback is
self-consistent without it, and the alignment question is flagged for
when an off-air C-channel capture is available.

P-channel SU classifier (`su::parse_p_su`): structured (non-user-data)
P-channel SUs are classified into JSON values surfaced as
`MessageBody::Aero { kind, details }`. SU type table is JAERO `AEROTypeP`
(`aerol.h`); per-type field layouts are JAERO's `aerol.cpp` handlers.

- AERO-1.1 — log-on/log-off control (0x10–0x17, JAERO `AEROTypeP`):
  0x10 log_on_request, 0x11 log_on_confirm, 0x12 log_off_request,
  0x13 log_on_reject, 0x14 log_on_interrogation,
  0x15 log_on/log_off_acknowledge, 0x16 log_on_prompt,
  0x17 data_channel_reassignment. AES id = octets 2–4, GES id = octet 5
  (JAERO `SendLogOnOff`). Surfaced as the AES↔GES session handshake with
  an inferred direction (AES-initiated request/log-off vs GES-issued
  confirm/reject/interrogation/prompt/reassignment; acknowledge either
  way). JAERO only *names* these types; xng emits structured session
  events.
- AERO-1.2 — Call_announcement (0x21) and T_channel_assignment (0x51):
  0x21 carries an incoming-call channel-pair announcement; JAERO routes
  it through `SendCAssignment`, reusing the C-channel-assignment octet
  layout (AES 2–4, GES 5, rx octets 7/8 → ×0.0025 +1510.0 MHz, tx octets
  9/10 → ×0.0025 +1611.5 MHz, spot-beam flags in the high octets). 0x51
  is the reservation T-channel assignment; JAERO names it and decodes no
  further fields, so xng surfaces the named event with AES/GES only.
- AERO-1.3 — AES system-table broadcast (0x05/0x07/0x0A/0x0C):
  - 0x0C satellite_identification: seqno = (byte3>>2)&0x3F; satid =
    ((byte3<<4)&0x30) | ((byte4>>4)&0x0F); longitude = byte6 × 1.5°
    (>180 ⇒ 360−x west); Psmc1 = ((byte7&0x7F)<<8 | byte8)×0.0025+1510.0
    MHz (spot-beam byte7 bit 7); Psmc2 from byte9/byte10, reported only
    when its channel is non-zero (JAERO rule). Gives the served satellite,
    its orbital longitude, and the P-channel carriers.
  - 0x05 GES Psmc/Rsmc channels: seqno/lsu from byte3 (lsu = byte3&0x03);
    GES = byte4; three 16-bit channels at byte5/6, byte7/8, byte9/10 →
    ×0.0025+1510.0 MHz. The Rsmc (AES-transmit) carriers sit +101.5 MHz
    from the base: lsu≤1 ⇒ {Psmc(RX), Rsmc0(TX), Rsmc1(TX)}; lsu=2 ⇒
    {Rsmc2..4(TX)}; lsu=3 ⇒ {Rsmc5..7(TX)} (JAERO `aerol.cpp`).
  - 0x07 GES_beam_support and 0x0A broadcast_index: named by JAERO with no
    further field decode; surfaced as named events (raw bytes carried).
  byteN above = our su[N-1] (JAERO's 1-based octet indexing).
- AERO-1.4 — remaining P-channel control/user-data types JAERO enumerates
  in `AEROTypeP` (`aerol.h`); only 0x40 carries fields JAERO decodes:
  - 0x40 P_R_channel_control_ISU: GES = octet 5 (su[4]); bit-rate code =
    (byte8>>4)&0x0F (su[7]) mapped through JAERO's table
    {0→600, 1→1200, 2→2400, 3→4800, 4→6000, 5→5250, 6→10500, 7→8400,
    9→21000; 8 and ≥10 reserved → JAERO −1, field omitted}; Pd channel =
    ((byte9&0x7F)<<8)|byte10 (su[8]/su[9]) → ×0.0025+1510.0 MHz; spot-beam =
    byte9 bit 7. Surfaced as the Pd-carrier advert (`pd_mhz`, `bit_rate`,
    `spotbeam`, `ges_id`). (JAERO `aerol.cpp` `P_R_channel_control_ISU`.)
  - 0x28 Data_EIRP_table_broadcast_complete_sequence, 0x41
    T_channel_control_ISU, 0x61 Request_for_acknowledgement (RQA), 0x62
    Acknowledge (RACK/TACK): JAERO names these and decodes no further
    fields; surfaced as named events.
  - 0x74/0x76 User_data_3-/4-octet_LSDU_RLS_P_channel: short LSDU user-data
    types JAERO names but does not run through the ISU/SSU reassembler;
    surfaced as a named `short-lsdu` event carrying the LSDU octet length
    (3 for 0x74, 4 for 0x76).

R-channel control-SU classifier (`su::parse_r_su`, AERO-3): a 19-byte
R-channel SU is a *control* SU when JAERO's user-data flag is clear
(`infofield[1] & 0x08 == 0`, our su[1] bit 3); otherwise it is user data
routed to the ISU/SSU reassembler (`RIsuReassembler`, which now also
enforces the same flag, and whose encoder `build_r_sus` sets it). For a
control SU the message type is the **third** byte (`infofield[2]` = su[2]
— the same byte the user-data path uses for the AES high octet, so AES/GES
do not apply). Types are JAERO's `AEROTypeR` enum (`aerol.h`), surfaced as
named events: 0x20 general access-request (telephone), 0x23 abbreviated
access-request (telephone), 0x22 access-request (data, R/T channel),
0x61 request-for-acknowledgement, 0x62 acknowledgement, 0x12
log-on/log-off control, 0x30 call-progress, 0x15 log-on/log-off
acknowledgement, 0x17 log-control ready-for-reassignment, 0x60
telephony-acknowledge. JAERO only *names* these; xng emits the named
event. R-burst control SUs surface as `AeroEvent`s tagged `Mode::AeroC`
at the burst bit rate.

R-channel SEQINDICATOR → (k, n) (previously flagged for verification) is
now confirmed against JAERO's `RISUData::update` switch (`aerol.cpp`):
1→(1,1), 2→(1,2), 3→(2,2), 4→(1,3), 5→(2,3), 6→(3,3); JAERO's SUindex is
0-based so our 1-based k = SUindex+1. Pinned by
`seq_indicator_matches_jaero_switch`.

Channel/mode tagging (AERO-8.1): each `AeroEvent` carries the physical
channel it came from. The L-band P-channel decoder (`AeroChannelDecoder`)
tags `Mode::AeroL`; the C-band feeder R/T burst decoder
(`AeroBurstDecoder`) tags `Mode::AeroC`. `to_message` propagates
`event.mode` instead of hard-coding `AeroL`, so C-band feeder bursts no
longer mislabel as `aero-l`. (JAERO models these as distinct physical
channels — `AeroL::ChannelType {PChannel, RChannel, TChannel}` on L-band
vs the C-band feeder bursts handled by the burst demodulators.)

Typed SU classifier + bit_rate/channel tag (AERO-8.2): the typed SU
classifier is shared across all three logical channels — `parse_p_su`
runs on P-channel SUs (`AeroChannelDecoder`) and on the P-style SUs
carried inside T bursts (`BurstPacketizer`), while `parse_r_su`
classifies the R-channel control set; the user-data ISU/SSU layer is the
same `Reassembler`/`RIsuReassembler` for P/R/T. Each `AeroEvent` now also
carries an `AeroChannel` (P/R/T): the P-channel decoder emits
`PChannel`; a C-band feeder burst emits `TChannel` for a reserved/TDMA T
burst (6-byte AES/GES header + P-style SUs) or `RChannel` for a
random-access R burst (single 19-byte SU) — mirroring JAERO's
`RTChannelDeleaveFECScram` OK_T_Packet / OK_R_Packet split. `to_message`
injects `channel` (p-/r-/t-channel) and `line_bit_rate` (the physical
frame/burst rate) into the `MessageBody::Aero` details; `line_bit_rate`
is kept distinct from any decoded protocol `bit_rate` field (e.g. the Pd
carrier rate in a 0x40 P/R-control ISU) so the two never clobber.

P-channel 16-bit frame header (`frame::FrameHeader`, AERO-4): the 16 bits
following the 32-bit UW, parsed MSB-first into four JAERO nibbles.
- Oracle: JAERO `aerol.cpp` `AeroL::Decode` assembles the 16 header bits
  into `frameinfo` and splits it as `formatid=(frameinfo>>12)&0xF`,
  `supfrmaker=(frameinfo>>8)&0xF` (superframe marker),
  `framecounter1=(frameinfo>>4)&0xF`, `framecounter2=frameinfo&0xF`. We
  parse the same four fields (`format_id`, `superframe`, `frame_counter1`,
  `frame_counter2`) and surface them in the message `details` as a nested
  `frame_header` object. The framer parses the header off the 16 soft bits
  it already collected after the UW (low rate) / out of the skip region
  (10.5k OQPSK, where the header is the first 16 of the 16+178 skip bits).
- The superframe-lock / AFC-DCD state machine that *consumes* the header
  (JAERO's `FreqOffsetEstimateSlot`) stays a documented follow-up; this
  task only parses and exposes the fields. The `FrameEncoder` round-trips
  through `FrameHeader::to_u16` so the wire word and the decoder's parse
  share one definition (`frame_header_roundtrips_through_encoder`).

Satellite/beam resolution (`satellite::SatelliteResolver`, AERO-2): the
L-band analogue of the HFDL system table (`xng-mode-hfdl::systable`) — a
self-configuring resolver that learns the serving satellite purely from
the AES system-table broadcast SUs (0x0C / 0x07 / 0x05) decoded in AERO-1.3
and tags every message with the resolved satellite + beam.
- Authoritative source is the 0x0C `satellite_identification` broadcast:
  JAERO (`aerol.cpp`,
  `AES_system_table_broadcast_satellite_identification_COMPLETE`) decodes
  `satid`, the orbital `longitude` (`byte6*1.5°`, `>180 ⇒ W`), and the
  Psmc carriers, and only *displays* `"SATELLITE ID = %1 (Long %3)…"` — it
  has **no satellite-name table** — so the resolved identity is the numeric
  `satellite_id` plus its measured `longitude_deg`/`longitude_dir`, taken
  verbatim from the broadcast.
- Beam (global vs spot) is read from the Psmc spot-beam flag JAERO carries
  in the high bit of the carrier's high octet (the `psmc1_spotbeam` field
  the 0x0C handler surfaces). 0x07 `GES_beam_support` (named-only in JAERO)
  sets a `ges_beam_support` presence flag.
- Ocean-region naming is a *nominal best-effort* hint (JAERO does not name
  regions): nearest classic Inmarsat region centre by orbital longitude,
  using the published Inmarsat-3 operational slots — AOR-W ≈ 54°W (F5
  documented at 54°W), POR ≈ 178°E (F3 documented at 178°E), AOR-E ≈ 15.5°W
  and IOR ≈ 64°E (classic region centres). Classified within a ±35°
  tolerance; a satellite far from every slot is left unclassified rather
  than guessed. The measured longitude (from the broadcast) is the ground
  truth; the region is secondary. Surfaced as `resolved_satellite`
  (`satellite_id`, `longitude_deg`, `longitude_dir`, optional `region`) and
  `beam` in the message `details`. The resolver is fed every structured SU
  on the P-channel framer, the 10.5k OQPSK framer, and the C-band burst
  decoder (T-burst P-style SUs can carry system-table broadcasts).

Deliberately out of scope here (noted, not done):
- AERO-4 superframe-lock / AFC-DCD state machine (JAERO's
  `FreqOffsetEstimateSlot`): the header is now parsed and exposed, but the
  state machine that locks the superframe and drives the channel AFC/DCD
  from `superframe`/`frame_counter` is left as the documented follow-up the
  task scopes out.
- AERO-2 satellite **name** mapping: JAERO has no satid→name table, and
  Inmarsat publishes no public stable satid→satellite registry, so the
  resolved identity stays numeric (satid) + measured longitude. The ocean
  region is a nominal longitude hint only; a precise satid→spacecraft map
  would need an external registry we cannot ground.
- C-channel descrambler `dl2` alignment — a demod/DSP off-air scrambler
  offset (`2714−6` bit delay before descramble) that JAERO applies; it is
  invisible to matched loopback and has no public C-channel capture to
  verify against, so it is left flagged (see the C-channel note above)
  rather than guessed.
- `docs/notes/AERO.md` — a repo-level doc outside this crate; left to the
  shared-docs owner.
- 10.5k A-QPSK aero-c burst path (AERO-8.3) and C-channel AMBE→WAV
  (feature-flagged audio) — separate big-bet/DSP tasks.
