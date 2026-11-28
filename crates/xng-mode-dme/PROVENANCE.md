# Provenance: xng-mode-dme

Clean-room passive DME receiver. No decoder code was copied or ported. The
signal facts come from ICAO Annex 10 Volume I §3.5 (DME/N): channel plan and
VHF pairing (Table A), pulse shape (3.5 µs half-amplitude width), pulse
spacings (X 12/12 µs, Y 36/30 µs), reply delays (50/56 µs), ident keying
(International Morse, 1350 pulse pairs per second, dots 0.1–0.16 s, dashes
0.3–0.48 s, at least every 40 s) and, for TACAN, the 15 Hz main reference
burst (12 pairs 30 µs apart, X mode). Facts are not copyrightable; the
implementation, the synthetic signal generator and the tests are original.

## gr-dme

[gr-dme](https://github.com/daniestevez/gr-dme) (Daniel Estévez, GPL-3.0) is
the benchmark reference only. Its headers and the parameters of its example
flowgraph were read to run it correctly; `bench/dme/grdme_run.py` builds that
flowgraph from gr-dme's installed Python module and treats it as a black box.
No gr-dme source is in this repository, and xng's detector differs in design
(energy of both pulses added before one decision, median noise floor, balance
gate, peak picking over the whole pair window, and an ident detector).

## Verification posture: synthetic, no off-air capture yet

There is no DME capture in the corpus. `modulate.rs` synthesizes a
transponder (Poisson squitter/replies with a 60 µs dead time, a Morse ident
at 1350 pp/s, TACAN reference bursts, Gaussian or cos² pulses, carrier
offset, AWGN) and returns the true pair times. `tests/loopback.rs` runs that
IQ through the real `DmeChannelDecoder` (DDC, detector, ident, status) and
checks detection rate, false pairs, the decoded ident, the channel naming and
the pairing table. `src/channels.rs` tests the Annex 10 frequency table
against published entries (1X 962 MHz, 64X 1151 MHz, 17X ↔ 108.00 MHz,
126Y ↔ 117.95 MHz, …).

The first off-air capture (any DME reply frequency, ideally one with a known
ident) should go to `xng-corpus` under `iq/dme/`, with the ident and channel
as ground truth.
