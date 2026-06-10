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
