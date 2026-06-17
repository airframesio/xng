# HFDL (ICAO Annex 10 Vol III Ch. 11 / ARINC 635) — implementation notes

Facts from ICAO Annex 10 Vol III Part I Ch. 11 (free PDF, ffac.ch) plus
dumphfdl 1.7.0 source read for facts only (GPL — wire layouts only, all
code re-derived per docs/REFERENCES.md). See PROVENANCE.md.

On the sigidwiki 21931 kHz capture xng decodes 36 events vs dumphfdl's
37 (97%); CI bench floor is 31. The residual gap is the weakest bursts
(4.0–5.0 dB SNR at 300 bps) — a sensitivity tail, not a convention bug;
the frame-exact diff shows the data LPDUs match the oracle one-for-one.
Crate: `xng-mode-hfdl` (`HfdlChannelDecoder`, one per channel).

## PHY

- USB channel; audio subcarrier = SSB carrier + **1440 Hz**
  (`SUBCARRIER_OFFSET_HZ`; ITU "assigned frequency" = carrier + 1400 Hz).
  Band 2.8–22 MHz, 1 kHz tuning. Emission 2K80J2DEN.
- M-PSK at **1800 symbols/s ±10 ppm**: M=2 → 300/600 bps (by code rate),
  M=4 → 1200, M=8 → 1800. Pulse: SRRC α=0.31. Receiver must handle
  subcarrier offset ±70 Hz, 5 ms multipath, 2 Hz Doppler.
- Gray ring mapping (phase position n carries Gray label n⊕(n>>1)):
  0°: 0/00/000, 45° 001, 90° 01/011, 135° 010, 180° 1/11/110, 225° 111,
  270° 10/101, 315° 100. Phase ref from preamble; residual π ambiguity
  resolved by A-correlation sign (global bitmask flips everything).
  Per-bit soft decisions are min-distance over the ring (`gray_soft`).
- 300 bps: rate-1/4 = each rate-1/2 chip transmitted twice; after
  deinterleave the soft pair is averaged.

## Burst (PPDU) anatomy — symbols

| Segment | Symbols | Content |
|---|---|---|
| Pre-key | 448 | unmodulated key-up |
| A1, A2 | 127 + 127 | fixed BPSK PN sequence (below), repeated |
| M1 | 127 | one of 8 cyclic shifts of the M sequence → rate+slot |
| M2 | 15 | continuation: (shift+127+j) mod 127 = M1's first 15 chips |
| EQ train | 9×15 | nine T segments |
| Data | N×(30+15) | 30 data symbols + 15-symbol BPSK T training, N=72 (single) / 168 (double) |

Totals: single 4219 sym (2.344 s), double 8539; slot = 32/13 s ≈ 4430.8
symbols; 13 slots per 32 s TDMA frame, slot 0 = SPDU squitter.

**A sequence (127 bits, 0 = +1/0°)** and **M base sequence (127 bits)**
are encoded verbatim in `fec.rs` (`A_BITS`, `M_BITS`); `T_BITS` =
`000100110101111` (15 BPSK chips, 0x9AF MSB-first).

**M shifts → settings** (`fec::SETTINGS`, M1-shift order): {72:300/S,
82:600/S, 113:1200/S, 123:1800/S, 61:300/D, 103:600/D, 93:1200/D,
9:1800/D}. Coded chips per burst: 2160/2160/4320/6480 (S),
5040/5040/10080/15120 (D). Decoded payload bits: 540/1080/2160/3240 (S),
1260/2520/5040/7560 (D). All derived in `Setting::chips/payload_bits`.

## Scrambler

x^15+x+1 (`xng_dsp::scramble::Lfsr15`, init 0x6959 — the VDL2/Aero
state), **truncated to 120 bits then reset** (`fec::scramble_flips`).
Applied per DATA SYMBOL at the modulation layer: LFSR bit 1 → rotate
symbol by π. 120 tiles exactly into 2160 and 5040 data symbols, so state
resets align with burst ends.

## FEC + interleaver

- Convolutional K=7 rate 1/2, classic 171/133 octal. **Pair order
  confirmed off-air: 133-output first** (`Viterbi::new(7, 0o133,
  0o171)`), same as Aero — verified against the sigidwiki 21931 kHz
  capture (171-first yields no valid FCS; 133-first matches dumphfdl
  field-for-field). Encoder zero-start/flush; decoder traceback to 0.
- Deinterleaver (`fec::deinterleave`): 40 rows × C cols, C = chips/40 ∈
  {54,108,162} single, {126,252,378} double. Push column shift **S = 17
  single / 23 double**; pop row step 9. Both index sequences generated
  and the table inverted; `interleave` is the TX inverse. A unit test
  asserts both are permutations and round-trip.
- After Viterbi the decoded bits are packed **LSB-first per byte**
  (net of the dumphfdl MSB-pack + byte bit-reversal) for all PDU layouts.

## Receive pipeline

DDC (channel + 1440 Hz, decimate to `CHANNEL_RATE` 12 kS/s ≈ 6.67
samples/symbol, ±1.5 kHz passband) — or, when the input is already at
12 kS/s and on-channel, a no-DDC path that applies the same ±1.5 kHz
**selectivity FIR** (101-tap lowpass) so the demod never sees the full
±6 kHz of noise (+4.5–5 dB measured). Then per burst (`demod.rs`):

1. **Hunt** — energy gate (asymmetric noise-floor tracker, fall-fast/
   rise-slow so a sweep can't raise the gate above a burst), then
   differential correlation against the 127-chip A sequence
   (`a_diff_correlate`, CFO-immune; gate `CORR_A1 = 0.4`). The
   differential metric also yields the per-symbol carrier rotation.
2. **Coherent A1 joint fit** (`a1_fit`) — over a symbol-denominated fine
   timing grid (±0.6 symbol, 0.25-sample step), the 127 known BPSK chip
   phases (signs removed, differential rotation pre-subtracted) are
   unwrapped and fit to residual ≈ a + b·k weighted by sample energy.
   The minimum-cost grid point gives timing and per-symbol CFO jointly —
   no 2π/127-per-symbol aliasing (the same coherent sync that recovered
   the VDL2 XID bursts). Replaced the earlier quarter-sample search.
3. **M1 shift** matched against all 8 cyclic shifts (energy-normalized,
   scale-invariant metric) → rate/slot setting.
4. **Carrier** — A1→A2 coherent phases (127 symbols apart) refine the
   per-symbol rotation; coherent-correlation sign resolves the global π.
5. **Equalize + track** — a **7-tap symbol-spaced LMS feed-forward
   equalizer** (identity-initialized, decision at the window center,
   trained on the 9 preamble T segments and retrained on every embedded
   T segment) plus a 2nd-order **decision-directed carrier loop** on
   every symbol. The DD loop matters more than the taps: the A1→A2
   refinement aliases at ±π/127/symbol, leaving up to ~0.025 rad/symbol
   the loop tracks out.
6. Per data segment: 30 data symbols (descramble π flips, Gray soft
   demod) + T retrain → deinterleave → (rate-1/4 average) → Viterbi →
   LSB-first bytes → PDU parse.
7. **Decode-stage rescue** — when every PDU CRC in a detected burst
   fails, re-run the demod over small timing (±0.5/±1 sample) and carrier
   (±0.007/±0.017 rad/symbol) offsets, the PDU header CRC arbitrating
   (only on failed bursts; ~20 extra finishes). Transplanted from the
   AIS deep-weak lesson; lifted the off-air haul 89% → 97%.

**fec_corrected** (HFDL-5): the decoded bits are re-encoded through the
same convolutional code (`Viterbi::encode`) and the Hamming distance to
the received hard decisions — i.e. the nearest-codeword distance, exactly
the symbols the Viterbi corrected — is stamped on every demod-path event.
Crate-local; clean burst → 0, never via parser loopback.

## Link layer (all FCS = CRC-16/X-25, `HDLC_FCS`, LE trailer)

PDU type: first octet bit0: 1 = MPDU, 0 = SPDU (`PduParser::parse`).

**SPDU** (`parse_spdu`, 66 octets, FCS over first 64): gs_id + gs_name
(`gs_name`, ARINC station list, ids 1–17), utc_sync, 12-bit frame_index,
frame_offset, change_note, min_priority, 12-bit systable_version, 20-bit
freqs-in-use bitmap, and the neighbor GS2/GS3 id+freq fields. (Per-slot
assignment codes [4..51] not parsed.)

**MPDU** (`parse_mpdu`): downlink (`[0]&2`) carries LPDU count, dst gs_id,
1-byte aircraft id, per-LPDU size octets, header FCS; uplink carries
n_aircraft, src gs_id, per-aircraft {id, count, size octets}. LPDUs
follow, each with its own trailing FCS.

**LPDU** (`parse_lpdu`, type byte = first octet, `lpdu_type_name`):
- `0x0D` unnumbered data, `0x1D` unnumbered ack'ed data → HFNPDU follows
- `0x8F` logon request (normal), `0xBF` logon request (DLS) → emits ICAO
  (24-bit, MSB-first in raw air order, re-reversed via `reverse_bits`)
- `0x4F` logon resume, `0x9F` logon confirm / `0x5F` logon resume confirm
  (carries assigned channel-local aircraft id)
- `0x2F` logon denied (ICAO + reason, `logon_denied_reason`)
- `0x3F` logoff request (ICAO + reason, `logoff_reason`)
- unknown types emit an `lpdu` event carrying the dumphfdl type name.

**Aircraft-ID → ICAO cache** (`ac_cache.rs`, HFDL-3): HFDL aircraft IDs
are 1-byte, channel-local, GS-assigned. Each logon-(resume-)confirm binds
its assigned id to the ICAO; later downlinks bearing that id are
back-filled with the resolved ICAO (`who.icao`). TTL-expires (default
3600 s, dumphfdl `AC_CACHE_TTL_DEFAULT`); evicted on logoff/logon-denied;
per-channel keying (one PduParser per channel ≡ dumphfdl's (freq, ac_id)).

## HFNPDU layer (`parse_hfnpdu`, [0]=0xFF, [1]=type, `hfnpdu_type_name`)

- **0xFF enveloped ACARS** — [2] = SOH then a standard parity-bearing
  ACARS block (`xng_acars::block::parse`, BCS CRC-16 KERMIT); labels,
  sublabels, and ARINC 622 apps (ADS-C, CPDLC, …) come from the shared
  ACARS application layer.
- **0xD1 performance data** — full 47-octet record: flight id, 20-bit
  lat/lon (×180/2^19), UTC (half-second counter → h/m/s), version,
  flight_leg, gs_id+name, freq_id, per-leg freq_search_cnt and
  hfdl_disabled_duration, per-bitrate {300,600,1200,1800} MPDU
  rx/rx_err/tx/delivered counters, spdus_rx/_errs, and freq_change_code
  with its cause table (`freq_change_cause`).
- **0xD5 frequency data** — flight id, lat/lon, UTC, then up to 6 per-GS
  {gs_id+name, 20-bit prop_freqs, 20-bit tuned_freqs} records.
- **0xD0 system table partial** — emits `systable-partial` (seq, total,
  12-bit version) and feeds the reassembler (below).
- **0xD2 system table request** (16-bit request_data), **0xDE delayed
  echo** (no body), named.
- Unknown HFNPDU types emit an `hfnpdu` event with the dumphfdl type name.
- **Parser policy:** a CRC-valid data LPDU is never dropped — an
  unparsable HFNPDU emits an `unnumbered-data` envelope event with the
  payload hex (cost 4+ frames on the bench when it was a silent drop).

**System table** (`systable.rs`, reassembled 0xD0): partials keyed by
(version, total) accumulate until every sequence is present, then
concatenate and parse as consecutive GS records — per GS: id+utc_sync,
20-bit lat/lon, 3-bit SPDU version, freq count (≤20), per freq 3 octets
BCD (100 Hz units, nibbles low→high) + master-frame-slot nibble. Version
is 12-bit wrapping (`version_is_newer`: newer if (new−old) mod 4096 <
2048). A version/size change discards the partial set.

## Outputs

`to_message` maps each event to the normalized `Message`: ACARS events
carry the parsed `AcarsBlock` (crc_ok, parity errors); all other kinds
become `MessageBody::Hfdl { kind, details }` (kinds: `squitter`,
`logon-request`/`-confirm`/`-resume`/`-denied`, `logoff-request`,
`unnumbered-data`, `performance-data`, `frequency-data`, `acars`,
`systable-partial`/`-complete`, `systable-request`, `delayed-echo`,
`hfnpdu`/`lpdu`). `fec_corrected` and rssi (level dBFS) ride along.

## Validation

- **Oracle: dumphfdl 1.7.0.** No public unit vectors; the sigidwiki
  21931 kHz Riverhead IQ recording (CC BY-SA, skip.land 2024-11-05,
  127 s) is the ground truth. An 8 s slice is vendored as a CI fixture
  (`tests/data/hfdl_21931khz_8s.i16`) and `tests/offair.rs` pins the
  squitter field-for-field (GS 4 Riverhead, frame index 2397, offset 1,
  systable version 52, utc_sync) plus fec_corrected presence.
- **Field layouts** pinned to dumphfdl byte offsets in unit tests
  (`pdu.rs`): performance-data and frequency-data full records, 0xD2/
  0xDE/0x2F naming + reason tables, AC-cache resolve/evict/TTL,
  systable reassembly/wraparound/BCD. A regression surfaces as a
  mismatch against the reference layout.
- **Synthetic TX→RX loopback** (`modulate.rs` + `tests/end_to_end.rs`):
  SPDU @300, ACARS @600/1200/1800, wideband-capture path, fec_corrected
  = 0 clean / >0 under noise.
- Live: any HF antenna; ground stations worldwide (public system table,
  also learned over the air from 0xD0).

## Known limitations / intentional gaps

- 1-frame residual vs dumphfdl on the bench capture: weakest 4–5 dB
  300-bps bursts (sensitivity tail). Standing falsifications: wider
  ±2/±3-sample retry shifts gain nothing; lowering the A1 gate 0.4 →
  0.32 is catastrophic (false anchors consume real bursts → 19 events).
- SPDU per-slot assignment codes [4..51] not parsed.
- LMS is 7-tap symbol-spaced (identity-init), not dumphfdl's 15-tap
  T/2-spaced lowpass-init form; the DD carrier loop + rescue cover the
  difference at the SNRs that matter here.
- Channel rate fixed at 12 kS/s (6.67 samples/symbol). Raising to
  24 kS/s tested worse — HFDL's marginal frames are fading/SNR-bound,
  not timing-resolution-bound; the LMS+DD loop owns that domain.

## References

- ICAO Annex 10 Vol III Part I Ch. 11 (normative PHY, free PDF).
- ARINC 635 (HFDL system definition).
- dumphfdl 1.7.0 (GPL — facts only: src/hfnpdu.c, lpdu.c, ac_cache.c,
  util.c, systable handling).
- libacars (ACARS/ARINC 622, via `xng-acars`).
- sigidwiki / skip.land 21931 kHz IQ recording (off-air ground truth).
