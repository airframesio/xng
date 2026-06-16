# Inmarsat STD-C / EGC

Facts cross-verified across inmarsatc (GPL, facts only), SatDump (GPL,
facts only), and Scytale-C documentation; scrambler table and UW
numerically verified. All code re-derived (not ported from these GPL
sources).

## PHY

- NCS carriers: AOR-W 1537.70 MHz, IOR 1537.10, AOR-E 1541.45,
  POR 1541.45. Continuous.
- BPSK 1200 sym/s, coherent (NOT differential despite some wiki labels):
  Costas loop + Gardner timing; RRC α=0.6 (SatDump: 31 taps, pll_bw 0.03).
  180° ambiguity resolved at the UW (correlate normal + inverted; if
  inverted wins, complement the frame). Handle mid-frame polarity flips.
- Frame = 10368 symbols = 8.64 s exactly; frame number 0..9999 resets at
  UTC midnight (seconds_of_day = frame_number × 8.64).

## Frame structure

- 64 rows × 162 columns, transmitted row by row; each row = 2 UW symbols
  + 160 data symbols.
- **UW (64 bits, each bit sent twice at row start):**
  `07 EA CD DA 4E 2F 28 C2` (MSB-first). Accept ≥121/128 matches
  (SatDump) over a sliding window.
- **Row permutation**: transmitted row j carries original row
  i = (j×39) mod 64; receiver: j = (i×23) mod 64.
  `out[i*162..] = in[((i*23)%64)*162..]`.
- **Deinterleave**: strip 2 UW columns, read 64×160 column-wise:
  `out[col*64+row] = in[row*162+col+2]` → 10240 soft symbols.
- **Convolutional**: K=7 r=1/2, textbook 171/133 octal (our Viterbi::k7).
  10240 → 5120 bits = 640 bytes; transmitter appends a flush byte
  (639 info + 1 flush, trellis ends in state 0).
- **Bit-reverse every byte** between Viterbi output and descrambler
  (chainback order → wire order). Mandatory.
- **Scrambler (after Viterbi)**: 640 bytes = 160 groups of 4 bytes; a
  7-bit LFSR G = 1+x³+x⁴+x⁵+x⁷, **init 0x80** (docs saying 0x40 are
  wrong), one output bit per group; bit=1 → XOR the 4 bytes with 0xFF.
  Step: `out = reg&1; new = out ^ (reg>>2&1) ^ (reg>>3&1) ^ (reg>>4&1);
  reg = (reg>>1) | (new<<7)`.
  First table entries: 0,0,0,0,0,0,0,1,0,0,0,1,1,1,0,0,0,1,0,0,1,0,1,1,
  1,0,0,0,0,0,0,1,1,0,0,1,0,0,1,0,0,1,1,0,1,1,1,0,0,1,0,0,0,0,...

## Packet layer (within the 640-byte frame)

- Descriptor: `0xxxxxxx` short — type=(b>>4)&7, len=(b&0xF)+1;
  `10xxxxxx` medium — type=b&0x3F, len=byte[1]+2;
  `11xxxxxx` long — len=(b1<<8|b2)+3.
  Descriptor 0x00 = padding, stop.
- **Checksum** (last 2 bytes of every packet; Fletcher/ISO-8473 style):
  C0+=B; C1+=C0 over the packet with checksum bytes as 0;
  CB1=u8(C0−C1), CB2=u8(C1−2·C0). Accept transmitted 0x0000 inside
  re-encapsulated multiframe content.
- Key types: 0x7D Bulletin Board (frame number at [2-3]);
  0xAA Message Data (LCN at [3], packet seq at [4]); 0x81 Announcement /
  0x83 LC Assignment (open logical channel); 0x27 LC Clear;
  0xB0 EGC single header / 0xB1+0xB2 EGC double header parts;
  0xBD multiframe start / 0xBE continue (reassemble, parse recursively).
- Channel freq: uplink MHz = ((b0<<8|b1)−6000)·0.0025+1626.5;
  downlink MHz = ((b0<<8|b1)−8000)·0.0025+1530.5.
- Sat/LES byte: bits 7-6 ocean region (0 AOR-W, 1 AOR-E, 2 POR, 3 IOR),
  bits 5-0 LES id; display LES = sat×100+id. MES id = 24-bit.

## EGC header (0xB0/B1/B2, same layout)

[2] service code; [3] bit7 continuation, bits6-5 priority (Routine/
Safety/Urgency/Distress), bits4-0 repetition; [4-5] message sequence
(BE); [6] packet sequence (1-based); [7] presentation (0 IA5 byte-per-
char & 0x7F, 6 ITA2 Baudot, 7 binary); [8..8+A-1] address; payload;
2-byte checksum.

Address length A by service code: 0x00→3, 0x02→5, 0x04→7, 0x11→4,
0x13→6, 0x14→7, 0x23→6, 0x24→7, 0x31→4, 0x33→6, 0x34→7, 0x44→7,
0x72→5, 0x73→6, default 3. (Area decoding per IMO SafetyNET manual —
carry raw hex initially.)

Assembly: key = message sequence number; order by (pkt_no×2 + is_part2);
complete when a 0xB2 (or single-header 0xB0) arrives with
continuation=0; ~30 s timeout fallback.

## Test material

xng's STD-C is oracle-validated field-exact (no count-style benchmark).

- sigidwiki "Inmarsat-C TDM" page hosts `Inmarsat-C_TDM_EGC_IQ.zip`, the
  public IQ test vector; validated field-exact against SatDump-derived
  goldens.
- SatDump writes `.frm` (640-byte descrambled frames) + JSON: run it on
  the sigidwiki capture for stage-by-stage goldens.
- Full TX chain is specified above, so synthetic roundtrip vectors are
  straightforward (scramble 639+1 bytes → conv encode → 64×160
  column-write/row-read → inverse row permutation → doubled UW per row).

## Gotchas

1. Byte bit-reversal between Viterbi and descrambler.
2. Scrambler init 0x80.
3. Accept checksum 0x0000 in multiframe content.
4. Mid-frame polarity reversal handling.
5. Packet length fields exclude descriptor byte(s) — add 1/2/3.
