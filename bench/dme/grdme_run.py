#!/usr/bin/env python3
"""Run gr-dme (https://github.com/daniestevez/gr-dme, GPL-3.0) as a black box.

Builds the receiver chain of gr-dme's own examples/dme_receiver.grc (Gaussian
matched filter, pulse detector, 12 us double-pulse coalesce, polyphase fine
estimate, tag file sink) with that example's parameters, on a cf32 file.
Nothing from gr-dme is copied into xng; this script only calls the installed
module. Usage:

  grdme_run.py <in.cf32> <samp_rate> <out_pairs_us.txt> [threshold] [spacing_us]

Prints the CPU time of the flowgraph run.
"""
import os
import sys
import resource
import time

import numpy as np
from gnuradio import blocks, filter, gr
from gnuradio import dme


def main():
    src, samp_rate, out = sys.argv[1], float(sys.argv[2]), sys.argv[3]
    threshold = float(sys.argv[4]) if len(sys.argv) > 4 else 10.0
    pulse_spacing = float(sys.argv[5]) * 1e-6 if len(sys.argv) > 5 else 12e-6

    # Parameters as in gr-dme examples/dme_receiver.grc.
    npolyphase = 128
    pulse_ntaps = 27
    pulse_width = 3.5e-6
    pulse_sigma = pulse_width / (2 * np.sqrt(2 * np.log(2)))
    pulse_spacing_tolerance = 0.5e-6
    k = np.arange(pulse_ntaps)
    pulse_taps_u = np.exp(-(k - pulse_ntaps // 2) ** 2 / (2 * (pulse_sigma * samp_rate) ** 2))
    pulse_taps = pulse_taps_u * np.max(pulse_taps_u) / np.sum(pulse_taps_u ** 2)
    kp = np.arange(pulse_ntaps * npolyphase)
    polyphase_pulse = np.exp(-(kp - pulse_ntaps * npolyphase // 2) ** 2
                             / (2 * (pulse_sigma * samp_rate * npolyphase) ** 2))
    pulse_spacing_polysamples = round(pulse_spacing * samp_rate * npolyphase)
    polyphase_double_pulse = np.concatenate((polyphase_pulse, np.zeros(pulse_spacing_polysamples)))
    polyphase_double_pulse[pulse_spacing_polysamples:pulse_spacing_polysamples + len(polyphase_pulse)] += polyphase_pulse
    polyphase_taps = polyphase_double_pulse * np.max(polyphase_double_pulse) / np.sum(polyphase_double_pulse ** 2)

    offsets = out + ".f64"
    mf = out + ".mf.c64"
    tb = gr.top_block()
    s = blocks.file_source(gr.sizeof_gr_complex, src, False)
    fir = filter.fir_filter_ccf(1, pulse_taps.astype(np.float32).tolist())
    mag2 = blocks.complex_to_mag_squared(1)
    delay = blocks.delay(gr.sizeof_gr_complex, pulse_ntaps // 2)
    det = dme.pulse_detector(pulse_ntaps, threshold, 1e-5)
    coal = dme.double_pulse_coalesce(int((pulse_spacing - pulse_spacing_tolerance) * samp_rate),
                                     int(np.ceil((pulse_spacing + pulse_spacing_tolerance) * samp_rate)),
                                     round(0.5 * pulse_spacing * samp_rate))
    fine = dme.polyphase_fine_estimate(polyphase_taps.astype(np.float32).tolist(), npolyphase,
                                       int(2 * pulse_width * samp_rate))
    sink = dme.tag_file_sink(offsets, mf)
    tb.connect(s, fir, mag2)
    tb.connect((mag2, 0), (det, 1))
    tb.connect(s, delay, (det, 0))
    tb.connect(det, coal, fine, sink)

    r0 = resource.getrusage(resource.RUSAGE_SELF)
    w0 = time.time()
    tb.run()
    w1 = time.time()
    r1 = resource.getrusage(resource.RUSAGE_SELF)
    cpu = (r1.ru_utime - r0.ru_utime) + (r1.ru_stime - r0.ru_stime)

    off = np.fromfile(offsets, dtype=np.float64) if os.path.exists(offsets) else np.zeros(0)
    np.savetxt(out, off / samp_rate * 1e6, fmt="%.3f")
    n = os.path.getsize(src) // 8
    print(f"PAIRS {len(off)}")
    print(f"CPU {cpu:.4f} WALL {w1 - w0:.4f} SECONDS {n / samp_rate:.3f}")
    for f in (offsets, mf):
        if os.path.exists(f):
            os.remove(f)


if __name__ == "__main__":
    main()
