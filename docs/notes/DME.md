# DME receive (`--mode dme`)

Passive receiver for DME and TACAN ground transponders (ICAO Annex 10 Vol I
§3.5). xng never transmits, so it cannot measure its own distance; it reports
what a beacon sends: its pulse pairs, their rate, its Morse ident, and its
channel and paired VOR or ILS.

```bash
# 64X reply (1151 MHz), RTL-SDR at 2.4 MS/s
xng listen --sdr driver=rtlsdr -m dme -r 2400000 -c 1151.0M --channels 1151.0M
# Two adjacent channels from one capture
xng listen --sdr driver=rtlsdr -m dme -r 2400000 -c 1151.5M --channels 1151.0M,1152.0M
```

```text
09:09:48.350 [dme] 1151.000 MHz DME ident SFO (... ..-. ---) 64X
09:09:48.561 [dme] 1151.000 MHz DME status 64X reply 2946 pp/s (ident 296) snr 13.0 dB ident SFO
```

Find the reply frequency from the navaid's VHF frequency with the pairing
table (`xng_mode_dme::channels::DmeChannel::from_vhf`): a VOR on 113.00 MHz is
77X, whose reply is on 1164 MHz. DME, like ADS-B, is vertically polarized; a
1090 MHz antenna works.

## Messages

`kind = "ident"` when a Morse ident is decoded (every 30–40 s per beacon):

| field | |
|---|---|
| `ident`, `morse` | `SFO`, `... ..-. ---` |
| `dot_ms`, `ident_rate_hz` | measured keying speed and pair rate (1350 nominal) |
| `repeats` | times in a row the same ident was decoded |
| `channel`, `role`, `spacing_us` | `64X`, `reply` (or `interrogation`), 12 |
| `paired_vhf_hz`, `paired_kind` | VOR or ILS frequency the channel pairs with |

`kind = "status"` every 10 s of signal: `pps` (all pairs), `ident_pps`,
`random_pps` (squitter and replies), mean pair `snr_db`, `level_dbfs`,
`last_ident`, `reference_bursts_per_s` and `tacan` (15 Hz main reference
bursts seen), plus the channel fields.

Squitter and replies are the same pulse pair, so on the reply frequency only
their sum is observable. Tune the interrogation frequency of the channel
instead (for example 1088 MHz for 64X) and the same decoder reports the
interrogations it hears from nearby aircraft (`role: interrogation`): the
locally received interrogation rate. That is not the beacon's reply load:
the receiver and the beacon hear different aircraft, and a beacon does not
answer every interrogation.

Some frequencies carry two links: 1025 MHz is the 64Y reply (30 µs) and the
1X/1Y interrogation (12/36 µs); 1088 MHz the 1Y reply and the 64X
interrogation. The decoder watches every spacing used there. Status reports
describe the busiest spacing. Idents are never taken from an interrogation
spacing (aircraft send none). An ident on a spacing that is not the busiest
is held for 3 s and dropped only if a busier spacing decodes the same text
(a strong channel's pulse skirts make weak pairs at the other spacings that
repeat its ident), so a reply's ident is still reported under heavier
interrogation traffic.

## How it works

- **Channel**: DDC to 1 MS/s, ±300 kHz. One sample per microsecond makes the
  12/30/36 µs spacings whole samples.
- **Pairs**: a Gaussian matched filter (3.5 µs wide) gives the pulse power
  `z`; for each spacing `D` the decision is on `z[n−D] + z[n]`, the energy of
  both pulses together, against a noise floor from block medians. Each half
  must also hold some power and at least 12 % of the other (a lone strong
  pulse is not a pair), and only the strongest candidate within the pair
  window survives (pulse skirts plus noise are not a second pair). Timing is
  refined with a parabolic fit.
- **Ident**: ident pairs repeat exactly every 740.7 µs, squitter is random.
  The period is learned from triples with equal gaps; a pair one to four
  periods after another pair is an ident hit. Hits per 10 ms bin, above the
  random-coincidence background, give the keying; a majority vote and a
  glitch merge clean it, and run lengths are read as Morse with an adaptive
  dot length. Because periodicity separates ident pairs from noise, the ident
  detector also gets pairs below the report threshold, which is why idents
  decode where most single pairs are lost.
- **Channel plan**: all 252 X/Y channels with their reply and interrogation
  frequencies, spacings and the VOR/ILS pairing (`channels.rs`).

## Benchmark against gr-dme and OnAir

These are the open-source DME decoders we found, each run as an installed
black box (no code from either is in xng):

- [gr-dme](https://github.com/daniestevez/gr-dme) (GNU Radio, GPL-3.0), with
  the settings of its own example flowgraph. It detects 12 µs pairs and has
  no ident decoder.
- The DME mode of [OnAir](https://github.com/adswill/OnAir) (GPL-3.0), through
  its command line receiver. It reads pairs and the ident, but prints only
  the pair rate of the last second and the ident letters, not pair times, so
  its pairs are counted (false pairs included) rather than matched.

All three got the same synthetic signals: 2.5 MS/s, 2700 pp/s random pairs
plus an "SFO" ident, three 5 s signals per SNR (46,667 pairs per SNR). SNR is
the peak pulse power over the noise in 1 MHz. Method and scripts:
[`bench/dme`](../../bench/dme/README.md); raw numbers:
[`bench/dme/results.json`](../../bench/dme/results.json).

![Pulse pairs and ident against SNR, and CPU time](../screenshots/dme-bench.png)

| SNR (dB) | xng pairs | xng false/s | gr-dme pairs | gr-dme false/s | OnAir pairs counted | xng ident | OnAir ident |
|---:|---:|---:|---:|---:|---:|:---:|:---:|
| 3 | 7.3 % | 2.5 | 0.0 % | 0.0 | 0.0 % | 3/3 | 0/3 |
| 4 | 17.6 % | 3.5 | 0.0 % | 0.0 | 0.0 % | 3/3 | 0/3 |
| 5 | 37.1 % | 3.9 | 4.0 % | 10.5 | 0.0 % | 3/3 | 0/3 |
| 6 | 63.5 % | 2.8 | 12.7 % | 13.9 | 0.0 % | 3/3 | 0/3 |
| 7 | 86.5 % | 1.6 | 25.9 % | 13.4 | 0.0 % | 3/3 | 0/3 |
| 8 | 97.4 % | 0.4 | 45.7 % | 13.1 | 0.1 % | 3/3 | 0/3 |
| 9 | 99.8 % | 0.1 | 65.4 % | 12.5 | 0.2 % | 3/3 | 0/3 |
| 10 | 99.9 % | 0.0 | 78.6 % | 11.7 | 0.5 % | 3/3 | 0/3 |
| 12 | 99.9 % | 0.0 | 91.9 % | 9.3 | 2.9 % | 3/3 | 0/3 |
| 15 | 99.9 % | 0.0 | 96.3 % | 6.6 | 27.5 % | 3/3 | 0/3 |
| 20 | 99.9 % | 0.0 | 98.6 % | 4.3 | 99.5 % | 3/3 | 0/3 |

"False" counts every detection more than 1.5 µs from a transmitted pair
(after removing each decoder's constant timing offset), so a badly timed real
pair counts too, and so does a second detection of a pair that was already
matched.

- **Sensitivity**: xng reaches 50 % of pairs about 3 dB below gr-dme and
  97 % at 8 dB, where gr-dme finds 46 %. At high SNR xng finds 99.9 % to
  gr-dme's 98.6 %, with no false pairs from 10 dB up (gr-dme 4 to 12 per
  second). OnAir counts under 1 % of the pairs up to 10 dB, 3 % at 12 dB,
  28 % at 15 dB and all of them at 20 dB: it needs about 11 dB more signal
  than xng.
- **None of the six gr-dme thresholds tried (4 to 10) closes the gap.** At
  6 dB, lowering gr-dme's threshold from 10 to 4 raises it from 13 % to 62 %
  of pairs but to 94 false pairs per second; xng finds 63 % with 4.
- **Ident**: xng decoded "SFO" in every run down to 3 dB, where it detects
  7 % of single pairs, and at 15 dB with 0.1 s and 0.16 s dots too (the
  Annex 10 limits).
  OnAir read it correctly in none: nothing below 20 dB, and wrong letters
  ("FM", "EATE", "IAT") at 20 dB; at 15 dB with 0.1 s or 0.16 s dots it read
  nothing. gr-dme has no ident decoder.
- **No beacon** (20 s of noise): xng reports 0.05 false pairs per second, gr-dme
  and OnAir none; no decoder reports an ident.
- **Robustness** (8 dB): with a 20 kHz carrier offset xng finds 97 % (gr-dme
  39 %), at 60 kHz 91 % (26 %), and with cos² pulses instead of the Gaussian
  both matched filters assume 97 % (45 %).
- **CPU**: 20 s of 2.5 MS/s IQ took 1.76 s of CPU in xng (one thread, from
  reading the file through the DDC and decoder to writing the pair times)
  and 9.22 s in gr-dme's flowgraph over the same work, 5.3 times less. xng
  runs at 11 times real time on one core of the benchmark machine. OnAir
  reports 0.87 s for its front end and decoder (without reading the file):
  it thresholds the pulse envelope, which is cheaper than xng's matched
  filter and likely why it needs a much stronger signal.

Not yet measured: a real capture. Daniel Estévez's public LimeSDR DME
recording (Zenodo) could not be downloaded from the build environment; the
first off-air capture should go into `xng-corpus` under `iq/dme/`.
