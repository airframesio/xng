# Provenance — xng-mode-navaid

Clean-room implementation. The sources below were read for signal facts
(frequencies, modulation depths, tolerances); no code was copied or ported
from any of them.

## Signal definitions

- ICAO Annex 10, Volume I (Radio Navigation Aids), 3.1 (ILS) and 3.3
  (VOR): the 30 Hz variable and reference signals, the 9960 Hz subcarrier
  with ±480 Hz deviation (modulation index 16), the 30 % depths, the
  radial convention (reference phase minus variable phase), the 90/150 Hz
  ILS tones, DDM and SDM definitions (localizer SDM 40 %, glideslope
  80 %), the 0.155 and 0.175 DDM full-scale deflections (150 µA), the
  1020 Hz ident tone at about 7 words per minute, and the two-frequency
  course/clearance systems.
- The VOR/ILS/DME channel pairing (ILS localizer to glideslope pairs, the
  50 kHz VOR raster, DME X/Y channels) follows the Annex 10 channel tables.

## Receiver design

Textbook receiver design throughout: FFT carrier acquisition, a
second-order PLL, coherent AM detection, single-bin DFTs for the 30/90/150
Hz tones, a maximum-likelihood fold-and-correlate estimator for the FM
reference phase, a comb search over the FM line spectrum (Bessel sidebands
30 Hz apart) for the subcarrier frequency, and Morse decoding with epoch
folding of the repeated ident.

## Benchmarks

`bench/navaid/` builds vortrack, voracious and dumpils from their public
repositories into `target/navaid-bench/` and runs them as black boxes on
the same synthetic IQ. None of their code is vendored or linked into xng.
`bench/navaid/rtlstub.c` and `bench/navaid/dumpils-file/main.go` are our
own file-replay harnesses for the two decoders that only read live SDRs.
