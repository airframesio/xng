# Radio time signals (multi-band WWV/WWVH/CHU + LF catalog) — implementation notes

Native multi-band radio time-signal decode core for `xng-mode-time`. "Time"
is a meta-mode: a family of standard-frequency / time-signal broadcasts spread
across the **LF** (< 300 kHz) and **HF** (3–30 MHz) bands. A single SDR can
only see part of the spectrum at once, so the crate is built around a **station
catalog with capability-ranked auto-scan**: given the SDR's tunable range it
returns the receivable stations, decodable digital/BCD stations first.

Two HF stations are decoded end-to-end; the rest are catalogued (their carriers
feed auto-scan) with the decoders deferred — exactly the verification posture
DSC/EOT/ATCS landed under (no off-air IQ exists, so the loopback + AWGN tests
are the CI oracle; no real-RF claim is made).

Status: **WIRED, SYNTHETIC-ONLY.** Runtime mode `Mode::Time`, body
`MessageBody::Time { station, details }`, and a `TimeChannelDecoder` that owns
an `xng_dsp::Ddc`. The CHU and WWV/WWVH decoders are green on loopback + AWGN;
the LF stations are catalog-only (documented follow-up below).

## Station catalog

`xng_mode_time::catalog` — each entry has a name, location, the broadcast
carriers (Hz), a band class (LF / HF), a modulation family, and a decode
capability (`Decode` vs `CatalogOnly`).

| Station | Carriers | Band | Modulation | Capability |
|---|---|---|---|---|
| **WWV**  | 2.5 / 5 / 10 / 15 / 20 MHz | HF | AM + 100 Hz subcarrier BCD | **decode** (WWV) |
| **WWVH** | 2.5 / 5 / 10 / 15 MHz | HF | AM + 100 Hz subcarrier BCD | **decode** (WWV) |
| **CHU**  | 3330 / 7850 / 14670 kHz | HF | AM + AFSK (Bell-103) | **decode** (CHU) |
| BPM   | 2.5 / 5 / 10 / 15 MHz | HF | AM carrier + tone schedule | catalog-only |
| RWM   | 4996 / 9996 / 14996 kHz | HF | carrier + A1/A2 ticks | catalog-only |
| YVTO  | 5 MHz | HF | carrier + tone | catalog-only |
| WWVB  | 60 kHz | LF | AM pulse-width (0.2/0.5/0.8 s dip) | catalog-only |
| DCF77 | 77.5 kHz | LF | AM pulse-width + phase | catalog-only |
| MSF   | 60 kHz | LF | on-off keyed pulse-width | catalog-only |
| JJY   | 40 & 60 kHz | LF | AM pulse-width | catalog-only |
| TDF   | 162 kHz | LF | phase-modulated time code | catalog-only |
| RBU   | 66.66 kHz | LF | LF carrier + code | catalog-only |

### Auto-scan by capability

`catalog::receivable(lo_hz, hi_hz) -> Vec<Receivable>` returns every catalog
carrier in `[lo, hi]`, **ranked**: fully decodable digital/BCD stations (CHU
AFSK, WWV/WWVH 100 Hz BCD) first, then HF carrier+tone stations (BPM/RWM/YVTO),
then catalog-only LF stations. `best_per_station` collapses a multi-carrier
station (e.g. WWV's five harmonics of the same time code) to one channel. This
is what `xng scan --mode time` / `xng listen --mode time` use to pick channels
from the SDR's range. The worked HF channel plan lives in `commands/scan.rs`
(`plan(Mode::Time)`).

## CHU decoder (`chu.rs`) — the flagship

CHU (NRC Ottawa) broadcasts a digital time code in audio seconds 31–39 as
**Bell-103 AFSK: MARK = 2225 Hz (logical 1 / idle), SPACE = 2025 Hz (logical
0), 300 baud, 8N2** async (1 start = space, 8 data LSB-first, 2 stop = mark; 11
bits/char). A packet = **10 bytes (110 bits) = 5 data + 5 redundancy**, each
byte two BCD nibbles. Per second: 0–10 ms 1000 Hz tick, 10–133.3 ms MARK
preamble, 133.3–500 ms the 110 data bits.

- **Format A** (sec 32–39): `[6][D][D][D][H][H][M][M][S][S]` (day-of-year +
  UTC h/m/s); redundancy = **exact copy** → validity gate `data == copy`.
- **Format B** (sec 31): `[X][Z][Y][Y][Y][Y][T][T][A][A]` (year, DUT1, TAI−UTC,
  DST); redundancy = **ones-complement** → validity gate `data == ~copy`.

```
channel IQ → Ddc → am_envelope → bandpass ~1900–2350 Hz
  → mark/space Goertzel discriminator (2225/2025), falling-edge start hunt
  → 8N2 UART receiver (LSB-first, fs/300 samples/bit, 2 stop = mark)
  → 10 BCD bytes → parse_packet (A or B by redundancy match) → validity gate
```

Format A gives time-of-day; Format B gives the year. The runtime combines them.

## WWV / WWVH decoder (`wwv.rs`)

WWV and WWVH carry an **identical** 100 Hz subcarrier BCD time code (modified
IRIG-H), 1 bit/second, 60-second frame. Per-second symbol = a 100 Hz burst
whose length codes the bit: **0 = 170 ms, 1 = 470 ms, marker = 770 ms** (nominal
200/500/800 ms minus a 30 ms suppressed lead-in). **Second 0 is a hole** =
frame reference; markers at {9,19,29,39,49,59}. BCD is **LSB-first, weights
1-2-4-8**.

```
channel IQ → Ddc → am_envelope
  → per-second: 100 Hz bandpass + windowed Goertzel → measure tone-burst length
  → classify {<0.32→0, 0.32–0.62→1, >0.62→marker, ~0→hole}
  → frame-sync on the sec-0 hole + the six markers → parse the BCD field map
  → full UTC (year + day-of-year + h:m, second 0 at the minute mark)
```

Station label: **WWV = 1000 Hz tick, WWVH = 1200 Hz tick** (the time code is
identical) — picked by comparing the two tick-tone energies.

## Pipeline (both decoders)

`TimeChannelDecoder::new(input_rate, freq_offset_hz)` owns an `xng_dsp::Ddc`
that mixes a wideband capture by the offset and decimates to `CHANNEL_RATE`
(12 000 S/s — carries the CHU 2225 Hz tone at 40 samples/bit and the WWV 100 Hz
subcarrier), AM-demodulates to audio, and runs the carrier-selected decoder
(`with_carrier(hz)` → CHU vs WWV via `decoder_for_carrier`). `to_message`
emits `MessageBody::Time { station, details }`; `details` JSON carries the
station, decoded UTC (ISO-8601 when full), the individual fields
(year/doy/h/m/s), DUT1, leap/DST flags, the validity gate, and a sync
confidence (CHU: redundancy + framing; WWV: marker-grid hits / 7).

## Tests

`cargo test -p xng-mode-time`:

- **Table tests** (`src/chu.rs`, `src/wwv.rs`, `src/catalog.rs`, `src/audio.rs`)
  anchor the BCD/redundancy/IRIG-H field maps, the catalog carriers, and the
  audio DSP to the published broadcast formats.
- **Loopback** (`tests/loopback.rs`, `*_synth`): modulate a known UTC →
  `TimeChannelDecoder` → assert the decoded UTC equals the input and the
  validity gate passes (CHU Format A; WWV full minute; WWVH tick labelling;
  CHU through the DDC at a carrier offset).
- **Synthetic AWGN**: modulate → add complex Gaussian noise (LCG + Box-Muller,
  the EOT/sonde bench pattern) → demod → require the validated UTC back on the
  large majority of seeds at a moderate SNR.

## Catalog-only follow-up (deferred decoders)

The LF stations (WWVB, DCF77, MSF, JJY, TDF, RBU) and the HF carrier+tone
stations (BPM, RWM, YVTO) are catalogued but not decoded:

- **LF capture path.** xng has no LF (< 300 kHz) front end yet; WWVB/DCF77/MSF/
  JJY/TDF/RBU need one before their decoders are worth wiring. WWVB is the
  most tractable (the same idea as WWV via AM **pulse-width** {0.2/0.5/0.8 s}
  carrier-power dips, BCD); DCF77 adds a phase-modulated code, MSF is on-off
  keyed, TDF is phase-modulated on the Allouis carrier.
- **HF carrier+tone.** BPM/RWM/YVTO broadcast a seconds-tick / tone schedule
  rather than a digital time code we currently decode; they are surfaced for
  detection/labelling (carrier-tone capability) but carry no parsable date.

When an LF path lands, WWVB is the recommended first decoder (pulse-width PWM,
closest to the already-working WWV approach).
