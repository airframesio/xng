# Off-air decode benchmarks vs oracle decoders

xng (v0.20.0) is benchmarked against the strongest open decoder for each
mode on real off-air captures.

Method: one capture per mode, each decoder fed its native preferred
rate/format (resampled with scipy `resample_poly` where needed), unique
frames compared (unique raw frames unless noted). Where the capture is
small enough to vendor, the decode count is fenced by CI: `bench/run.sh`
decodes the fixture and fails if the count drops below its committed
floor in `bench/baselines.json`. Keys ending in `_max` are ceilings
(false-positive gates) instead. Aero, STD-C, and Iridium are fenced
separately by exact-result `cargo test` fixtures (their captures are too
large to vendor).

## Results

| mode | xng | oracle | capture | CI gate |
|---|---|---|---|---|
| ADS-B / Mode S | 164 | readsb 167 (98%) | modes1 @2.4 MS/s | floor (modes1 @2 MS/s) |
| ADS-B / Mode S | 161 | dump1090-fa 162 (99%) | modes1 @2 MS/s | floor |
| VDL2 | 44 | dumpvdl2 41 | sigidwiki | floor 42 |
| HFDL | 36 | dumphfdl 37 (97%) | 21931 kHz sigidwiki | floor 31 |
| AIS | 48 | AIS-catcher 53 (91%) | 5 min, Sacramento | fixture floor |
| Iridium IDA | 758 | gr-iridium 573 | 300 s Airspy R2 | oracle tests |
| Radiosonde (RS41) | 119 | rs1729 `rs41mod` 119 (100%) | radiosonde_auto_rx 96 kS/s | floor 110 |
| NAVTEX | 29 | fldigi/YaND (real USCG msg, char-identical) | SDRplay navtex.zip 62.5 kS/s | floor 25 |
| UAT 978 | 879 CRC-OK | (live; no oracle on this capture) | live 50 s, KSMF (not vendored) | — |

## ADS-B / Mode S

Capture: `modes1.bin`, the canonical dump1090 test capture
(antirez/dump1090 testfiles; 2.0 MS/s UC8, ~0.18 s, dense traffic). xng
decodes it natively at 2 MS/s; the oracles get it resampled to their
preferred rate (2.4 MS/s SC16).

Approach: magnitude PPM demod with near-floor candidate gates (the CRC
layer arbitrates, so strict pre-gates only cost frames). At fractional
rates (2.4 MS/s, the RTL-SDR's best) half-µs slots are integrated from
prefix sums with fractional edges and bits decided at interpolated
half-bit centers; the timing grid is scanned at several sub-sample phases
and the streams merged by bytes+position (bursts landing between samples
split pulse energy across slots). Pass count is effort-gated (live = 2
passes, max = 16; the count asymptotes at 16). xng decodes 5 frames
readsb misses and 7 that dump1090-fa misses.

Residual gap: ~3 frames vs readsb, attributable to readsb's
phase-classified per-phase bit templates. The modes1 captures saturate
both decoders, so further movement needs new captures with genuinely
weak/dense traffic.

Standing falsifications (do not retry without a stronger prior):
- In-frame collision rescanning pollutes the ICAO cache: false
  mid-frame DF11/DF17-shaped candidates fill the cache and evict real
  aircraft (−7 unique). No cache-clock policy fixes this; a cache large
  enough to be pollution-proof would weaken the overlay-DF trust model.
- Two-bit syndrome-pair repair for DF17 (known-ICAO-gated) gains zero
  frames and halves max-effort throughput.
- Replacing the on-grid stream with an interpolated one loses frames
  (the midpoint samples blur pulse/quiet contrast): the phases must be
  scanned independently and unioned, never substituted.

Reproduce:

```
xng decode modes1.cu8 -f cu8 -m adsb -r 2000000 -c 1090000000 --channels 1090
readsb --ifile modes1_24m.sc16 --iformat SC16 --no-fix --raw
dump1090 --ifile modes1_24m.sc16 --iformat SC16 --no-fix --raw
```

## VDL2

Capture: the sigidwiki off-air VDL2 capture. CI floor 42.

Approach: D8PSK burst demod. Differential correlation against the
16-symbol unique word gives the per-symbol carrier rotation; symbol-
spaced differential phase maps to the nearest π/4 multiple and inverse
Gray triplet, with decision-directed phase-drift tracking. Single-sample
symbol decisions, no matched filter. The Annex-10 transmit pulse is full
raised-cosine (α = 0.6, already Nyquist at the symbol instants), so an
RRC RX matched filter does not belong here: cascading RRC onto a
full-RC TX pulse produces an RC^1.5 response that injects ISI and
collapses off-air decode (1 frame vs 17 in testing). Differential
detection also beats coherent/absolute D8PSK on this capture (the
oscillator has phase wander that differential cancels).

The long-standing 19/41 gap was an MSB-vs-LSB Reed-Solomon symbol-
assembly bug (the RS code operates on octets in HDLC LSB-first wire
order; MSB packing makes the RS reject perfect codewords). It was found
via octet-level oracle ground truth from dumpvdl2's debug output; see
docs/notes/VDL2.md.

Residual gap: none on this capture (xng leads 44 vs 41). The remaining
RS failures on accepted bursts are soft-decision territory (per-symbol
confidence into RS erasure marking).

## HFDL

Capture: the 21931 kHz sigidwiki capture. CI floor 31.

Approach: burst demod with a T/2-spaced 15-tap LMS feed-forward
equalizer trained on the known T training segments, plus decision-
directed carrier tracking. Hunt is differential correlation against the
127-chip A sequence (CFO-immune); A1→A2 coherent phases refine the
carrier and resolve the global π ambiguity; M1 is matched against its 8
cyclic shifts to learn the rate/slot setting. When every PDU CRC in a
detected burst fails, the demod re-runs at small timing (±0.5/±1 sample)
and carrier (±2/±5 Hz) offsets with the PDU header CRC arbitrating (only
on failed bursts). Parser policy: no CRC-valid LPDU is ever silently
dropped (unparsable HFNPDUs emit an envelope event).

Residual gap: 1 frame vs dumphfdl. The frame-exact diff shows the data
LPDUs match the oracle one-for-one; the misses are the weakest bursts
(4.0-5.0 dB SNR at 300 bps), a sensitivity tail, not a convention bug.

Standing falsifications: wider ±2/±3-sample retry shifts gain nothing;
lowering the A1 detection threshold 0.4 → 0.32 is catastrophic (false A1
anchors consume real bursts, dropping to 19 events).

## AIS

Capture: 5 min at 162.000 MHz / 6 MS/s (Airspy Mini + VHF whip,
Sacramento; inland, so mostly weak distant type-4 base-station reports,
a sensitivity test by construction). CI fixture floor.

Approach: two demods run per channel. The streaming path is an
FM-discriminator GMSK demod (needs ~14 dB SNR). The weak-signal path is
coherent: a power gate finds bursts, a complex template (last preamble
bits + start flag) anchors them, fine CFO comes from the template phase
slope, and a 16-state GMSK-exact MLSE Viterbi (phase-quadrant × two
in-flight levels, branch waveforms synthesized from the true BT=0.4
Gaussian phase pulse) decodes them, with both GMSK and MSK pulse
hypotheses tried per burst and the FCS arbitrating. Weak bursts are also
re-decoded at shifted timing windows (the stride-2 hunt lands a sample
or two off; the fractional refine only spans ±0.5). Confirmed FCS-valid
bursts are reconstructed and subtracted (successive interference
cancellation) and the residual re-hunted.

The wide hypothesis fan-out makes random FCS-16 passes a real rate, so
rescue-decoded frames require a sane message type (1-27) and a confirmed
source MMSI (already seen, or a second held frame from the same MMSI:
the Mode S two-sighting policy transplanted). Result: zero false
decodes, 48 a clean subset of AIS-catcher's 53.

Residual gap: 5 payloads that anchor but never produce an FCS-valid
frame under any tested hypothesis, the deepest fades on this capture.

Standing falsification (do not retry without a stronger prior): soft-bit
list repair manufactures false frames under the weak FCS-16. A
max-log soft-output trellis with a Chase-style search flipping the K
least-reliable bits recovered none of the 5 genuine misses and instead
forged a valid-FCS frame from a foreign MMSI; it even subverted the
two-sighting MMSI guard (the repair emits several FCS-valid variants of
one burst, and two variants sharing the forged MMSI "confirm" each
other). FCS-16 is too weak to gate a search that large. At the noise
floor, sensitivity is a capture problem, not a code problem.

Reproduce:

```
xng decode ais_6m.cs16 -f cs16 -m ais -r 6000000 -c 162000000 --channels 161.975,162.025
AIS-catcher -r CS16 ais_6m.cs16 -s 6000000 -n
```

## Iridium

Capture: a 300 s off-air capture (Airspy R2, 1622 MHz, 10 MS/s, KSMF).
xng decodes 758 CRC-OK IDA frames vs gr-iridium's 573 (iridium-extractor
+ iridium-parser.py on the same file; total IDA 1577 vs 1214, ~48% pass-
rate on both, 587 distinct-content). The whole gap was IDA-frame
production, not CRC quality: a gr-style FFT detector, squared-FFT fine
CFO, multi-frame-per-burst decode, and a gr peak-relative end-of-frame
rule (trim only after 3 consecutive symbols below peak/8, instead of
breaking on the first payload symbol below an absolute noise×4 threshold
that was truncating weak frames early).

Not CI-count-gated (the capture is 11 GB, too large to vendor); fenced
instead by bit-exact and field-exact oracle tests. Full campaign in
[IRIDIUM.md](IRIDIUM.md). STD-C and Aero are oracle-validated field-exact
with no count-style comparison yet.

## Radiosonde (RS41)

Capture: `radiosonde_auto_rx` decoder-performance sample `rs41_96k_float.bin`
(serial N3920808, Adelaide AU, 2019), 96 kS/s cf32, 120 s (`bench/data/sonde_96k.cf32`,
release asset). xng `-m sonde` decodes **119 frames / 119 CRC-OK**; the rs1729/RS
`rs41mod` reference (built from source) decodes **119** on the same file —
**exact parity**, serial + GPS frame-by-frame identical (sub-meter). CI floor 110.
First real off-air IQ for this mode (was synthetic + byte-oracle only).

## NAVTEX

Capture: SDRplay's official `navtex.zip` IQ demo (`bench/data/navtex_62500.cs16`,
release asset), 62.5 kS/s cs16, center 516 kHz, NAVTEX at 518 kHz. xng `-m navtex`
decodes the real US Coast Guard message **character-identical** to the
fldigi-derived CCIR-476/FEC-B oracle (and the YaND output in the bundled
screenshot). This exercises the narrow-passband DDC fix — at 62.5 kS/s the old
final-stage filter buried the ±85 Hz FSK (0 frames); the fix recovers 29 frames.
CI floor 25.

## UAT (978 MHz)

No public UAT IQ exists (the canonical dump978 dataset is bits, not IQ), so this
is validated on a **live** capture: tuner on the 1090 antenna at 978 MHz for 50 s
→ **879 CRC-OK frames**, real GA aircraft (callsign/ICAO/position/track/altitude,
e.g. N402AA, N316ME). Not CI-gated (live capture, not vendored).

## Synthetic demod validation (round 5/6)

These are **not** off-air benchmark runs and carry no real-IQ frame counts.
Each is a genuine modulate → complex-AWGN → demod noise/BER test (an
explicitly-allowed synthetic oracle, not a noiseless loopback): a waveform
is built with the crate's own `modulate`, complex Gaussian noise is added at
a controlled SNR, and the same decoder front end recovers it. They quantify
a sensitivity gain; they do not establish real-RF performance (STD-C and
Aero still lack vendorable off-air captures, see the Iridium section).

- **STD-C RRC matched filter.** The BPSK receive path now applies the
  receive-half RRC matched filter (TX RRC + RX RRC = a raised-cosine Nyquist
  pulse); it is on by default (`StdcChannelDecoder::new`), with a
  `with_matched_filter(false)` switch kept only for the test. The
  `matched_filter_recovers_at_lower_snr` test (xng-mode-stdc) sweeps the
  noise sigma into the marginal-SNR cliff and counts frames recovered with
  the matched filter ON vs OFF: ON never recovers fewer frames at any SNR
  and recovers materially more near the cliff (measured net ≈ +66/180 frames
  over the 10-trial × 6-frame × 3-sigma sweep), i.e. frame recovery at
  equal-or-lower SNR.
- **AERO-6 coherent carrier path.** A decision-directed (Costas-style)
  coherent MSK detector (`coherent::CoherentMskDemod`) is added alongside
  the existing non-coherent frequency-discriminator demod; both share the
  same front end and timing loop. It runs as a fallback for marginal bursts
  when the discriminator's packetizer fails to lock. The
  `coherent_beats_discriminator_ber_vs_snr` test (xng-mode-aero) sweeps a
  modulate → AWGN → demod BER curve and shows the coherent path is no worse
  at every point and clearly better (≥20% lower BER) at the mid-range points,
  reaching the discriminator's 8 dB error rate at ~1 dB lower SNR — recovery
  at lower SNR than the non-coherent path.

## Decode CPU (×-realtime, Apple M-series; `bench/cpu.sh`)

| mode | effort | speed |
|---|---|---|
| adsb | `live` (default for SDR) | 16.6× |
| adsb | `max` (default for files) | 5.3× |
| ais | full | 8.6× |
| vdl2 | full | 85× |
| hfdl | full | 283× |

Pi-class hardware runs ~5-8× slower than the bench machine: `live`
effort keeps ADS-B comfortably real-time there; AIS lands ~1.4×. ADS-B
and AIS earn their speed from cached GMSK/PPM waveform tables, a
traceback-matrix Viterbi (no O(n²) path clones), stride-2 template
hunting with low-metric span skipping, and the `--demod-effort` knob
(file decode = max, SDR commands = live).

### ACARS shared multi-channel front end

ACARS runs many narrowband channels from one wideband capture, so the
per-channel downconverter front end (not the bit demod, which is ~17×
cheaper) dominates and scales linearly with channel count. A shared front
end downconverts all channels from one pass; the default is a polyphase
channelizer whose cost is independent of channel count and channel spacing.

×-realtime on an 8-channel synthetic 2.4 MS/s capture (`bench/cpu.sh`,
32-core x86 dev box — *not* the M-series machine above, so not directly
comparable to that table), decoding the front end three ways:

| front end | 8 ch, tight cluster | 16 ch, wide span |
|---|---|---|
| per-channel DDC (old) | 3.7× | 1.9× |
| shared decimation (`SharedDdc`) | 7.7× | 2.5× |
| polyphase channelizer (`ChannelizedDdc`, default) | 27× | 18× |

The channelizer is span-independent (the shared-decimation win shrinks as
channels spread, because its coarse stage can only decimate as far as the
widest channel allows). Both front ends produce byte-identical decodes (a
`cargo test` asserts it); the choice is purely CPU. The same `xng-dsp`
front ends are intended for VDL2/AIS/Aero/STD-C next (all use the same
per-channel offset DDC today).

## Live-capture authenticity: phantom frames and ICAO confirmation

Dense benchmark captures hide a defect that quiet live RF exposes: with
near-floor candidate gates and many sub-sample CRC trials, a random
112-bit Mode S candidate passes the 24-bit parity with probability 2⁻²⁴,
so a minute of pure noise yields ~140 expected false DF17s (a quiet
60 s 1090 capture produced 70 phantoms where both oracles reported ~0).
Worse, false DF11s (17 parity bits effectively checked) learn junk
ICAOs into the cache, which then validate junk address-overlaid
DF0/4/5 frames. The 0.18 s modes1 fixture expects ~0.4 phantoms, which
is why the count benchmarks never surface this.

Safeguard (load-bearing, the policy readsb uses for unreliable sources):
two-sighting ICAO confirmation. A CRC-clean DF17/18/11 whose address has
never been seen is held, not emitted; a second clean frame with the same
address confirms the aircraft, releases the held frame at its original
position, and admits the ICAO to the cache. Random phantoms never repeat
an address (P ≈ 2⁻²⁴ per pair), so they die in the pending table (capped
at 64, age-evicted). Address-overlaid frames already required a cached
ICAO, so they inherit confirmed-only trust. The staleness clock ticks on
sightings, not candidate attempts.

Cost: zero on modes1 (a single heavily-repeated aircraft, the gate still
reads its floor). Benefit: the quiet live capture drops from 70 phantoms
to exactly 0, matching both oracles. CI fences this with a ceiling gate:
`adsb_quiet_max = 4` (measured 1) on the first 20 s of the quiet capture,
so any future relaxation of the candidate gates that revives the phantoms
fails the bench job.
