#!/usr/bin/env python3
"""DME benchmark: xng-mode-dme against gr-dme on identical synthetic signals.

Needs: `cargo build --release -p xng-mode-dme --example dme_bench`, and
gr-dme installed for the python that GRPY names (see README.md).

  bench/dme/run.py <workdir> [--quick]

Writes <workdir>/results.json and prints a Markdown summary.
"""
import json
import os
import subprocess
import sys

import numpy as np

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
XNG = os.path.join(ROOT, "target", "release", "examples", "dme_bench")
GRPY = os.environ.get("GRPY", "/usr/bin/python3.12")
GRRUN = os.path.join(os.path.dirname(os.path.abspath(__file__)), "grdme_run.py")
RATE = 2_500_000  # gr-dme's example rate
CARRIER = "1151000000"  # 64X reply: X spacing only
TOL_US = 1.5


def sh(cmd):
    return subprocess.run(cmd, check=True, capture_output=True, text=True).stdout


def parse(out):
    r = {"idents": []}
    for line in out.splitlines():
        f = line.split()
        if not f:
            continue
        if f[0] == "CPU":
            r["cpu"], r["wall"], r["seconds"] = float(f[1]), float(f[3]), float(f[5])
        elif f[0] == "IDENT":
            r["idents"].append(f[1])
    return r


def match(truth, det, offset, tol=TOL_US):
    """Greedy one-to-one match of detections to transmitted pairs."""
    det = np.sort(np.asarray(det, dtype=float) - offset)
    used = np.zeros(len(truth), dtype=bool)
    tp = 0
    for x in det:
        i = np.searchsorted(truth, x - tol)
        while i < len(truth) and truth[i] <= x + tol:
            if not used[i]:
                used[i] = True
                tp += 1
                break
            i += 1
    return tp, len(det) - tp


def median_offset(truth, det):
    det = np.asarray(det, dtype=float)
    i = np.clip(np.searchsorted(truth, det), 1, len(truth) - 1)
    d = np.where(abs(det - truth[i]) < abs(det - truth[i - 1]), det - truth[i], det - truth[i - 1])
    return float(np.median(d))


def load(p):
    try:
        return np.atleast_1d(np.loadtxt(p))
    except (ValueError, OSError):
        return np.zeros(0)


def case(wd, name, snr, secs=5.0, ident="SFO", offset_hz=0, shape="gauss", seed=1, gr_threshold=10.0):
    iq = os.path.join(wd, f"{name}.cf32")
    tr = os.path.join(wd, f"{name}.truth")
    sh([XNG, "sim", iq, tr, str(RATE), str(secs), str(snr), ident, str(offset_hz), shape, str(seed)])
    truth = np.loadtxt(tr, delimiter=",")[:, 0]
    xp = os.path.join(wd, f"{name}.xng")
    gp = os.path.join(wd, f"{name}.gr")
    x = parse(sh([XNG, "run", iq, str(RATE), xp, CARRIER]))
    g = parse(sh([GRPY, GRRUN, iq, str(RATE), gp, str(gr_threshold)]))
    os.remove(iq)
    return truth, load(xp), x, load(gp), g


def main():
    wd = sys.argv[1]
    quick = "--quick" in sys.argv
    os.makedirs(wd, exist_ok=True)

    # Calibrate each decoder's constant timing offset at high SNR.
    truth, xd, _, gd, _ = case(wd, "cal", 25, secs=2.0, ident="-")
    xoff, goff = median_offset(truth, xd), median_offset(truth, gd)

    snrs = [3, 4, 5, 6, 7, 8, 9, 10, 12, 15, 20] if not quick else [4, 6, 8, 12]
    seeds = [1, 2, 3] if not quick else [1]
    rows = []
    for snr in snrs:
        acc = {"snr_db": snr, "pairs": 0, "xng_tp": 0, "xng_fp": 0, "gr_tp": 0, "gr_fp": 0,
               "xng_ident_ok": 0, "runs": 0, "seconds": 0.0}
        for seed in seeds:
            truth, xd, x, gd, g = case(wd, f"snr{snr}_{seed}", snr, seed=seed)
            a, b = match(truth, xd, xoff)
            c, d = match(truth, gd, goff)
            acc["pairs"] += len(truth)
            acc["xng_tp"] += a
            acc["xng_fp"] += b
            acc["gr_tp"] += c
            acc["gr_fp"] += d
            acc["xng_ident_ok"] += int("SFO" in x["idents"])
            acc["runs"] += 1
            acc["seconds"] += x["seconds"]
        acc["xng_pd"] = acc["xng_tp"] / acc["pairs"]
        acc["gr_pd"] = acc["gr_tp"] / acc["pairs"]
        acc["xng_fa_per_s"] = acc["xng_fp"] / acc["seconds"]
        acc["gr_fa_per_s"] = acc["gr_fp"] / acc["seconds"]
        rows.append(acc)
        print(f"snr {snr:>3} dB  xng Pd {acc['xng_pd']:.3f} FA/s {acc['xng_fa_per_s']:.1f}  "
              f"gr-dme Pd {acc['gr_pd']:.3f} FA/s {acc['gr_fa_per_s']:.1f}  "
              f"xng ident {acc['xng_ident_ok']}/{acc['runs']}", flush=True)

    # gr-dme threshold sweep at one SNR (is there a gr-dme setting that wins?).
    sweep = []
    for th in ([4, 5, 6, 7, 8, 10] if not quick else [5, 10]):
        truth, xd, x, gd, g = case(wd, f"sweep{th}", 6, gr_threshold=th, seed=7)
        c, d = match(truth, gd, goff)
        a, b = match(truth, xd, xoff)
        sweep.append({"gr_threshold": th, "gr_pd": c / len(truth), "gr_fa_per_s": d / g["seconds"],
                      "xng_pd": a / len(truth), "xng_fa_per_s": b / x["seconds"]})
        print(f"sweep th {th}: gr-dme Pd {c/len(truth):.3f} FA/s {d/g['seconds']:.1f}  "
              f"xng Pd {a/len(truth):.3f} FA/s {b/x['seconds']:.1f}", flush=True)

    # Robustness: carrier offset and a cos² pulse (not the Gaussian both
    # matched filters assume).
    robust = []
    for label, kw in [("offset 20 kHz", {"offset_hz": 20000}), ("offset 60 kHz", {"offset_hz": 60000}),
                      ("cos² pulse", {"shape": "cos2"})]:
        truth, xd, x, gd, g = case(wd, "rob", 8, **kw, seed=11)
        a, b = match(truth, xd, xoff)
        c, d = match(truth, gd, goff)
        robust.append({"case": label, "snr_db": 8, "xng_pd": a / len(truth), "gr_pd": c / len(truth),
                       "xng_fa_per_s": b / x["seconds"], "gr_fa_per_s": d / g["seconds"],
                       "xng_ident": "SFO" in x["idents"]})
        print(f"{label}: xng Pd {a/len(truth):.3f} gr-dme Pd {c/len(truth):.3f}", flush=True)

    # CPU: one long file, both decoders, three runs each, best of three.
    secs = 20.0 if not quick else 5.0
    iq = os.path.join(wd, "cpu.cf32")
    sh([XNG, "sim", iq, os.path.join(wd, "cpu.truth"), str(RATE), str(secs), "12", "SFO", "0", "gauss", "5"])
    xc = min(parse(sh([XNG, "run", iq, str(RATE), os.path.join(wd, "cpu.xng"), CARRIER]))["cpu"] for _ in range(3))
    gc = min(parse(sh([GRPY, GRRUN, iq, str(RATE), os.path.join(wd, "cpu.gr")]))["cpu"] for _ in range(3))
    os.remove(iq)
    cpu = {"seconds_of_iq": secs, "sample_rate": RATE, "xng_cpu_s": xc, "gr_cpu_s": gc,
           "xng_realtime_x": secs / xc, "gr_realtime_x": secs / gc}
    print(f"CPU for {secs:.0f} s at {RATE/1e6} MS/s: xng {xc:.2f} s, gr-dme {gc:.2f} s", flush=True)

    res = {"offsets_us": {"xng": xoff, "gr": goff}, "snr": rows, "sweep": sweep, "robust": robust, "cpu": cpu}
    with open(os.path.join(wd, "results.json"), "w") as f:
        json.dump(res, f, indent=2)


if __name__ == "__main__":
    main()
