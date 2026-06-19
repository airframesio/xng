# Provenance — xng-mode-hfdl

Clean-room implementation from the protocol facts in docs/notes/HFDL.md:
ICAO Annex 10 Vol III Part I Chapter 11 (normative PHY, freely
published) cross-verified against dumphfdl (GPL-3 — **facts only; all
code re-derived** per docs/REFERENCES.md policy).

Key facts encoded: M-PSK 1800 sym/s Gray ring, +1440 Hz subcarrier,
SRRC α=0.31; burst anatomy (448-symbol pre-key, A1/A2 127-chip PN, M1
cyclic-shift rate signalling {72,82,113,123,61,103,93,9}, 15-symbol T
training 0x9AF); per-symbol π-flip scrambler (shared LFSR15 init,
120-bit truncation); K=7 171/133 convolutional code with rate-1/4 chip
doubling; 40×C interleaver (push column shift 17/23, pop row step 9);
byte bit-reversal between Viterbi and PDU layers; SPDU/MPDU/LPDU/HFNPDU
layouts with CRC-16/X-25 FCS; HFNPDU 0xFF enveloped ACARS through
xng-acars::block; 20-bit coordinate scaling ×180/2^19.

v1 demodulator divergence (documented): per-T-segment phase
re-estimation instead of dumphfdl's 15-tap LMS equalizer — adequate for
clean/strong signals and loopback; the equalizer is the planned upgrade
for real HF multipath. Scrambler output-bit convention and PDU bit
order flagged for verification against off-air captures (sigidwiki IQ
samples decodable by dumphfdl as ground truth).

## Off-air validation (2026-06)

Validated against the sigidwiki 21 931 kHz IQ recording (CC BY-SA,
skip.land, 2024-11-05; 127 s) with dumphfdl 1.7.0 as ground truth
(37 frames decoded from the same file). Three real-signal fixes came
out of it, none visible to synthetic loopback:

1. **Coherent fine timing after the differential A1 hunt.** The
   differential metric's peak can sit ~3 samples (0.45 symbol) off true
   symbol timing — enough to null the coherent M1 correlation at 6.67
   samples/symbol. A quarter-sample search on the coherent A correlation
   fixes acquisition (M1 metrics 0.97+ on real bursts).
2. **Scale-invariant correlation gates.** Coherent metrics were
   amplitude-scaled (calibrated to unit-level synthetic signals); the
   real capture sits at ~0.08 amplitude. Gates now normalize by window
   energy.
3. **Coded pair order is 133-output first** (libcorrect convention),
   matching the same finding on Aero. With 171-first nothing passes FCS;
   with 133-first the SPDU squitter matches dumphfdl field-for-field
   (GS 4, frame index 2397, offset 1, systable version 52) and the full
   capture yields 28 events: logon confirms (ICAO 040087, 04C11B), ACARS
   (N538AV, CC-BBF, N401AV, CS-TSF), and performance-data downlinks with
   live positions (CM0498, LP2482).

An 8 s slice is vendored as a CI fixture (tests/data/, attributed) and
guarded by tests/offair.rs against dumphfdl's field values.

## LMS equalizer + decision-directed carrier loop (2026-06)

The per-T-segment phase re-estimation is replaced by the structure
dumphfdl uses: a symbol-spaced 7-tap LMS feed-forward equalizer
(identity-initialized, decision at the window center, trained on the 9
preamble T segments and retrained on every embedded T segment) plus a
2nd-order decision-directed carrier loop running on every symbol. The
carrier loop matters more than the equalizer taps: the A1→A2 carrier
refinement is ambiguous modulo 2*pi/127 per symbol, so a residual
rotation of up to ~0.025 rad/symbol can survive acquisition — per-T
re-estimation papered over it, the DD loop removes it. Off-air result
on the sigidwiki 21931 kHz capture: 31 events vs 28 before (dumphfdl:
37; the rest is weak-burst acquisition sensitivity).

## Full HFNPDU/LPDU record decode + AC cache + FEC count (2026-06)

Decode-completeness pass against dumphfdl 1.7.0 (GPL — facts only, wire
layouts read from src/hfnpdu.c, lpdu.c, ac_cache.c, util.c; all code
re-derived):

- **Performance data (0xD1)** split out of the shared 0xD5 handler and
  decoded in full (47-octet record): version, flight_leg, gs_id+name,
  freq_id, per-leg freq_search_cnt and hf_data_disabled_duration,
  per-bitrate MPDU rx/rx_err/tx/delivered counters, SPDU rx/missed, and
  freq_change_code with its cause table.
- **Frequency data (0xD5)** now emits the up-to-6 per-GS {gs_id,
  prop_freqs, tuned_freqs} arrays (20-bit packed) it previously dropped.
- **0xD2 system-table-request**, **0xDE delayed-echo**, **0x2F
  logon-denied** (with reason table) named; **0x3F logoff** gains its
  reason text; unknown HFNPDU/LPDU types carry the dumphfdl type-name.
- **Aircraft-ID → ICAO cache** (ac_cache.rs): records the ICAO from each
  logon-confirm under its assigned channel-local aircraft ID (per-channel
  keying, one PduParser per channel), back-fills it on later downlinks,
  TTL-expires (default 3600 s, dumphfdl AC_CACHE_TTL_DEFAULT) and evicts
  on logoff/logon-denied.
- **fec_corrected** populated from the Viterbi: decoded bits are
  re-encoded through the same convolutional code and the Hamming distance
  to the received hard decisions (= nearest-codeword distance = corrected
  symbols) is stamped on every demod-path event. Pure crate-local
  (Viterbi::encode), no xng-dsp change.

Tests pin the dumphfdl byte offsets/semantics (regressions surface as a
mismatch against the reference layout); fec_corrected is checked against
the FEC's own definition (clean burst → 0) and on the real off-air
capture, never via parser loopback.

## SPDU first-octet flags + per-slot assignment region (2026-06, HFDL-6)

Completeness pass on the squitter (SPDU) parse against dumphfdl 1.7.0
src/spdu.c `spdu_parse()` (GPL — facts only; all code re-derived):

- **First-octet flags** now surfaced field-for-field with the oracle:
  `rls_in_use` (`buf[0] & 2`, bit 1), `spdu_version` (`(buf[0] >> 2) & 3`,
  bits 2-3), `iso8208_supported` (`buf[0] & 0x20`, bit 5). `change_note`
  (bits 6-7) was already decoded; the other three were dropped before.
- **Per-slot assignment / TDMA reservation region** `buf[4..52)` (48
  octets, the single largest span dumphfdl leaves opaque — it reads the
  header up to `buf[3]` then jumps to `min_priority` at `buf[52]`) is now
  surfaced verbatim as `slot_assignment_hex`. No public spec (ARINC 635-3
  is paywalled; dumphfdl/SigID/PC-HFDL give no byte map) defines its
  per-slot subfield layout, so it is carried raw rather than fabricated.

Verification: a spec-derived unit test pins each bit position to the
oracle formula; the off-air test (`tests/offair.rs`) additionally asserts
the flags and the 48-octet region against the real 21931 kHz capture's
own decoded bytes (byte0 = 0x10 → rls/iso clear, version 0), i.e. against
dumphfdl 1.7.0's ground truth on the same recording — not a loopback.

## System-table (0xD0) station-name enrichment (2026-06, HFDL-6)

The reassembled `SystemTable` GS records already matched dumphfdl 1.7.0
src/systable.c `systable_decode_gs()` field-for-field (gs_id, utc_sync,
lat/lon, spdu_version, freq_cnt, per-freq BCD frequency + master frame
slot). The one in-memory field dumphfdl emits that we dropped is the
per-station **name**: each decoded `GroundStation` now carries `gs_name`
from the crate's built-in public ARINC HFDL GS list (`pdu::gs_name`),
mirroring dumphfdl's per-station `name` JSON field. This is decode-side
enrichment from the same station list already used (and asserted)
elsewhere in the crate — it needs no external config file (the
config-driven systable file / GS-name file is HFDL-2.x, deliberately out
of scope here). Unassigned IDs (the 12 holes in 1..=17, or any ID outside
it) leave `gs_name` unset rather than invent a name.

Verification: `parse_stations` is asserted to populate the name for known
IDs (1 → San Francisco, 13 → Santa Cruz) and to leave it `None` for an
unassigned ID (12); the reassembly test asserts GS 4 → "Riverhead, New
York" in the completed `systable-complete` event. The ID→name table is
the public ARINC list, not a loopback.

## Coherent A1 sync (2026-06, demod v2 step 2)

The quarter-sample coherent-correlation refinement after the
differential A1 hunt is replaced by the same coherent joint fit that
recovered the VDL2 XID bursts: over a fine timing grid, the per-symbol
phases of the 127 known BPSK chips (signs removed, residual rotation
pre-subtracted using the differential estimate) are unwrapped and fit
to residual ≈ a + b·k weighted by sample energy. The minimum-cost grid
point yields timing and per-symbol CFO jointly, with none of the
2π/127-per-symbol aliasing of the A1→A2 dphi refinement. Off-air
result: 33 events on the 21931 kHz capture (from 31; dumphfdl: 37).

## System-table persistence + full GS-name roster (2026-06, HFDL-2.1/2.2)

**HFDL-2.1 — system-table persistence API.** `SystemTable` gains serde
`Deserialize` (alongside the existing `Serialize`) and a load/save pair:
`SystemTable::save(path)` / `SystemTable::load(path)` plus the
free-function aliases `save_system_table(table, path)` /
`load_system_table(path)` named by the task (the `--system-table` CLI
flag is the documented follow-up, not wired here). Persisted as pretty
JSON through the crate's existing serde_json channel — the serde
equivalent of dumphfdl's libconfig `systable_save_config()` /
`systable_read_from_file()` (szpajder/dumphfdl src/systable.c, GPL —
read for the persisted-field set only: GS id 0..=127, optional name,
lat/lon, frequencies, table version). JSON rather than libconfig so it
needs no new dependency. Purpose: cold-start enrichment — a long-running
receiver saves the most recent reassembled 0xD0 table so a later run
starts with known GS positions/frequencies instead of waiting for the
next over-the-air set.

Round-trip verification (`systable_persistence_round_trip`): a table
built by running real GS records through `parse_stations` (so name
enrichment for id 1 and the `None` hole for id 12 are both exercised) is
saved and reloaded; every integer/string/bool field is asserted equal,
coordinates to <1e-6° (text f64 round-trip is exact in practice but not
guaranteed by the serde contract, so coordinates are compared
approximately), and a save→load cycle is shown to be a bit-exact fixed
point. `systable_load_missing_file_errors` covers the I/O-error path.
This is a persistence round-trip, not a decode oracle — the decode
itself is still grounded against dumphfdl elsewhere.

**HFDL-2.2 — built-in GS-name roster.** The `pdu::gs_name` table is the
published HFDL/ARINC ground-station roster, verified id-for-id against
dumphfdl's distributed `etc/systable.conf` (szpajder/dumphfdl, GPL —
facts only). That roster assigns **exactly ids 1..=11 and 13..=17**; id
12 is the only hole inside that span and **ids 18..=127 are unassigned**
in the public roster. The crate table already matched this list, so
there were no real names to add: the task's "12 holes to fill up to 127"
does not correspond to any published assignments, and per the
verification mandate those ids are left `None` rather than filled with
fabricated names. Coverage is now explicit over the full 7-bit GS-id
space (0..=127) and pinned by `gs_name_roster_matches_published_list`,
which asserts the mapping id-for-id against the systable.conf roster
(every assigned id → its name; every other id in 0..=127 → `None`). The
only wording difference from systable.conf is id 1 "San Francisco, USA"
vs the conf's "San Francisco, California" — both name KSFO; the crate's
string is the one already asserted across the crate's other tests and is
left unchanged to avoid a cosmetic, test-breaking churn.
