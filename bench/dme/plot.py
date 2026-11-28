#!/usr/bin/env python3
"""Plot bench/dme results: pair detection rate against SNR, and CPU time.

  bench/dme/plot.py <results.json> <out.png>
"""
import json
import sys

import matplotlib

matplotlib.use("Agg")
import matplotlib.pyplot as plt  # noqa: E402

res = json.load(open(sys.argv[1]))
rows = res["snr"]
snr = [r["snr_db"] for r in rows]
XNG, GR = "#2a7de1", "#9a9a9a"

fig, (a, b) = plt.subplots(1, 2, figsize=(11, 4.2), gridspec_kw={"width_ratios": [2.2, 1]})
a.plot(snr, [100 * r["xng_pd"] for r in rows], "o-", color=XNG, lw=2, label="xng-mode-dme")
a.plot(snr, [100 * r["gr_pd"] for r in rows], "s-", color=GR, lw=2, label="gr-dme (example settings)")
for r in rows:
    if r["xng_ident_ok"] == r["runs"]:
        a.annotate("ident", (r["snr_db"], 100 * r["xng_pd"]), textcoords="offset points", xytext=(0, 8),
                   ha="center", fontsize=7, color=XNG)
a.set_xlabel("Peak pulse SNR in 1 MHz (dB)")
a.set_ylabel("Pulse pairs detected (%)")
a.set_ylim(-3, 103)
a.grid(alpha=0.3)
a.legend(loc="lower right", frameon=False)
a.set_title("Pulse-pair detection, same synthetic signals", fontsize=11)

cpu = res["cpu"]
names = ["xng", "gr-dme"]
vals = [cpu["xng_cpu_s"], cpu["gr_cpu_s"]]
bars = b.bar(names, vals, color=[XNG, GR])
for bar, v in zip(bars, vals):
    b.text(bar.get_x() + bar.get_width() / 2, v, f"{v:.2f} s", ha="center", va="bottom", fontsize=9)
b.set_ylabel("CPU seconds")
b.set_title(f"CPU for {cpu['seconds_of_iq']:.0f} s of IQ at {cpu['sample_rate']/1e6:g} MS/s", fontsize=11)
b.spines[["top", "right"]].set_visible(False)
a.spines[["top", "right"]].set_visible(False)
fig.tight_layout()
fig.savefig(sys.argv[2], dpi=150)
