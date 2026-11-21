# Provenance — xng-mode-time

Clean-room implementation of a multi-band radio time-signal decode core. No
decoder code was copied or ported; only published broadcast formats (carrier
frequencies, modulation parameters, time-code bit layouts) were used, each
cited below. Facts are not copyrightable; the implementation, modulators, and
tests are original.

## What this crate is

"Time" is a meta-mode covering the world's standard-frequency / time-signal
broadcasts across the LF (< 300 kHz) and HF (3–30 MHz) bands. The crate
provides a station catalog with capability-ranked auto-scan, two fully
implemented HF decoders (CHU AFSK, WWV/WWVH 100 Hz BCD), the shared audio DSP
they need, and a self-generated modulator used only by tests.

- `catalog` — the station table (WWV, WWVH, CHU, WWVB, DCF77, MSF, JJY, BPM,
  RWM, TDF, RBU, YVTO) with carriers, band class, modulation family, and decode
  capability, plus `receivable(lo, hi)` — the capability-ranked auto-scan that
  maps an SDR's tunable range to channels.
- `chu` — the CHU AFSK (Bell-103, 300 baud, 8N2) packet decoder.
- `wwv` — the WWV/WWVH 100 Hz subcarrier BCD (modified IRIG-H) decoder.
- `audio` — AM-envelope demod, Goertzel single-bin tone power, RBJ biquads.
- `modulate` — waveform synthesis used ONLY by tests.

## Verification posture — SELF-GENERATED modulate → demod path

There is **no off-air time-signal IQ** vendored or available paired with
ground truth, so — exactly like the DSC / EOT / ATCS cores landed — the demod
is validated synthetically:

1. The DECODE cores are anchored to the published broadcast formats by their
   own `#[test]` table tests: CHU BCD nibble layout + redundancy gate
   (`src/chu.rs`), WWV BCD weights + IRIG-H second-by-second field map and
   marker sync (`src/wwv.rs`), and the catalog carriers (`src/catalog.rs`).
2. `modulate.rs` synthesizes the on-air audio for a KNOWN UTC (CHU AFSK packet
   timing; WWV 100 Hz PWM with the 30 ms suppressed lead-in and the sec-0 hole)
   and AM-modulates it to IQ. `tests/loopback.rs` runs that IQ through the real
   `TimeChannelDecoder` (DDC + AM-envelope + the carrier-selected decoder) and
   asserts the recovered UTC equals the input AND the validity gate passes,
   including through the DDC at a carrier offset and under added complex AWGN.

The modulator is **not** an external reference — it only exercises the front
end; the waveform parameters it uses are the published broadcast facts. No
real-RF claim is made.

## Sources (broadcast facts / formats only)

### CHU — NRC Canada broadcast format + NTP `refclock_chu` (driver7)

- Carriers **3330 / 7850 / 14670 kHz**, AM. Digital time code in audio seconds
  **31–39**; **Bell-103 AFSK: MARK = 2225 Hz, SPACE = 2025 Hz, 300 baud, 8N2**
  async (1 start = space, 8 data LSB-first, 2 stop = mark; 11 bits/char).
  Per second: **0–10 ms 1000 Hz tick, 10–133.3 ms MARK preamble, 133.3–500 ms
  the 110 data bits** (last stop bit ends at exactly 500 ms).
  Source: National Research Council Canada **"CHU broadcast format"** /
  **"Information on the time signals"**, and the NTP reference-clock driver
  **`refclock_chu` (driver7)** documentation, which describes the 10-byte
  packet (5 data + 5 redundancy), the 8N2 / Bell-103 framing, and the two
  formats.
- **Format A** (seconds 32–39): data nibbles `[6][D][D][D][H][H][M][M][S][S]`
  (6 = frame id, DDD = day-of-year, HH/MM/SS = UTC); redundancy = **exact copy**
  of the 5 data bytes. **Format B** (second 31): data nibbles
  `[X][Z][Y][Y][Y][Y][T][T][A][A]` (X = leap/DUT1-sign code, Z = |DUT1| tenths,
  YYYY = year, TT = TAI−UTC, AA = Canada DST); redundancy = **ones-complement**
  of the 5 data bytes. Source: NRC CHU broadcast codes + `refclock_chu`.
  - NOTE: the exact sub-bit layout of the Format-B `X` nibble (which bit is the
    DUT1 sign vs the leap-second indication) is not unambiguously published; the
    code uses a conservative interpretation (high bit = DUT1 sign, low bits
    non-zero = leap pending) and surfaces the raw DST byte `AA` unmodified.

### WWV / WWVH — NIST time-code description (NIST SP-432)

- WWV carriers **2.5 / 5 / 10 / 15 / 20 MHz**; WWVH **2.5 / 5 / 10 / 15 MHz**,
  AM. Identical **100 Hz subcarrier BCD time code** (modified IRIG-H), 1 bit per
  second, 60-second frame. Per-second symbol = a 100 Hz tone burst whose length
  codes the bit: **binary 0 = 170 ms, binary 1 = 470 ms, position marker =
  770 ms** (nominal 200/500/800 ms minus a 30 ms tone-suppressed lead-in, so
  each pulse starts 30 ms after the true second). **Second 0 = a hole** (no
  pulse) = frame reference. BCD is **LSB-first, weights 1-2-4-8**.
  Markers at seconds {9,19,29,39,49,59}.
- Full 60-second field map (year units 4–7; P1=9; minute 10–17; P2=19; hour
  20–26; P3=29; doy 30–41; P4=39; P5=49; UT1 sign 50; year decade 51–54; DST1
  55; UT1 magnitude 56–58; P0=59) per the NIST WWV/WWVH time-code description.
  Source: **NIST Special Publication 432, "NIST Time and Frequency Services"**,
  and the **NIST WWV / WWVH** station pages (time-code format figure).
- Station label: **WWV = 1000 Hz** seconds tick, **WWVH = 1200 Hz** tick (the
  time code itself is identical). Source: NIST WWV/WWVH station pages.

### Other catalogued stations (carriers + modulation family only)

Carrier frequencies and modulation families for **WWVB** (60 kHz, AM
pulse-width), **DCF77** (77.5 kHz, AM pulse-width + phase), **MSF** (60 kHz,
on-off keyed), **JJY** (40 & 60 kHz, AM pulse-width), **BPM** (2.5/5/10/15 MHz,
HF carrier + tone schedule), **RWM** (4996/9996/14996 kHz, HF carrier + A1/A2
ticks), **TDF** (162 kHz, phase-modulated time code on the Allouis carrier),
**RBU** (66.66 kHz), and **YVTO** (5 MHz, carrier + tone) are taken from the
respective national time-service descriptions (NIST, PTB, NPL/NPL Anthorn,
NICT, NTSC, Russian VNIIFTRI, ANFR/France Inter, Observatorio Cagigal). These
are **catalog-only**: their carriers feed `receivable` for auto-scan, but no
decoder is wired (see `docs/notes/TIME.md` for the follow-up plan). The LF
stations additionally need an LF-capable capture path, which xng does not yet
have.

## DSP

The audio front end (`audio.rs`) is textbook DSP: coherent-magnitude AM
envelope demod, the Goertzel single-bin DFT recurrence, and RBJ audio-EQ
cookbook biquad bandpass / low-pass designs. No external code.
