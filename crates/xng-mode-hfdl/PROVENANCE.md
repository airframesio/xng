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
