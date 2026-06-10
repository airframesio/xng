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
Area address fields are carried raw pending IMO SafetyNET manual
decoding.

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
