#!/usr/bin/env python3
"""Plot bench/dme results: pair detection and ident against SNR, and CPU.

  bench/dme/plot.py <results.json> <out.png>

OnAir only reports a pair rate, not pair times, so its curve is pairs
counted per pair sent (false pairs included) rather than pairs matched.
"""
import json
import sys

import matplotlib

matplotlib.use("Agg")
import matplotlib.pyplot as plt  # noqa: E402

res = json.load(open(sys.argv[1]))
rows = res["snr"]
snr = [r["snr_db"] for r in rows]
onair = "onair_count_ratio" in rows[0]
XNG, GR, OA = "#2a7de1", "#9a9a9a", "#e8862a"

fig, (a, c, b) = plt.subplots(1, 3, figsize=(14, 4.4), gridspec_kw={"width_ratios": [2.0, 1.4, 1]})

a.plot(snr, [100 * r["xng_pd"] for r in rows], "o-", color=XNG, lw=2, label="xng-mode-dme")
a.plot(snr, [100 * r["gr_pd"] for r in rows], "s-", color=GR, lw=2, label="gr-dme (example settings)")
if onair:
    a.plot(snr, [100 * min(r["onair_count_ratio"], 1.2) for r in rows], "^--", color=OA, lw=2,
           label="OnAir (pairs counted, not matched)")
a.set_xlabel("Peak pulse SNR in 1 MHz (dB)")
a.set_ylabel("Pulse pairs found (%)")
a.set_ylim(-3, 105)
a.grid(alpha=0.3)
a.legend(loc="upper left", frameon=False, fontsize=8)
a.set_title("Pulse pairs, same synthetic signals", fontsize=11)

c.plot(snr, [100 * r["xng_ident_ok"] / r["runs"] for r in rows], "o-", color=XNG, lw=2, label="xng-mode-dme")
if onair:
    c.plot(snr, [100 * r["onair_ident_ok"] / r["runs"] for r in rows], "^--", color=OA, lw=2, label="OnAir")
c.text(0.98, 0.5, "gr-dme has no\nident decoder", transform=c.transAxes, ha="right", va="center",
       fontsize=8, color=GR)
c.set_xlabel("Peak pulse SNR in 1 MHz (dB)")
c.set_ylabel("Runs with the ident read correctly (%)")
c.set_ylim(-5, 105)
c.grid(alpha=0.3)
c.legend(loc="center left", frameon=False, fontsize=8)
c.set_title('Morse ident "SFO"', fontsize=11)

cpu = res["cpu"]
names, vals, cols = ["xng", "gr-dme"], [cpu["xng_cpu_s"], cpu["gr_cpu_s"]], [XNG, GR]
if "onair_cpu_s" in cpu:
    names.append("OnAir*")
    vals.append(cpu["onair_cpu_s"])
    cols.append(OA)
bars = b.bar(names, vals, color=cols)
for bar, v in zip(bars, vals):
    b.text(bar.get_x() + bar.get_width() / 2, v, f"{v:.2f} s", ha="center", va="bottom", fontsize=9)
b.set_ylabel("CPU seconds")
b.set_title(f"CPU for {cpu['seconds_of_iq']:.0f} s of IQ at {cpu['sample_rate']/1e6:g} MS/s", fontsize=11)
if "onair_cpu_s" in cpu:
    b.text(0.5, -0.2, "*OnAir's own figure for its front end and decoder,\nwithout file reading",
           transform=b.transAxes, ha="center", fontsize=7, color="#666")
for ax in (a, b, c):
    ax.spines[["top", "right"]].set_visible(False)
fig.tight_layout()
fig.savefig(sys.argv[2], dpi=150)
