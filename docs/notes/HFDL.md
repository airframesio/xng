# HFDL (ICAO Annex 10 Vol III Ch. 11 / ARINC 635) — implementation notes

Facts from ICAO Annex 10 Vol III Part I Ch. 11 (free PDF, ffac.ch) plus
dumphfdl source read for facts only (GPL, so all code is re-derived).

On the sigidwiki 21931 kHz capture xng decodes 36 events vs dumphfdl's 37
(97%); CI bench floor is 31. The residual gap is the weakest bursts (4-5
dB SNR at 300 bps), a sensitivity tail, not a convention bug.

## PHY

- USB channel; audio subcarrier = SSB carrier + **1440 Hz** (ITU
  "assigned frequency" = carrier + 1400 Hz). Band 2.8–22 MHz, 1 kHz
  tuning. Emission 2K80J2DEN.
- M-PSK at **1800 symbols/s ±10 ppm**: M=2 → 300/600 bps (by code rate),
  M=4 → 1200, M=8 → 1800. Pulse: SRRC α=0.31 (single-cos spectrum in
  the spec). Receiver must handle subcarrier offset ±70 Hz, 5 ms
  multipath, 2 Hz Doppler.
- Gray ring mapping (phase position n carries Gray label n⊕(n>>1)):
  0°: 0/00/000, 45° 001, 90° 01/011, 135° 010, 180° 1/11/110, 225° 111,
  270° 10/101, 315° 100. Phase ref from preamble; residual π ambiguity
  resolved by A-correlation sign (global bitmask flips everything).
- 300 bps: rate-1/4 = each rate-1/2 chip transmitted twice (copies
  separated by the interleaver; consecutive after deinterleave; average
  the soft pair).

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

**A sequence (127 bits, 0 = +1/0°):**
0101101110111100011101000101011100000011110110011000100100111001
111100100000100011010101001101101001010000101100001100101111111

**M base sequence (127 bits):**
0111011011110100010110010111110001000000110011011000111001110101
110000100110000010101011010010010100111100100011010100001111111

(NOTE: transcribe carefully — 127 bits each.)

**M shifts → settings** (index order): {72:300/S, 82:600/S, 113:1200/S,
123:1800/S, 61:300/D, 103:600/D, 93:1200/D, 9:1800/D}.
Coded chips per burst: 2160/2160/4320/6480 (S), 5040/5040/10080/15120
(D). Decoded payload bits: 540/1080/2160/3240 (S), 1260/2520/5040/7560
(D).

**T training (15 BPSK symbols):** 0x9AF = 000100110101111 MSB-first
(phases + + + − + + − − + − + − − − −). LMS equalizer (dumphfdl: 15
taps) retrains on every T segment.

Correlation thresholds (dumphfdl): A1 |ρ|>0.36, A2 >0.30, M1 >0.30.

## Scrambler

x^15+x+1, init 0x6959 (= the same 110100101011001 state as VDL2/Aero),
**truncated to 120 bits then reset**. Applied per DATA SYMBOL at the
modulation layer: LFSR bit 1 → rotate symbol by π. 120 tiles exactly
into 2160 and 5040 data symbols, so state resets align with burst ends.
(Output-bit convention flagged: verify against a real capture; our
Lfsr15 matches the VDL2 derivation.)

## FEC + interleaver

- Convolutional K=7 rate 1/2, classic 171/133 octal (Karn's 0x6d/0x4f
  are bit-reversed forms). Encoder zero-start, zero-flush (tail inside
  the fixed payload size); decoder traceback to state 0.
  **Pair order confirmed off-air: 133-output first** in each
  coded pair (libcorrect convention), same as Aero. Verified against the
  sigidwiki 21931 kHz capture — with 171-first no FCS validates; with
  133-first the SPDU matches dumphfdl field-for-field.
- Deinterleaver: 40 rows × C cols, C = chips/40 ∈ {54,108,162} single,
  {126,252,378} double. Push (per received chip, soft, MSB-first within
  symbol): write (row,col); row++; on row wrap col++; then every push
  col = (col − S) mod C with **S = 17 single / 23 double**. Pop: read
  (row,col); row = (row+9) mod 40; on wrap col++. TX = inverse
  permutation (generate both index sequences and invert).
- After Viterbi: bits packed MSB-first then **every byte bit-reversed**
  (net: air order is LSB-first per byte for all PDU layouts).

## Link layer (all FCS = CRC-16/X-25, LE trailer)

PDU type: first octet bit0: 1 = MPDU, 0 = SPDU.

**SPDU** (66 octets, FCS over first 64): [0] b1 RLS, b2-3 version,
b5 ISO8208, b6-7 change note (0 none/1 channel down/2 freq change/3 GS
down); [1] b0-6 src GS id, b7 UTC sync; [2]+[3]b0-3 frame index (12b);
[3]b4-7 frame offset; [4..51] per-slot assignment codes (not parsed
v1); [52]b0-3 min priority; [53]+[54]b0-3 systable version (12b);
[54]b4-7+[55]+[56] self freqs-in-use (20-bit bitmap, bit i = freq index
i in systable); [57..60] neighbor GS2 id+freqs; [60..63] GS3 id+freqs.

**MPDU downlink** ([0]&2 set): [0] b2-5 LPDU count; [1] b0-6 dst GS id;
[2] aircraft id (8-bit alias); [3..5] reserved; [6..6+n) LPDU size
octets (len = value+1); header FCS (2); LPDUs follow, each with its own
trailing FCS.
**MPDU uplink**: [0] b4-6 = n_aircraft−1; [1] b0-6 src GS id; per
aircraft: id octet, count octet (high nibble = LPDU count), then size
octets; header FCS; LPDUs grouped per aircraft.

**LPDU types**: 0x0D unnumbered data, 0x1D unnumbered ack'd data (both:
HFNPDU follows), 0x8F/0xBF/0x4F logon req (ICAO 24-bit at [1..3] —
bytes MSB-first in raw air order, i.e. re-reverse those 3 bytes),
0x9F/0x5F logon confirm ([4] assigned AC id), 0x2F logon denied,
0x3F logoff request.

**HFNPDU** ([0]=0xFF, [1]=type): 0xD0 system table partial ([2] hi
nibble+1 = total, lo = seq; version 12b at [3]>>4|[4]<<4); 0xD1
performance data (flight id [2..7], 20-bit lat/lon ×180/2^19 at
[8..12], UTC/2 u16 LE [13..14]); 0xD2 systable request; 0xD5 frequency
data (flight id, lat/lon, then ≤6 × {GS id, 20-bit heard bitmap,
20-bit listening bitmap}); 0xDE delayed echo; **0xFF enveloped ACARS**:
[2] = SOH then standard parity-bearing ACARS block (BCS CRC-16 KERMIT
init 0, DEL) — xng-acars::block::parse handles it.

**System table** (reassembled 0xD0): per GS: id+UTC b7; lat/lon 20-bit;
[6]b0-2 SPDU version, b3-7 freq count (≤20); per freq 3 octets BCD
(100 Hz units, nibbles low→high) + 1 octet (lo nibble = master frame
slot). Version 12-bit wrapping (newer if (new−old) mod 4096 < 2048).

## Receive pipeline order

DDC (channel+1440 Hz) → ~2.8 kHz filter → matched filter → symbol
timing → carrier (Costas; A1/A2 correlation phase for acquisition) →
LMS equalizer (T/2-spaced, 15 taps, trained on the 9 preamble T segments
and retrained on every embedded T segment) + decision-directed 2nd-order
carrier loop → A1/A2 hunt (π-sign bitmask) → M1 shift → 9 T → per
data segment: 30 data symbols (descramble π flips, Gray soft demod
MSB-first) + T retrain → deinterleave → (rate-1/4: average pairs) →
Viterbi → bit-reverse bytes → SPDU/MPDU → LPDU FCS → HFNPDU → ACARS.

## Validation

- No public unit vectors; sigidwiki hosts off-air IQ (e.g. 21931 kHz
  Riverhead) decodable by dumphfdl for ground truth; dumphfdl
  --iq-file + DEBUG stage dumps give stage-by-stage goldens.
- Synthetic TX→RX loopback is fully determined by the above.
- Live: any HF antenna; 16 ground stations worldwide (systable.conf).

## Channel rate

CHANNEL_RATE is 12 kS/s (6.67 samples/symbol). Raising it to 24 kS/s
tested worse (decodes dropped), because HFDL's marginal frames are
fading/SNR-bound, not timing-resolution-bound; the LMS equalizer + DD
carrier loop already own that domain.
