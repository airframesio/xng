# Iridium — implementation notes

xng-mode-iridium decodes Iridium L-band bursts: PHY demod, the layer-2
bitsparser port, IRA/IBC/ITL/LCW frame typing, a wideband full-band
pipeline, IDA/SBD reassembly, and beam-pattern reconstruction. Sources:
iridium-toolkit (muccc, BSD-2 — code portable with attribution;
bitsparser.py/bch.py are the layer-2 reference), gr-iridium and
iridium-sniffer (alphafox02; GPL-3 — **facts only**; iridium-sniffer's
ARCHITECTURE.md documents the whole pipeline with parameters and is the
best single PHY reference).

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

## Layer 2 (iridium-toolkit bitsparser.py, BSD — ported)

Bit stream starts with the 24-bit access code; `data` = rest.

Classification (downlink, in order): IMS if `data[0..32]` ==
`00110011111100110011001111110011`; ITL if 96-bit header `11` + 94
zeros; IBC if BCH(7,3) poly 29 over `data[0..6]` == 0 and the 2-way
deinterleave of the next 64 bits passes ringalert BCH; LCW (duplex)
via the 46-bit LCW permutation + BCH 29/465/41; **IRA** if the 3-way
deinterleave of `data[0..96]` yields 3×32-bit blocks each passing
ringalert BCH. ITL is checked **before** IRA: its all-zero header is a
valid (degenerate) BCH codeword that would otherwise fall through to
the IRA classifier and mis-decode as a ring alert at sat 0 / position
(0,0,0).

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
alt = 4·√(x²+y²+z²) km (radius; subtract ~6378−23 for height). The
degenerate all-zero header (sat 0, x=y=z=0) is rejected — no real
broadcasting satellite sits at Earth's center.

## Wideband pipeline

The full-band hunter (`wideband.rs`) detects bursts by FFT across the
whole capture, downmixes each to baseband, and feeds the per-burst
demod. It runs multi-threaded over a real off-air capture (Airspy R2 /
SAWbird+IR / Maxtena PN100) at up to 10 MS/s.

Design points that the live decode depends on:

- **Channelize with the DDC, not a boxcar.** Each burst goes through
  `xng_dsp::Ddc` (the same two-stage windowed-sinc the single-channel
  path uses), one-sided passband **28 kHz**. A boxcar-of-decim averager
  is a poor anti-alias filter — its sinc sidelobes fold ~8 dB of
  wideband noise into the 250 kHz channel (measured peak/noise 8.5 dB
  vs 16.6 dB through a real FIR on the same burst).
- **Seed the demod noise floor.** The demod's asymmetric noise EMA
  starts at 1.0 and needs ~1400 quiet samples to converge. A
  wideband-extracted burst has only ~1000 channel samples of pre-roll,
  so an unseeded floor freezes ~18 dB high and the acquisition gate
  (`noise·8`) sits *above* the signal — zero energetic windows, no UW
  fit. The front end estimates the channel noise (20th-percentile
  power) and `seed_noise()`s the demod so the gate is correct from the
  first sample (clean UW costs 0.003–0.04).
- **Don't over-reject in the BCH/classify gates.** `ecc_blocks` trusts
  a weight-1 BCH correction even when the separate even-parity bit is
  flipped (an unambiguous correction on this d=5 code; the parity flip
  is a second, harmless error), and `classify` accepts
  BCH-*correctable* RA headers, not only zero-syndrome ones.

Frame typing: real captures are dominated by **ITL ("TL",
Time-Location)** bursts, reported as `kind:"itl"`.

## Acquisition and sensitivity

The acquisition chain is a faithful port of gr-iridium's, plus a few
beyond-gr refinements (all env-overridable; defaults given). On a
shared 300 s off-air capture (Airspy R2, 1622 MHz, 10 MS/s, KSMF) it
runs ahead of gr-iridium:

| | xng | gr-iridium |
|---|---:|---:|
| CRC-OK IDA frames | **758** | 573 |
| total IDA frames | 1577 | 1214 |
| distinct-content CRC-OK | **587** | (573 raw) |

gr-iridium = `iridium-extractor -o -c 1622000000 -r 10000000 -f ci16_le FILE`
→ `iridium-parser.py -o line` on the same file; CRC-OK = lines with `CRC:OK`.
xng = `xng decode FILE -f cs16 -r 10000000 -c 1622.000M --channels 1622.000M
--mode iridium`; CRC-OK = IDA frames with `body.details.crc_ok`. Both
pass-rates match (~48 %), so the whole gap is IDA-frame **production**
(weak bursts reaching a valid frame at all), not decode quality.

Chain:

- **Detector** — gr `fft_burst_tagger`: 512-frame rolling-*mean*
  baseline, threshold `10^(dB/10)/ENBW` (ENBW 1.72, default **16 dB**),
  integer **peak-bin** centering, ±burst_width/2 mask, and the
  `(fs/burst_width)·0.8` max-bursts squelch. Per-bin freeze keeps a
  sustained burst from lifting its own floor. gr defaults to 7 dB, but
  on this capture 16 dB decodes more: the extra weak/noise detections a
  lower threshold produces each claim a per-channel demod slot but
  rarely convert.
- **Channelization** — per-burst `Ddc` to the 250 kHz channel rate,
  **28 kHz** one-sided passband (gr's gentle `input_fir` passes energy
  out to ~28 kHz too).
- **Fine CFO** — gr's squared-FFT estimate: square the preamble+UW
  (removes the BPSK → tone at 2·CFO), Blackman window, 16× zero-padded
  FFT, quadratic interpolation, halve. Re-estimated **per frame** in
  the multi-frame loop; a ±640 Hz residual-CFO grid (`CFO_REFINE=2`) is
  searched jointly with timing in the sync correlation.
- **Sync** — full **28-symbol** (16 preamble + 12 UW) coherent
  correlation at full sample resolution, DL and UL, free initial phase;
  no magnitude gate (the access code + CRC decide).
- **Multi-frame** — decode **every** TDMA time-slot frame in one
  detector window (`handle_multiple_frames_per_burst`), advancing by
  each frame's symbol count. 24 ms post-roll keeps detection alive
  across the short inter-slot gaps so adjacent frames land in one
  window.
- **Demod** — 1st-order decision-directed PLL (α=0.2) on the payload,
  carrier seeded from the UW correlation; differential decode in
  iridium-toolkit pair order.
- **End-of-frame** — trim only after **3 consecutive** symbols below
  **peak/8** (−18 dB from the burst's own max), reading a full
  ≤191-symbol frame. Breaking on the first payload symbol below
  `noise×4` (absolute) truncates weak frames: a weak burst sits right
  at that floor, so one faded symbol cuts the frame below the
  ~190-symbol BCH/CRC length and loses it.
- **Validation** — differential access-code gate at the random-match
  boundary (≤12 of 24 bits; the 24-bit CRC, false-pass ≈ 6e-8, is the
  real arbiter) → BCH (matches iridium-toolkit exactly) → CRC.
- **Two-filter union** — every burst is demodulated both unfiltered and
  through the RRC matched filter; the two recover largely *disjoint*
  populations (strong vs weak: 182 vs 758 CRC-OK alone), so both are
  emitted and deduped by decoded content.

**Tested and rejected** (standing warnings): lowering the detector
threshold floods with noise detections that never decode and starve the
demod slots; a preamble decision-directed PLL hurts (the 28-symbol
batch correlation already extracts the optimal phase, and the onset
preamble symbols are RRC-edge-unreliable). `MIN_BURST_SPAN=0`
(extracting single-frame bursts, as gr does) adds frames offline but is
**offline-only** — on the live station it floods the per-channel decode
queues and grows `chan_dropped`, losing more than it gains; the default
(2) keeps the soak drop-free at 10 MS/s.

Not count-gated in CI: the capture is 11 GB, too large to vendor. The
demod core is fenced by the bit-exact
(`demodulates_gr_iridium_test_burst`) and field-exact
(`offair_ida_sbd_decodes_with_crc`) oracle tests instead.

## Beam-pattern reconstruction (`src/beam.rs`)

IRA ring-alert frames carry two position kinds at different altitudes: the
broadcasting satellite (~780 km) and a ground beam footprint (~0 km;
iridium-toolkit's "down" / beam positions). `classify_altitude` splits them
into satellite-track updates and footprint observations, dropping anything
outside the physical bands so a BCH/CRC false-pass cannot plant a phantom.

Each footprint is de-rotated into the broadcasting satellite's own frame
(cross-track / along-track km), so a beam accumulates a stable mean
regardless of where the satellite is when it is heard. Direction (north/
south) comes from the geocentric-z trend of successive fixes; it is sticky
across short gaps so a sparsely-heard satellite still projects.

The drawn pattern is the canonical **48-beam, 4-tier** layout — 3 Main
Mission Antennas × 16 beams, tiers of 3 / 9 / 15 / 21 from nadir outward
(MathWorks Satellite Communications Toolbox Iridium model, FCC filings).
Tier ground radii are the off-nadir boresight angles (~11° / 24° / 42° /
59°) projected from 780 km onto the Earth sphere via
`Δ = asin((R+h)/R · sinθ) − θ`, `ground = R·Δ`. The three inner tiers match
the ~1480 km extent a single station actually decodes; the **outer tier is
stretched to the documented ~2250 km radius / ~4500 km footprint** (edge
~62°, just inside the 62.97° horizon limb). The station only demodulates the
stronger mid-footprint beams, so faint limb beams illuminate the ground but
rarely decode here — they belong on the map as modelled coverage even when
unheard. The outer band is wide because oblique projection radially
elongates limb beams.

Beams render in three tiers of confidence: **active** (a spot beam swept
this station within ~30 s) in beam colour; merely **decoded** (≥2 low-scatter
observations) as muted grey at its *measured* position, tracking the
satellite as it moves; and not-yet-decoded **modelled** slots as a faint
dashed gap-fill at the canonical position, so the whole intended 48-beam
pattern always shows plus exactly what has been heard. A polluted average
(RMS scatter > 600 km, i.e. direction-fold) falls back to the modelled slot.

Footprint polygons and the satellite ground track are **unwrapped across the
±180° antimeridian**: every cell vertex and trail point is shifted into the
±180° window of the satellite's sub-point, so a footprint west of the date
line reads as e.g. -185° and Leaflet draws it across the seam (off the edge)
instead of the long way across the whole map. The dashboard shows one
satellite's pattern at a time (click to pin); satellites expire 2 min after
last contact.

## SBD multi-packet reassembly — Layer B (`crates/xng-mode-iridium/src/sbd.rs`)

SBD reassembly is two layers. **Layer A** rebuilds an IDA packet from its DA
bursts (by continuation/counter). **Layer B** joins multiple IDA packets into
one SBD message: a long ACARS/SBD body is split across `packets` IDA frames
numbered `1..=packets`, whose bodies concatenate in order. The count comes
from the 7608 downlink `0x26` pre-header (`packets` at byte 3, mapped to
`msgcnt`); each packet's sequence number is `msgno`.

`parse_l2` routes by header: `msgno==0` (header-less / mailbox-check) or
`msgcnt<=1 && msgno==1` parse immediately; `msgcnt>1 && msgno==1` buffers a
`MultiSbd`; `msgno>1` appends to the matching open message (same direction,
`msgno==prev+1`), completing at `msgno==msgcnt` and tagging the result
`multi_packets` = packet count. Partials expire after `SBD_MULTI_EXPIRE_S`.
Covered by `sbd_multi_packet_reassembles` (a 2-packet 7608 message
reassembles and is marked `multi_packets=2`) and `sbd_multi_packet_expires`.

In practice multi-packet messages are rare: the live downlink is almost all
single-packet (`packets=1`), control-plane traffic. The path is verified
against the real frame format and waits for a body that spans two packets.
