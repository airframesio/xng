# VOR and ILS (localizer, glideslope)

xng measures VHF navigation aids from the ground: VOR radials, ILS
localizer and glideslope course readings (DDM), and the Morse idents. It
only receives; it never transmits. The crate is `crates/xng-mode-navaid`.
DME is a separate mode (`--mode dme`, [DME.md](DME.md)).

## Usage

```sh
# Two VORs in one capture (any rate that is a multiple of 48 kHz)
xng listen --sdr driver=rtlsdr --mode vor -r 960000 -c 113.000M \
    --channels 113.050,112.950 --http 0.0.0.0:8080

# Localizers: --mode ils picks localizer or glideslope from the frequency
xng listen --sdr driver=rtlsdr --mode ils -r 960000 -c 110.200M \
    --channels 110.100,110.300

# The glideslope paired with 110.10 MHz
xng listen --sdr driver=rtlsdr --mode ils -r 240000 -c 334.350M \
    --channels 334.400
```

Each channel reports once every 10 seconds. A report is one asf-2.0
message (mode `vor` or `ils`, a generic body whose `kind` is `vor`,
`localizer` or `glideslope`). The dashboard lists the channels on its
Navaids tab, and the console prints one line per report:

```
[vor] 113.050 MHz VOR SAC radial 123.3° ±1.1 C/N0 50 dB-Hz [normal]
[ils] 110.300 MHz LOC IOAK DDM -0.023 (-22 µA) SDM 40% | 2nd carrier -8 dB DDM +0.304 C/N0 46 dB-Hz [normal]
[ils] 334.400 MHz GS DDM +0.040 (34 µA) SDM 80% C/N0 52 dB-Hz [normal]
```

Navaid reports are not sent to the Airframes per-mode ingests. They go out
over asf-2.0 and the JSON outputs only.

### Report fields

| Field | Meaning |
| --- | --- |
| `status` | `no_signal`, `acquiring`, `normal` (ident heard), `no_ident` (carrier on the air for 60 s with no keying, the usual maintenance signature), `test` (keying TST or TEST) |
| `cn0_dbhz` | Carrier to noise density (dB-Hz), calibrated to within 0.2 dB on synthetic noise |
| `carrier_offset_hz` | Carrier offset from the channel frequency, from the PLL |
| `ident`, `ident_votes` | The ident once it is certain, and how many repetitions agree |
| `ident_depth_pct`, `ident_tone_hz` | Ident modulation depth and measured tone frequency |
| `vor.radial_deg`, `vor.radial_std_deg` | Radial over the report interval and its spread (absent when no block carried both tones) |
| `vor.mod30_pct`, `vor.mod9960_pct`, `vor.deviation_hz`, `vor.subcarrier_offset_hz` | Signal health: 30 Hz AM depth, subcarrier depth, FM deviation (strong signals only), subcarrier frequency error |
| `ils[].ddm`, `ils[].ddm_ua`, `ils[].ddm_std` | Difference in depth of modulation, the same in µA of course deflection (150 µA at 0.155 for a localizer, 0.175 for a glideslope), and its spread |
| `ils[].sdm_pct`, `ils[].mod90_pct`, `ils[].mod150_pct`, `ils[].phase_lock_deg` | Sum of depths and the two tone depths, and the 90/150 Hz phase relationship (absent when a tone is missing) |
| `ils[].tone90_hz`, `ils[].tone150_hz` | Measured tone frequencies (Annex 10 allows 2.5 % off nominal; depths are measured at these) |
| `ils[].carrier_offset_hz`, `ils[].relative_db` | For two-frequency systems, one entry per carrier (course first) |

## How it works

1. **Carrier.** The channel is mixed to 48 kHz. A 32768-point FFT finds the
   carrier (two for a localizer or glideslope: two-frequency systems put
   the course and clearance carriers 5 to 14 kHz apart). Each carrier gets
   its own second-order PLL (15 Hz loop bandwidth), so AM detection is
   coherent: the in-phase arm holds the signal and the noise in the
   quadrature arm is discarded, 3 dB better than an envelope detector, and
   the two carriers of a two-frequency system never beat against each
   other. The carriers are searched again every 20 seconds.
2. **VOR.** The in-phase signal is decimated to 2400 Hz (80 samples per
   30 Hz cycle). The variable phase is a single-bin DFT of the 30 Hz AM.
   The reference phase comes from the 9960 Hz subcarrier: the FM is not
   demodulated. Instead each 0.5 s block is folded onto one 30 Hz cycle and
   correlated against `exp(-j·16·sin(ωt + φ))` templates for 360 reference
   phases, the maximum-likelihood estimate. It keeps working at
   subcarrier SNRs where an FM discriminator has broken down. A
   subcarrier frequency error (the tolerance is ±1 %, about ±100 Hz) is
   found by a comb search over the FM line spectrum, whose lines are 30 Hz
   apart, and the 30 Hz alias is resolved by the fold correlation.
   Blocks are weighted by their 30 Hz and subcarrier depths and averaged
   on the circle.
3. **ILS.** 90 and 150 Hz single-bin DFTs on whole 30 Hz cycles in 0.5 s
   blocks give the two depths, DDM and SDM.
4. **Ident.** The 1020 Hz tone is located to within a fraction of a Hz,
   narrowed to ±5 Hz and keyed into a Morse decoder with an adaptive
   threshold. Idents repeat every few seconds, so the decoder also folds
   up to two minutes of the tone envelope on the repetition period (found
   by autocorrelation) and decodes the average, gaining 3 dB each time the
   number of repetitions doubles. A folded word is reported only when
   every mark and space stands clear of the threshold, it fits the Annex
   10 ident rules (VOR: 2 or 3 letters; localizer: 2 or 3 letters, optionally after an I;
   TST/TEST), and two folds in a row agree. Below its threshold the
   decoder reports no ident rather than a wrong one.

## Benchmarks

`bench/navaid/run.py` compares xng with the open-source decoders on the
same synthetic IQ (8-bit, the way an RTL-SDR records it, with the total
level at a quarter of full scale). `bench/navaid/build-competitors.sh`
builds them; they run as black boxes and none of their code is in xng.

| Decoder | Measures | Front end | How it was run |
| --- | --- | --- | --- |
| vortrack | VOR radial | RTL-SDR only | our file-replay `librtlsdr` stub (`rtlstub.c`) |
| voracious 0.5.2 | VOR radial and ident, localizer DDM and ident | file input | `-f cu8 -w 2` |
| dumpils | localizer and glideslope DDM | RTL-SDR only | our harness calling its demodulator package on file IQ (`dumpils-file/`) |

Signals follow Annex 10 nominal values (30 % depths, 16 deviation index,
20 % localizer and 40 % glideslope tone depths, 10 % ident at 7 wpm);
noise is set as C/N0. Scores: radial and DDM errors after a 6 s warmup on
2 s reports (xng reports every 2 s in the benchmark); ident `correct` is
the fraction of runs whose final ident is right, `wrong` the number of
reports with a wrong ident per run.

### VOR radial

RMS radial error (2 s reports, four radials per level) by C/N0 in dB-Hz:

| Decoder | 30 | 34 | 38 | 42 | 46 | 50 | 55 |
|---|---|---|---|---|---|---|---|
| **xng** | **3.9°** | **2.5°** | **1.5°** | **1.1°** | **0.7°** | **0.5°** | 0.3° |
| vortrack | 103° | 90° | 101° | 108° | 101° | 44° | 0.2° |
| voracious | 100° | 104° | 99° | 110° | 94° | 93° | 0.4° |

Reports within 2° of the truth:

| Decoder | 30 | 34 | 38 | 42 | 46 | 50 | 55 |
|---|---|---|---|---|---|---|---|
| **xng** | **39 %** | **55 %** | **82 %** | **93 %** | **100 %** | **100 %** | 100 % |
| vortrack | 2 % | 0 % | 0 % | 0 % | 0 % | 61 % | 100 % |
| voracious | 0 % | 5 % | 0 % | 0 % | 0 % | 0 % | 100 % |

vortrack and voracious lose the radial between 50 and 55 dB-Hz, the sharp
edge of a detector threshold (an FM discriminator on the subcarrier breaks
down about there). xng's maximum-likelihood reference phase degrades
gradually instead, and holds about 1° some 13 dB lower. On strong signals all three agree to a
few tenths of a degree.

Impairments (RMS error at 40 dB-Hz / 55 dB-Hz):

| Impairment | xng | vortrack | voracious |
|---|---|---|---|
| Carrier 3 kHz off frequency | **1.4°** / **0.2°** | 101° / 0.3° | 100° / 0.3° |
| Subcarrier 40 Hz off (0.4 %) | **1.2°** / **0.2°** | 96° / 0.3° | 93° / 0.4° |
| 30 % voice modulation | **1.1°** / **0.2°** | 112° / 3.4° | 110° / 0.4° |
| Doppler VOR, 10 % sideband asymmetry | **1.1°** / **0.2°** | 109° / 0.2° | 90° / 0.4° |

### Localizer and glideslope DDM

RMS DDM error (four DDMs per level, including full scale):

| Decoder | 30 | 34 | 38 | 42 | 46 | 50 | 55 |
|---|---|---|---|---|---|---|---|
| **xng** localizer | **0.031** | **0.019** | **0.014** | **0.007** | **0.005** | **0.003** | **0.002** |
| dumpils localizer | 0.076 | 0.076 | 0.076 | 0.076 | 0.074 | 0.071 | 0.061 |
| voracious localizer | 0.077 | 0.077 | 0.077 | 0.077 | 0.077 | 0.077 | 0.077 |
| **xng** glideslope | **0.036** | **0.019** | **0.010** | **0.006** | **0.005** | **0.003** | **0.002** |
| dumpils glideslope | 0.076 | 0.076 | 0.076 | 0.075 | 0.073 | 0.071 | 0.061 |

Two-frequency localizer (course carrier 4 kHz above the channel, clearance
carrier 4 kHz below and 10 dB weaker with a 0.3 DDM): xng 0.007 / 0.002 at
40 / 50 dB-Hz, dumpils 0.076 / 0.071, voracious 0.077 / 0.077.

0.002 DDM is about 2 µA of localizer deflection. The two other decoders
were checked on clean signals so these numbers are theirs and not the
harness's: dumpils reads exactly right without noise (96.8 µA for 0.1 DDM)
but its reading shrinks toward zero as noise rises, while voracious 0.5.2's ILS output
does not follow the DDM at all on these signals (about −0.21 for both
−0.1 and +0.1 without noise). xng's coherent detector keeps the
quadrature noise out of the depth estimates, so its DDM has no noise bias.

### Idents

Four idents per level (SAC, OAK, LIN, SWR for VORs; ISAC, IOAK, IJFK,
ILAX for localizers), 60 s each, ident every 10 s at 7 wpm and 10 % depth.
`correct` is the share of runs whose last reported ident is right, `wrong`
the average number of reports per run that carry a wrong ident.

| Decoder | | 30 | 34 | 38 | 42 | 46 | 50 |
|---|---|---|---|---|---|---|---|
| **xng** VOR | correct | 0 % | 0 % | 0 % | 100 % | 100 % | 100 % |
| | wrong | 0.0 | 0.0 | 0.0 | 0.0 | 0.0 | 0.0 |
| voracious VOR | correct | 0 % | 0 % | 0 % | 0 % | 0 % | 0 % |
| | wrong | 12.2 | 10.5 | 11.2 | 12.0 | 11.2 | 13.5 |
| **xng** localizer | correct | 0 % | 0 % | 0 % | 100 % | 100 % | 100 % |
| | wrong | 0.0 | 0.0 | 0.0 | 0.0 | 0.0 | 0.0 |
| voracious localizer | correct | 0 % | 0 % | 0 % | 0 % | 0 % | 0 % |
| | wrong | 23.0 | 23.0 | 23.0 | 23.0 | 23.0 | 23.0 |

xng named every station from 42 dB-Hz and reported no wrong ident:
below its threshold it reports none. voracious reported a wrong ident in
every run, even on strong signals (SAC read as SEO and ZEO at 55 dB-Hz),
so its ident cannot be trusted at any level here.

### CPU

CPU seconds per second of signal, one channel, 8-bit IQ read from a file
(the xng and vortrack/voracious runs at 2 MS/s, dumpils at its fixed
1.31 MS/s):

| Decoder | VOR | Localizer | Glideslope |
|---|---|---|---|
| xng | 0.031 | 0.030 | 0.028 |
| vortrack | 0.018 (radial only) | – | – |
| voracious | 1.29 | 0.68 | – |
| dumpils | – | 0.123 | 0.112 |

xng uses about 3 % of one core per channel and carries the ident,
status, C/N0 and health figures as well; vortrack is cheaper but measures
only the radial and holds it only above 50 dB-Hz.

Full numbers are in `docs/notes/navaid-bench.json`. Re-run with
`python3 bench/navaid/run.py` (about an hour on 4 cores; `--quick` for a
smoke run, `--only vor_radial,loc_ddm` for single tests).

## Not done yet

- Real off-air captures. Everything above is synthetic; the next step is
  recordings of local VORs and an ILS for regression tests.
- Doppler VOR sideband asymmetry is tolerated (benchmarked at 10 %) but
  not reported as a health figure.
- Navaid values are not Prometheus gauges yet (the per-channel level and
  frame counters are).
- `xng scan` has no navaid plan: VOR and ILS frequencies are site
  specific, so channels are given with `--channels`.
