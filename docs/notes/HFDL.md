# HFDL (ICAO Annex 10 Vol III Ch. 11 / ARINC 635) — implementation notes

Native HF Data Link decode for `xng-mode-hfdl` (`HfdlChannelDecoder`, one
per channel). Full PHY → burst → PDU chain: M-PSK demod with an LMS
equalizer + decision-directed carrier loop → K=7 r=1/2 Viterbi →
SPDU/MPDU/LPDU/HFNPDU parse → enveloped ACARS via `xng-acars`. Clean-room:
facts from ICAO Annex 10 Vol III Part I Ch. 11 (free PDF, ffac.ch) plus
dumphfdl 1.7.0 source read **for wire layouts only — all code re-derived**
per docs/REFERENCES.md. See PROVENANCE.md.

On the full sigidwiki 21931 kHz Riverhead capture xng decodes 36 events
vs dumphfdl 1.7.0's 37 (97%; the residual single frame is the weakest
4–5 dB / 300 bps burst, a sensitivity tail, not a convention bug — the
frame-exact diff shows the data LPDUs match the oracle one-for-one). The
final 89%→97% step came from the decode-stage rescue (below). An 8 s
slice is the vendored CI fixture (`tests/offair.rs` pins the squitter
field-for-field); the full capture is fenced separately by `bench/run.sh`
(`bench/baselines.json` floor `hfdl_offair`). Crate:
`crates/xng-mode-hfdl/src/`.

## Pipeline

wideband IQ → `xng_dsp::Ddc` (channel + 1440 Hz subcarrier, decimate to
`CHANNEL_RATE` 12 kS/s ≈ 6.67 samples/sym, ±1.5 kHz passband) — or, when
the input is already 12 kS/s on-channel, a **no-DDC path** that applies
the same ±1.5 kHz selectivity FIR (101-tap lowpass) so the demod never
sees the full ±6 kHz of noise (+4.5–5 dB measured) → `demod::HfdlDemod`
(burst hunt, A1/A2/M1 acquisition, equalize+track, Viterbi) →
`pdu::PduParser::parse` (SPDU vs MPDU → LPDU → HFNPDU → ACARS) →
`HfdlEvent` → `to_message` → `xng_types::Message`. One `HfdlDemod` and one
`PduParser` per channel (the per-channel keying that scopes the aircraft
cache).

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
  0o171)`), same as Aero — verified against the 21931 kHz capture
  (171-first yields no valid FCS; 133-first matches dumphfdl
  field-for-field). Encoder zero-start/flush; decoder traceback to 0.
- Deinterleaver (`fec::deinterleave`): 40 rows × C cols, C = chips/40 ∈
  {54,108,162} single, {126,252,378} double. Push column shift **S = 17
  single / 23 double**; pop row step 9. Both index sequences generated
  and the table inverted; `interleave` is the TX inverse. A unit test
  asserts both are permutations and round-trip.
- After Viterbi the decoded bits are packed **LSB-first per byte**
  (net of the dumphfdl MSB-pack + byte bit-reversal) for all PDU layouts.

## Receive demod (`demod.rs`)

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
   scale-invariant metric; gate `CORR_M1 = 0.4`) → rate/slot setting.
4. **Carrier** — A1→A2 coherent phases (127 symbols apart) refine the
   per-symbol rotation; coherent-correlation sign resolves the global π.
5. **Equalize + track** — a **symbol-spaced 7-tap LMS feed-forward
   equalizer** (identity-initialized at the center tap, the delay line
   advanced one sample per symbol, decision at the window center, trained
   on the 9 preamble T segments and retrained on every embedded T segment)
   plus a 2nd-order **decision-directed carrier loop** on every symbol.
   The DD loop matters more than the taps: the A1→A2 refinement aliases at
   ±π/127/symbol, leaving up to ~0.025 rad/symbol the loop tracks out.
   (VERIFY-5: the as-built count is **7 SYMBOL-spaced** taps;
   dumphfdl's documented 15 are **T/2 (half-symbol)-spaced** because its
   input is matched-filtered at 2 samples/symbol while ours is not. Both
   are correct — they describe different sample geometries, so the tap
   counts are not directly comparable. Any stale "15 taps" reading should
   be understood as T/2-spaced, not a discrepancy.)
6. Per data segment: 30 data symbols (descramble π flips, Gray soft
   demod) + T retrain → deinterleave → (rate-1/4 average) → Viterbi →
   LSB-first bytes → PDU parse.
7. **Decode-stage rescue** — when every PDU CRC in a detected burst
   fails, re-run the demod over small timing (±0.5/±1 sample) and carrier
   (±0.007/±0.017 rad/symbol) offsets, the PDU header CRC arbitrating
   (only on failed bursts). Transplanted from the AIS deep-weak lesson.

**Per-burst quality figures** (HFDL-5): every demod-path event is stamped
with three measured figures.

- **fec_corrected** — the decoded bits are re-encoded through the same
  convolutional code (`Viterbi::encode`) and the Hamming distance to the
  received hard decisions — the nearest-codeword distance, exactly the
  symbols the Viterbi corrected — is counted. Crate-local (no `xng-dsp`
  change); clean burst → 0, never via parser loopback.
- **snr_db** — an EVM-derived SNR: the ratio of mean equalized-symbol
  power to residual error power on the KNOWN embedded T training symbols,
  `10·log10(S/N)`. Accumulated only on the post-convergence embedded T
  segments (the 9 preamble T segments are excluded — the equalizer and
  carrier loop are still converging there and would understate the
  steady-state SNR). `None` if no training symbols were processed.
- **freq_skew_hz** — the carrier frequency offset the demod actually
  removed: the acquisition rotation `theta` plus the steady-state
  decision-directed loop frequency `carr_fr` (rad/symbol), converted at
  the symbol rate (`f = rate · rad / 2π`). `theta` is referenced to
  `a1_pos`, so it already excludes the +1440 Hz subcarrier removed
  upstream. Measured, never assumed.

Events built directly from bytes (tests, reassembled tables) carry `None`
for all three.

## Link layer (all FCS = CRC-16/X-25, `HDLC_FCS`, LE trailer)

PDU type: first octet bit0: 0 = SPDU, 1 = MPDU (`PduParser::parse`).

**SPDU** (`parse_spdu`, 66 octets, FCS over first 64): gs_id + gs_name
(`gs_name` roster, see below), utc_sync, 12-bit frame_index,
frame_offset, then the **first-octet flags** decoded field-for-field with
dumphfdl `spdu.c` (HFDL-6): `rls_in_use` (bit 1), `spdu_version` (bits
2–3), `iso8208_supported` (bit 5), `change_note` (bits 6–7). `min_priority`,
12-bit `systable_version`, 20-bit `freqs_in_use` bitmap, neighbor GS2/GS3
id+freq. The **48-octet per-slot assignment / TDMA reservation region**
`buf[4..52)` — the single largest span dumphfdl leaves opaque (it reads
the header to `buf[3]` then jumps to `min_priority` at `buf[52]`); no
public spec defines its subfield layout — is surfaced verbatim as
`slot_assignment_hex`, raw rather than fabricated.

**MPDU** (`parse_mpdu`): downlink (`[0]&2`) carries LPDU count, dst gs_id,
1-byte aircraft id, per-LPDU size octets, header FCS; uplink carries
n_aircraft, src gs_id, per-aircraft {id, count, size octets}. LPDUs
follow, each with its own trailing FCS.

**LPDU** (`parse_lpdu`, type byte = first octet, `lpdu_type_name`):

| Type | Name | Decoded fields |
|---|---|---|
| `0x0D` | unnumbered data | → HFNPDU follows |
| `0x1D` | unnumbered ack'ed data | → HFNPDU follows |
| `0x8F` | logon request (normal) | ICAO (24-bit, re-`reverse_bits`) |
| `0xBF` | logon request (DLS) | ICAO |
| `0x4F` | logon resume | ICAO |
| `0x9F` | logon confirm | ICAO + assigned channel-local id → AC cache |
| `0x5F` | logon resume confirm | ICAO + assigned id → AC cache |
| `0x2F` | logon denied | ICAO + reason (`logon_denied_reason`); evicts cache |
| `0x3F` | logoff request | ICAO + reason (`logoff_reason`); evicts cache |
| other | (named only) | `lpdu` event with dumphfdl type name |

**Aircraft-ID → ICAO cache** (`ac_cache.rs`, HFDL-3): HFDL aircraft IDs
are 1-byte, channel-local, GS-assigned. Each logon-(resume-)confirm binds
its assigned id to the ICAO; later downlinks bearing that id are
back-filled with the resolved ICAO (`who.icao`). TTL-expires (default
3600 s = dumphfdl `AC_CACHE_TTL_DEFAULT`, overridable via
`PduParser::with_ac_cache_ttl`); evicted on logoff/logon-denied;
per-channel keying (one `PduParser` per channel ≡ dumphfdl's (freq,
ac_id)). A re-logon under a new id drops the stale mapping.

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
  with its cause table (`freq_change_cause`). The fix also feeds the
  normalized `details["position"]` object (HFDL-4, Outputs).
- **0xD5 frequency data** — flight id, lat/lon, UTC, then up to 6 per-GS
  {gs_id+name, 20-bit prop_freqs, 20-bit tuned_freqs} records. Same
  position header as 0xD1, also lifted into `details["position"]`
  (HFDL-4).
- **0xD0 system table partial** — emits `systable-partial` (seq, total,
  12-bit version) and feeds the reassembler (below).
- **0xD2 system table request** (16-bit request_data), **0xDE delayed
  echo** (no body), named.
- Unknown HFNPDU types emit an `hfnpdu` event with the dumphfdl type name.
- **Parser policy:** a CRC-valid data LPDU is never dropped — an
  unparsable HFNPDU emits an `unnumbered-data` envelope event with the
  payload hex (cost 4+ frames on the bench when it was a silent drop).

## System table (`systable.rs`, reassembled 0xD0)

Partials keyed by (version, total) accumulate in `SystableAssembler`
until every sequence is present, then concatenate and `parse_stations`
as consecutive GS records — per GS: id+utc_sync (`[0]` bit 7), 20-bit
lat/lon, 3-bit SPDU version, freq count (≤20, the SPDU 20-bit-bitmap
bound), per freq 3 octets BCD (100 Hz units, nibbles low→high) +
master-frame-slot nibble. A new partial that disagrees on version or set
size discards the partial set; a same-slot repeat with new content trusts
the newest copy. Each decoded `GroundStation` is enriched with `gs_name`
from the built-in roster (HFDL-6) — the in-memory field dumphfdl emits
that the crate previously dropped — populated on decode, `None` for an
unassigned id. Version is 12-bit wrapping (`version_is_newer`: newer if
(new−old) mod 4096 < 2048). A complete reassembly emits `systable-complete`
with the full serialized `SystemTable`.

### System-table persistence (`SystemTable::save` / `load`, HFDL-2.1)

`SystemTable` derives both serde `Serialize` and `Deserialize` and exposes
a load/save pair for **cold-start enrichment**: a long-running receiver
saves the most recent reassembled 0xD0 table so a later run starts with
known GS positions/frequencies instead of waiting for the next
over-the-air set.

| API | Form | Behavior |
|---|---|---|
| `SystemTable::save(path)` | method | pretty-JSON write, `io::Result<()>` |
| `SystemTable::load(path)` | method | JSON read → `SystemTable` |
| `save_system_table(table, path)` | free fn | alias of `save` (task/CLI API name) |
| `load_system_table(path)` | free fn | alias of `load` |

Persisted as pretty JSON through the crate's existing serde_json channel —
the serde equivalent of dumphfdl's libconfig `systable_save_config()` /
`systable_read_from_file()` (`src/systable.c`), JSON rather than libconfig
so it needs no new dependency. The persisted field set is the same: GS id
(0..=127), optional name (`skip_serializing_if`/`serde(default)` so a
nameless station omits the key and loads back as `None`), lat/lon,
frequencies, table version. **The `--system-table` CLI flag to wire this
into the binary is the documented follow-up — not wired here.**

## GS-name roster (`pdu::gs_name`, HFDL-2.2)

Built-in ground-station name table keyed by the 7-bit GS id (valid range
0..=127), used by SPDU, MPDU, performance/frequency data, and system-table
decode. The roster is the published HFDL/ARINC list, **verified id-for-id
against dumphfdl's distributed `etc/systable.conf`** (szpajder/dumphfdl,
GPL — facts only). It assigns **exactly ids 1..=11 and 13..=17**; **id 12
is the only hole inside that span and ids 18..=127 are unassigned** —
those return `None` rather than a fabricated name (correcting any prior
"12 holes to fill up to 127" framing: there are no published assignments
there). When the official roster adds stations, the id→name pairs are
added here.

| id | name | id | name |
|---|---|---|---|
| 1 | San Francisco, USA | 10 | Muan, South Korea |
| 2 | Molokai, Hawaii | 11 | Albrook, Panama |
| 3 | Reykjavik, Iceland | 12 | _(unassigned)_ |
| 4 | Riverhead, New York | 13 | Santa Cruz, Bolivia |
| 5 | Auckland, New Zealand | 14 | Krasnoyarsk, Russia |
| 6 | Hat Yai, Thailand | 15 | Al Muharraq, Bahrain |
| 7 | Shannon, Ireland | 16 | Agana, Guam |
| 8 | Johannesburg, South Africa | 17 | Canarias, Spain |
| 9 | Barrow, Alaska | 18..127 | _(unassigned)_ |

(id 1 reads "San Francisco, USA" vs the conf's "San Francisco, California"
— both name KSFO; the crate's string is the one already asserted across
the crate's other tests and is left unchanged to avoid cosmetic churn.)

## Outputs

`to_message` maps each event to the normalized `Message`: ACARS events
carry the parsed `AcarsBlock` (crc_ok, parity errors); all other kinds
become `MessageBody::Hfdl { kind, details }` (kinds: `squitter`,
`logon-request`/`-confirm`/`-resume`/`-denied`, `logoff-request`,
`unnumbered-data`, `performance-data`, `frequency-data`, `acars`,
`systable-partial`/`-complete`, `systable-request`, `delayed-echo`,
`hfnpdu`/`lpdu`). Each burst-derived event also carries the
demod-measured quality figures (HFDL-5): `decode.fec_corrected`, plus
`signal.snr_db` (EVM-derived), `signal.freq_skew_hz` (CFO), and
`signal.rssi_db` (level dBFS). All three are stamped onto every event a
burst produces in `process` (`lib.rs`); byte-built events (tests,
reassembled tables) leave them `None` — never fabricated.

### HFDL-4 aircraft positions (`details["position"]`)

The position-bearing HFNPDUs — **0xD1 performance-data and 0xD5
frequency-data** — carry an aircraft fix (20-bit lat/lon, the UTC
half-second-of-day counter, flight id) at the same fixed offsets.
`pdu::position_obj` lifts these into a normalized `details["position"]`
object `{lat, lon, utc_s, utc, flight}` on those two event kinds. The
all-zero placeholder `(0,0)` is suppressed (returns `None`): an aircraft
without a GPS fix transmits zeros, and dumphfdl likewise treats it as "no
position", so no null-island fix is planted. The 8-bit GS-local downlink
`aircraft_id` and — when resolved — the `icao` are copied verbatim from
`who` onto the position object. The ICAO comes from the logon-confirm
aircraft-ID cache (the HFDL-3 `ac_cache` `resolved()` back-fill on
downlink LPDUs, below), so a fix only carries an ICAO once the aircraft
has logged on.

The embedded dashboard's map adapter (the XM-2.2 `position->map` path in
`outputs/http.rs`, `MessageBody::Hfdl` arm) reads `details["position"]`
and merges the fix into the shared aircraft table by ICAO — the same
master entity as 1090/UAT ADS-B and ACARS, so one aircraft heard on
several carriers coalesces into one map marker. **Events with no resolved
ICAO are skipped** (the arm only upserts when a 6-hex ICAO is present), so
an unkeyed fix never plants a phantom entity. The **SBS-1/Beast feed and
a `--freq-as-squawk` option are still open** — those outputs currently
carry ADS-B Mode S only.

## Validation / oracles

- **Oracle: dumphfdl 1.7.0.** No public unit vectors. The sigidwiki
  21931 kHz Riverhead IQ recording (CC BY-SA, skip.land 2024-11-05,
  127 s) is ground truth. An 8 s slice is the CI fixture
  (`tests/data/hfdl_21931khz_8s.i16`); `tests/offair.rs` pins the
  squitter field-for-field — GS 4 Riverhead, frame index 2397, offset 1,
  systable version 52, utc_sync, the first-octet flags (real byte0 = 0x10
  → rls/iso clear, version 0, change_note 0) and the 48-octet
  reservation region taken verbatim from the off-air bytes, plus
  `fec_corrected` presence. Full-capture haul: 36 events / 97% of
  dumphfdl's 37 (logon confirms, ACARS, performance-data downlinks with
  live positions) — the decode-stage rescue lifted it from 33 (89%).
- **Field layouts** pinned to dumphfdl byte offsets in `pdu.rs` unit
  tests: performance-data and frequency-data full records, 0xD2/0xDE/
  0x2F/0x3F naming + reason tables, AC-cache resolve/evict/TTL, the
  SPDU first-octet flags and slot region against `spdu.c`. A regression
  surfaces as a mismatch against the reference layout.
- **GS-name roster** pinned id-for-id by
  `gs_name_roster_matches_published_list` (`pdu.rs`): every assigned id →
  its `systable.conf` name; every other id in 0..=127 → `None`; id 12
  asserted as the only hole in 1..=17.
- **System table** (`systable.rs`): reassembly out-of-order,
  version-change discard, BCD frequency, malformed-body rejection, name
  enrichment (id 1 → San Francisco, id 13 → Santa Cruz, id 12 → `None`),
  and the **persistence round-trip** (`systable_persistence_round_trip`):
  a table built by running real GS records through `parse_stations` is
  saved and reloaded via both the method and free-function forms; integer/
  string/bool fields asserted equal, coordinates to <1e-6° (text f64
  round-trip is exact in practice but not guaranteed by the serde
  contract), a save→load cycle shown to be a bit-exact fixed point, a
  nameless station shown to omit `gs_name` and reload as `None`.
  `systable_load_missing_file_errors` covers the I/O-error path. This is
  a persistence round-trip, not a decode oracle — the decode is grounded
  against dumphfdl elsewhere.
- **Synthetic TX→RX loopback** (`modulate.rs` + `tests/end_to_end.rs`):
  SPDU @300, ACARS @600/1200/1800, wideband-capture path, `fec_corrected`
  = 0 clean / >0 under noise.
- Live: any HF antenna; ground stations worldwide (public system table,
  also learned over the air from 0xD0).

## Known limitations / intentional gaps

- Residual gap vs dumphfdl on the bench capture: one frame (the weakest
  4–5 dB 300-bps burst), a sensitivity tail. Standing falsifications:
  wider ±2/±3-sample retry shifts gain nothing; lowering the A1 gate
  0.4 → 0.32 is catastrophic (false anchors consume real bursts, dropping
  to 19 events).
- **`--system-table` CLI flag not wired** — the persistence API exists
  (`save`/`load` + free-function aliases) and round-trips, but loading a
  saved table into the running decoder at startup, and choosing the path,
  is a follow-up.
- SPDU per-slot assignment codes `buf[4..52)` surfaced raw
  (`slot_assignment_hex`), not parsed — no public spec defines the
  subfield layout.
- LMS is 7-tap **symbol-spaced** (identity-init); dumphfdl's documented
  15 are **T/2-spaced** lowpass-init (its input is matched-filtered at
  2 samples/symbol, ours is not). This is a spacing-convention difference,
  not a tap shortfall (VERIFY-5 resolved); the DD carrier loop + rescue
  cover the difference at the SNRs that matter here.
- Channel rate fixed at 12 kS/s (6.67 samples/symbol). Raising to
  24 kS/s tested worse — HFDL's marginal frames are fading/SNR-bound, not
  timing-resolution-bound; the LMS+DD loop owns that domain.
- The persisted table is not auto-loaded for `gs_name` (the roster is
  built-in and complete); persistence is for GS **positions/frequencies**
  learned over the air, pending the CLI flag.

## References

- ICAO Annex 10 Vol III Part I Ch. 11 (normative PHY, free PDF).
- ARINC 635 (HFDL system definition).
- dumphfdl 1.7.0 (GPL — facts only: src/spdu.c, hfnpdu.c, lpdu.c,
  ac_cache.c, systable.c, util.c; `etc/systable.conf` for the GS roster;
  the compiled binary as off-air ground truth).
- libacars (ACARS/ARINC 622, via `xng-acars`).
- sigidwiki / skip.land 21931 kHz IQ recording (off-air ground truth).
- PROVENANCE.md — sourcing policy and per-pass oracle notes.
