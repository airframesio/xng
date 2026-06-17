# Provenance — xng-mode-stdc

Implemented from protocol facts collected in `docs/notes/STDC.md`,
cross-verified across inmarsatc (GPL-3), SatDump (GPL-3), and Scytale-C
(GPL-3) — **facts only; all code here is re-derived** (the sourcing
policy in docs/REFERENCES.md). Key constants were numerically re-verified
during research: unique word `07 EA CD DA 4E 2F 28 C2`, descrambler LFSR
G = 1 + x^3 + x^4 + x^5 + x^7 with init 0x80 (circulating docs that say
0x40 are wrong), row permutation i·23 mod 64, 64×162 interleaver,
K=7 r=1/2 code 171/133 (shared xng-dsp Viterbi).

Bit-order convention: the 5120 decoded bits pack into bytes LSB-first
(equivalent to the KA9Q chainback + per-byte bit reversal described by
the reference implementations); flagged for confirmation against the
public sigidwiki capture (`Inmarsat-C_TDM_EGC_IQ.zip`).

EGC service-code address lengths and the packet checksum (Fletcher /
ISO 8473 style) follow the cross-verified tables in docs/notes/STDC.md.

## Field-decode tables and oracles (2026-06)

- **frame_number → UTC-of-day** and **channel-frequency formula**:
  oracle is docs/notes/STDC.md (frame = 8.64 s exactly, 10000 frames/day;
  uplink/downlink MHz formulas), cross-checked against inmarsatc
  `decode_7D` timestamp and `uplinkChannelMhz`/`downlinkChannelMhz`.
  Both are deterministic mappings with deterministic tests; the off-air
  capture validates them on real bytes (frame 5987 → 14:22:07; the real
  0x6C uplink word 0x2748 → 1636.64 MHz, inside the L-band uplink band).
- **EGC service long names** and **LES/NCS operator-name + ocean-region
  long-name tables**: verbatim from inmarsatc `getServiceCodeAndAddress
  Name` / `getLesName` / `getSatName` (facts only; re-typed, not ported).
  The LES table keys on the full region×100+id code because inmarsatc
  maps the same id to different operators by ocean region.
- **ITA2 / Baudot (presentation 6)**: oracle is the ITU-T ITA2 standard
  alphabet (universal, deterministic); one 5-bit code per on-air byte
  with LTRS/FIGS shift. No open decoder (inmarsatc/SatDump) does this.
- **Geographic area-address (STDC-1)**: C2 → shape + documented C3 field
  layout decoded per the IMO International SafetyNET Manual (2019),
  Annex 4 part A §5.2–5.3, cross-checked against inmarsat-sniffer's C2
  service-name table. Only the manual-verifiable classification + typed
  raw payload bytes are surfaced. The on-air *binary packing* of the C3
  coordinate digits is undocumented in every accessible primary source
  and decoded by no open decoder (inmarsatc, SatDump, sdrangel and
  inmarsat-sniffer all carry the EGC address as raw bytes only), so
  lat/lon/radius extraction is deliberately deferred rather than guessed.

Demodulator: textbook coherent BPSK — square-law FFT coarse frequency
estimation, decision-directed Costas loop, Gardner timing — written
independently of the GPL references.

Known demod limitation (documented during loopback bring-up): timing
acquisition from a cold start on an unfiltered direct-injection signal is
weak — the Gardner loop needs the receive-path (DDC) filtering and a few
seconds of the continuous carrier to converge, which deployment always
provides. Definitive demod validation target: the public sigidwiki
capture (Inmarsat-C_TDM_EGC_IQ.zip), stage-by-stage against SatDump's
.frm output.

## Off-air validation (2026-06)

Validated against the sigidwiki Inmarsat-C TDM/EGC IQ recording
(CC BY-SA, 49 s, AOR-E, TDM carrier at +216 Hz in the capture). The
demod chain (coarse AFC, Costas, Gardner, UW frame sync) worked on the
real signal as-is — the UW scored 128/128 on the first frame. The one
convention fix: **coded pair order is 133-output first** (the same
finding as Aero and HFDL); with 171-first the deinterleaved frame
decodes to pseudorandom bytes and no packet checksum passes, with
133-first every packet in the frame validates.

Result: 51 packets from the capture — bulletin boards with consecutive
TDM frame numbers (5987, 5988, ...), logical-channel announcements with
MES IDs and LES routing (AOR-E), confirmations, and signalling-channel
descriptors. A 14 s slice (one full frame) is vendored as a CI fixture
(tests/data/, attributed) guarded by tests/offair.rs.
