# Iridium — implementation notes

Recorded 2026-06 for xng-mode-iridium. Sources: iridium-toolkit
(muccc, BSD-2 — code portable with attribution; bitsparser.py/bch.py
are the layer-2 reference), gr-iridium and iridium-sniffer
(alphafox02; GPL-3 — **facts only**; iridium-sniffer's ARCHITECTURE.md
documents the whole pipeline with parameters and is the best single
PHY reference).

## PHY

- L-band 1616–1626.5 MHz. Duplex channels below ~1625.979 MHz
  (toolkit `f_duplex` incl. doppler guard), simplex above ~1626.104
  (`f_simplex`). Ring-alert channel 1626.270833 MHz; quaternary
  messaging channels nearby.
- DQPSK, **25 000 symbols/s**, bursts. Channel width 40 kHz
  (gr-iridium burst detector width); RRC matched filter.
- Burst anatomy: preamble tone (16 symbols normal, 64 long/simplex) →
  12-symbol UW → payload. Max burst 90 ms; normal frames 131–191
  payload symbols, simplex 80–444.
- UW absolute QPSK symbols: DL `[0,2,2,2,2,0,0,0,2,0,0,2]`,
  UL `[2,2,0,0,0,2,0,0,2,0,2,2]` (gr-iridium); the toolkit's 24-bit
  "access codes" are the *differential decode* of these:
  DL `001100000011000011110011`, UL `110011000011110011111100`.
- Demod (gr-iridium/sniffer): decimate to 1 sps → 1st-order PLL
  (α=0.2) → hard QPSK → UW verify (DL+UL, Hamming ≤ 2) → differential
  decode `map[(s−prev) mod 4]` with `dqpsk_map = [0,2,3,1]` → 2 bits
  per mapped symbol MSB-first (0→00, 1→01, 2→10, 3→11; verified: UW
  symbols reproduce the access-code bits exactly).
- gr-iridium front end (for the wave-2 wideband mode): 8192-pt
  sliding FFT at 10 MHz, adaptive noise floor over 512 frames, 16 dB
  threshold, per-burst downmix to 250 kHz / 10 sps.

## Layer 2 (iridium-toolkit bitsparser.py, BSD — ported)

Bit stream starts with the 24-bit access code; `data` = rest.

Classification (downlink, in order): IMS if `data[0..32]` ==
`00110011111100110011001111110011`; ITL if 96-bit header `11` + 94
zeros; IBC if BCH(7,3) poly 29 over `data[0..6]` == 0 and the 2-way
deinterleave of the next 64 bits passes ringalert BCH; LCW (duplex)
via the 46-bit LCW permutation + BCH 29/465/41; **IRA** if the 3-way
deinterleave of `data[0..96]` yields 3×32-bit blocks each passing
ringalert BCH.

- BCH polys (as integers, toolkit convention): ringalert/IBC **1207**,
  messaging **1897**, IBC header **29**, LCW parts 29/465/41. Blocks
  are 32 bits = 31 BCH bits (21 data + 10 check) + 1 even-parity bit.
  Repair: try 1–2 bit flips until the syndrome clears; whole-block
  even parity must hold.
- Deinterleave operates on QPSK symbol pairs **with the two bits of
  each pair swapped**, reading symbols from the end backwards:
  2-way (64 bits → 2×32) alternating odd/even, 3-way (96 → 3×32).
  After the leading IRA triple, the remainder deinterleaves 2-way per
  64-bit chunk.
- FILL pattern (64 bits, removed before ECC, ≤2 bitdiff per half):
  `10100010011100111011111101101101 01010100010001011100001011100110`.

### IRA (ring alert) payload — concatenated 21-bit BCH data blocks

| Bits | Field |
|---|---|
| 0–6 | satellite id |
| 7–12 | beam id |
| 13–24 | pos_x (sign bit + 11) |
| 25–36 | pos_y |
| 37–48 | pos_z |
| 49–55 | RA interval (90 ms units) |
| 56 | broadcast timeslot |
| 57 | EPI? |
| 58–62 | BCH downlink sub-band |
| 63… | pages, 42 bits each: tmsi(32) zero(2) msc_id(5) zero(3); all-1s page = END |

lat = atan2(z, √(x²+y²)) (geocentric), lon = atan2(y, x),
alt = 4·√(x²+y²+z²) km (radius; subtract ~6378−23 for height).

## v1 scope in xng

Single-channel decoder (DDC to 250 kHz / 10 sps) aimed at the fixed
ring-alert channel: burst detect → tone CFO → RRC → coherent UW fit
(timing/phase/CFO jointly, as in the VDL2/HFDL demods) → 1-sps
decisions → DQPSK → bits → access check → IRA/IBC parse. Wideband
burst hunting across the full band (gr-iridium style channelizer) is
the wave-2 follow-up, as is IDA/SBD→ACARS (iridium-sniffer documents
the chain: LCW FT==2/6/7, descramble 124-bit blocks, BCH(31,20),
CRC-CCITT, multi-burst reassembly, SBD→ACARS via ARINC 622).

Validation: iridium-toolkit's parser run offline as an oracle on
generated frames (bit-identical field decode), vectors vendored into
unit tests; live RA channel needs only an L-band antenna (bursts every
few seconds worldwide).

## Wideband (wave 2) — decoding real off-air bursts

The full-band hunter (`wideband.rs`) detects bursts by FFT, downmixes
each to baseband, and feeds the single-channel demod. Getting this to
decode a real 6 MS/s off-air capture (SAWbird+IR / Maxtena PN100) took
three fixes, each isolated and tested:

1. **Channelize with the DDC, not a boxcar.** A boxcar-of-decim averager
   is a poor anti-alias filter — its sinc sidelobes fold ~8 dB of
   wideband noise into the 250 kHz channel (measured peak/noise 8.5 dB
   vs 16.6 dB through a real FIR on the same burst). Each burst now goes
   through `xng_dsp::Ddc` (the same two-stage windowed-sinc the
   single-channel path uses). (Initial bring-up used a 50 kHz one-sided
   passband; later retuned to **28 kHz** once multi-frame decode and the
   post-roll change shifted the optimum — see the sensitivity campaign
   below.)
2. **Seed the demod noise floor (the decisive fix).** The demod's
   asymmetric noise EMA starts at 1.0 and needs ~1400 quiet samples to
   converge. A continuous stream gives it that; an isolated
   wideband-extracted burst has only ~1000 channel samples of pre-roll,
   so the floor froze ~18 dB high when the burst arrived and the
   acquisition gate (`noise·8`) sat *above* the signal — zero energetic
   windows, no UW fit attempted. The front end now estimates the
   channel's noise (20th-percentile power) and `seed_noise()`s the demod
   so the gate is correct from the first sample. This alone took the real
   capture from 0 → decoding (clean UW costs 0.003–0.04).
3. **Don't over-reject in the BCH/classify gates.** `ecc_blocks` trusts a
   weight-1 BCH correction even when the separate even-parity bit is
   flipped (an unambiguous correction on this d=5 code; the parity flip
   is just a second, harmless error), and `classify` accepts
   BCH-*correctable* RA headers, not only zero-syndrome ones.

Frame typing: **ITL ("TL", Time-Location)** must be classified *before*
IRA. Its 96-bit header is `11` + 94 zeros, which is a valid (degenerate)
all-zero BCH codeword — so without an explicit ITL check it falls through
to the IRA classifier and mis-decodes as a ring alert with an all-zero
satellite/position. The real captures are dominated by ITL bursts; they
are now reported as `kind:"itl"` (full satellite/plane PRS decode via the
toolkit's `itl.py` tables is still TODO).

## Sensitivity campaign: matching and beating gr-iridium

The wideband path above got real captures *decoding*; a later campaign took
the IDA yield from ~2 % of gr-iridium to **ahead of it**. On a shared 300 s
off-air capture (Airspy R2, 1622 MHz, 10 MS/s, KSMF) the scoreboard is now:

| | xng | gr-iridium |
|---|---:|---:|
| CRC-OK IDA frames | **758** | 573 |
| total IDA frames | 1577 | 1214 |
| distinct-content CRC-OK | **587** | (573 raw) |

gr-iridium = `iridium-extractor -o -c 1622000000 -r 10000000 -f ci16_le FILE`
→ `iridium-parser.py -o line` on the same file; CRC-OK = lines with `CRC:OK`.
xng = `xng decode FILE -f cs16 -r 10000000 -c 1622.000M --channels 1622.000M
--mode iridium`; CRC-OK = IDA frames with `body.details.crc_ok`. Both
pass-rates match (~48 %), so the entire gap was IDA-frame **production**
(weak bursts reaching a valid frame at all), not decode quality.

The acquisition chain is a faithful port of gr-iridium's, plus a few
beyond-gr refinements (all env-overridable; defaults given):

- **Detector** — gr `fft_burst_tagger`: 512-frame rolling-*mean* baseline,
  threshold `10^(dB/10)/ENBW` (ENBW 1.72, 16 dB), integer **peak-bin**
  centering, ±burst_width/2 mask, and the `(fs/burst_width)·0.8` max-bursts
  squelch. Per-bin freeze keeps a sustained burst from lifting its own floor.
- **Channelization** — per-burst `Ddc` to the 250 kHz channel rate, **28 kHz**
  one-sided passband (re-swept after the post-roll/multi-frame changes moved
  the optimum; gr's gentle `input_fir` passes energy out to ~28 kHz too).
- **Fine CFO** — gr's squared-FFT estimate: square the preamble+UW (removes
  the BPSK → tone at 2·CFO), Blackman window, 16× zero-padded FFT, quadratic
  interpolation, halve. Re-estimated **per frame** in the multi-frame loop;
  a ±640 Hz residual-CFO grid (`CFO_REFINE=2`) is searched jointly with
  timing in the sync correlation.
- **Sync** — full **28-symbol** (16 preamble + 12 UW) coherent correlation at
  full sample resolution, DL and UL, free initial phase; no magnitude gate
  (the access code + CRC decide).
- **Multi-frame** — decode **every** TDMA time-slot frame in one detector
  window (`handle_multiple_frames_per_burst`), advancing by each frame's
  symbol count. 24 ms post-roll keeps detection alive across the short
  inter-slot gaps so adjacent frames land in one window.
- **Demod** — 1st-order decision-directed PLL (α=0.2) on the payload,
  carrier seeded from the UW correlation; differential decode in
  iridium-toolkit pair order.
- **End-of-frame (the decisive win)** — trim only after **3 consecutive**
  symbols below **peak/8** (−18 dB from the burst's own max), reading a full
  ≤191-symbol frame. xng previously broke on the *first* payload symbol below
  `noise×4` (absolute); a weak burst sits right at that floor, so one faded
  symbol truncated the frame below the ~190-symbol BCH/CRC length and the
  whole frame was lost. This rule alone lifted CRC-OK IDA **516 → 758**.
- **Validation** — differential access-code gate at the random-match boundary
  (≤12 of 24 bits; the 24-bit CRC, false-pass ≈ 6e-8, is the real arbiter)
  → BCH (matches iridium-toolkit exactly) → CRC.
- **Two-filter union** — every burst is demodulated both unfiltered and
  through the RRC matched filter; the two recover largely *disjoint*
  populations (strong vs weak: 182 vs 758 CRC-OK alone), so both are emitted
  and deduped by decoded content.

Campaign deltas (300 s CRC-OK IDA): squared-FFT CFO 337→419, multi-frame
419→454, 28 kHz channel 454→463, 24 ms post-roll 463→489, residual-CFO
refine 489→506, access-gate 506→516, **end-of-frame rule 516→758**.

**Tested and rejected** (negative results worth keeping): lowering the
detector threshold floods with noise detections that never decode; a
preamble decision-directed PLL *runway* hurts (xng's 28-symbol batch
correlation already extracts the optimal phase, and the onset preamble
symbols are RRC-edge-unreliable); the absolute UW `check_sync_word` and a
whole-burst PLL are both neutral; multi-hypothesis FFT-peak CFO gains nothing
(the squared-FFT always lands on the right lobe within ±1.6 kHz).
`MIN_BURST_SPAN=0` (extracting single-frame bursts, as gr does) adds +9
frames offline but is **offline-only** — on the live station it floods the
per-channel decode queues and grows `chan_dropped`, losing more than it
gains; the default (2) keeps the soak drop-free at 10 MS/s.

Not count-gated in CI: the capture is 11 GB, too large to vendor. The demod
core is fenced by the bit-exact (`demodulates_gr_iridium_test_burst`) and
field-exact (`offair_ida_sbd_decodes_with_crc`) oracle tests instead.
