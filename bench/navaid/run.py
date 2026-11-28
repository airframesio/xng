#!/usr/bin/env python3
"""Navaid decoder benchmark: xng-mode-navaid against voracious, vortrack and dumpils.

Every decoder gets byte-identical synthetic captures (RTL-SDR cu8), written by
`navaid_gen` from the Annex 10 signal model, at the sample rate and channel
offset its file front end expects. Decoders run as black boxes; only their
printed output is scored.

    bench/navaid/build-competitors.sh            # once: builds the three tools
    python3 bench/navaid/run.py [--quick] [--out docs/notes/navaid-bench.json]

Tests:
  vor_radial   radial error and availability vs C/N0 (2 s outputs)
  vor_impair   carrier error, subcarrier error, voice, Doppler VOR asymmetry
  vor_ident    Morse ident correctly decoded vs C/N0 (60 s)
  loc_ddm      localizer DDM error vs C/N0
  loc_twofreq  two-frequency localizer (course + clearance carrier)
  gs_ddm       glideslope DDM error vs C/N0
  loc_ident    localizer ident vs C/N0
CPU is user+system time per second of signal, measured per decoder run.
"""

import argparse, cmath, json, math, os, resource, subprocess, sys, tempfile, threading
from concurrent.futures import ThreadPoolExecutor

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
EX = os.path.join(ROOT, "target", "release", "examples")
COMP = os.environ.get("NAVAID_BENCH_TOOLS", os.path.join(ROOT, "target", "navaid-bench"))
VORACIOUS = os.path.join(COMP, "voracious")
VORTRACK = os.path.join(COMP, "vortrack-file")
DUMPILS = os.path.join(COMP, "dumpils-file")

VOR_MHZ = 113.0
LOC_MHZ = 110.1
GS_MHZ = 334.4
WARMUP_S = 6.0


def gen(path, args):
    subprocess.run([os.path.join(EX, "navaid_gen"), path] + [str(a) for a in args], check=True)


def timed(cmd, env=None, cwd=None):
    """Run cmd, return (stdout, CPU seconds of that child alone)."""
    cmd = [str(c) for c in cmd]
    with tempfile.TemporaryFile() as fh:
        p = subprocess.Popen(cmd, stdout=fh, stderr=subprocess.DEVNULL, env=env, cwd=cwd)
        _, status, ru = os.wait4(p.pid, 0)
        p.returncode = os.waitstatus_to_exitcode(status)
        if p.returncode != 0:
            # A crashed or failed run must not be scored as a decoder result.
            raise RuntimeError(f"{' '.join(cmd)} exited with {p.returncode}")
        fh.seek(0)
        out = fh.read().decode(errors="replace")
    return out, ru.ru_utime + ru.ru_stime


# ---------------------------------------------------------------- decoders

def run_xng(path, kind, rate, offset, report=2.0):
    out, cpu = timed([os.path.join(EX, "navaid_replay"), path, "--rate", rate, "--offset", offset,
                      "--kind", kind, "--report", report])
    rows = []
    for line in out.splitlines():
        o = json.loads(line)
        r = {"t": o["t_s"], "ident": o.get("ident"), "ident_last": o.get("ident_last")}
        if o.get("vor"):
            r["radial"] = o["vor"].get("radial_deg")
        if o.get("ils"):
            r["ddm"] = o["ils"][0]["ddm"]
        rows.append(r)
    return rows, cpu


def run_vortrack(path, interval=2):
    out, cpu = timed([VORTRACK, "-l", str(interval), str(VOR_MHZ)], env={**os.environ, "NAV_IQ": path})
    rows = []
    for i, line in enumerate(out.split()):
        try:
            rows.append({"t": (i + 1) * interval, "radial": float(line)})
        except ValueError:
            pass
    return rows, cpu


def run_voracious(path, kind, rate, offset, window=2.0):
    mhz = VOR_MHZ if kind == "vor" else LOC_MHZ
    flag = "-v" if kind == "vor" else "--ils-freq"
    center = mhz - offset / 1e6
    out, cpu = timed([VORACIOUS, path, "-s", str(int(rate)), "-f", "cu8", flag, str(mhz), "-c", f"{center:.6f}",
                      "-w", str(window)])
    rows = []
    for i, line in enumerate(out.splitlines()):
        try:
            o = json.loads(line)
        except ValueError:
            continue
        r = {"t": (i + 1) * window, "ident": o.get("ident")}
        if "radial_deg" in o:
            r["radial"] = o["radial_deg"]
        if "mod_90_pct" in o:
            # voracious reports (env90-env150)/(env90+env150); the ICAO DDM
            # is the difference of the depths, which it also prints.
            r["ddm"] = (o["mod_90_pct"] - o["mod_150_pct"]) / 100.0
        rows.append(r)
    return rows, cpu


def run_dumpils(path, gs=False):
    out, cpu = timed([DUMPILS, path, "gs" if gs else "loc", "200000"])
    rows = []
    fs = 150.0 / (17.5 if gs else 15.5)
    for i, line in enumerate(out.splitlines()[1:]):
        p = line.split(";")
        try:
            ua = float(p[1])
        except (ValueError, IndexError):
            continue
        # dumpils prints µA with 150 Hz dominance positive; convert to the
        # ICAO m90 - m150 fraction.
        rows.append({"t": (i + 1) * 0.1, "ddm": -ua / fs / 100.0})
    # Average to 2 s like the others (dumpils itself reports every 0.1 s).
    out2 = []
    for k in range(0, len(rows) - 19, 20):
        blk = rows[k:k + 20]
        out2.append({"t": blk[-1]["t"], "ddm": sum(b["ddm"] for b in blk) / len(blk)})
    return out2, cpu


# ---------------------------------------------------------------- scoring

def ang_err(a, b):
    return (a - b + 180.0) % 360.0 - 180.0


def score_radial(rows, truth, duration):
    rows = [r for r in rows if r["t"] > WARMUP_S and r.get("radial") is not None and math.isfinite(r["radial"])]
    # 2 s reports after the warmup; the last one may fall just past the end
    # of the file (filter delay), so it is not expected.
    expected = max(1, int((duration - WARMUP_S) / 2.0) - 1)
    if not rows:
        return {"rms_deg": None, "within_2deg": 0.0, "availability": 0.0}
    e = [ang_err(r["radial"], truth) for r in rows]
    return {
        "rms_deg": math.sqrt(sum(x * x for x in e) / len(e)),
        "within_2deg": min(1.0, sum(abs(x) <= 2.0 for x in e) / expected),
        "availability": min(1.0, len(rows) / expected),
    }


def score_ddm(rows, truth, duration):
    rows = [r for r in rows if r["t"] > WARMUP_S and r.get("ddm") is not None and math.isfinite(r["ddm"])]
    # 2 s reports after the warmup; the last one may fall just past the end
    # of the file (filter delay), so it is not expected.
    expected = max(1, int((duration - WARMUP_S) / 2.0) - 1)
    if not rows:
        return {"rms_ddm": None, "within_0.005": 0.0, "availability": 0.0}
    e = [r["ddm"] - truth for r in rows]
    return {
        "rms_ddm": math.sqrt(sum(x * x for x in e) / len(e)),
        "within_0.005": min(1.0, sum(abs(x) <= 0.005 for x in e) / expected),
        "availability": min(1.0, len(rows) / expected),
    }


def score_ident(rows, truth):
    """Correct: the decoder's final reported ident matches. Wrong: any
    reported ident that differs (a false ident is worse than none)."""
    ids = [r.get("ident") for r in rows if r.get("ident")]
    return {"correct": bool(ids) and ids[-1] == truth, "wrong_reports": sum(1 for i in ids if i != truth)}


def mean(xs):
    xs = [x for x in xs if x is not None]
    return sum(xs) / len(xs) if xs else None


# ---------------------------------------------------------------- tests

class Bench:
    def __init__(self, quick, tmp):
        self.quick = quick
        self.tmp = tmp
        self.cpu = {}
        self.n = 0
        self.lock = threading.Lock()

    def file(self):
        with self.lock:
            self.n += 1
            n = self.n
        return os.path.join(self.tmp, f"c{n}.cu8")

    def cpu_add(self, name, cpu, seconds):
        with self.lock:
            c = self.cpu.setdefault(name, [0.0, 0.0])
            c[0] += cpu
            c[1] += seconds

    def vor_case(self, cn0, radial, seconds, seed, extra=(), ident=None, decoders=("xng", "voracious", "vortrack")):
        f = self.file()
        args = ["--kind", "vor", "--rate", 2_000_000, "--offset", 50_000, "--seconds", seconds,
                "--cn0", cn0, "--radial", radial, "--seed", seed] + list(extra)
        if ident:
            args += ["--ident", ident, "--ident-start", 2.0 + seed % 5]
        gen(f, args)
        res = {}
        for d in decoders:
            if d == "xng":
                rows, cpu = run_xng(f, "vor", 2_000_000, 50_000)
            elif d == "voracious":
                rows, cpu = run_voracious(f, "vor", 2_000_000, 50_000)
            else:
                rows, cpu = run_vortrack(f)
            self.cpu_add(d + ":vor", cpu, seconds)
            res[d] = rows
        os.remove(f)
        return res

    def ils_case(self, kind, cn0, ddm, seconds, seed, extra=(), ident=None, decoders=("xng", "voracious", "dumpils")):
        res = {}
        base = ["--kind", kind, "--seconds", seconds, "--cn0", cn0, "--ddm", ddm, "--seed", seed] + list(extra)
        if ident:
            base += ["--ident", ident, "--ident-start", 2.0 + seed % 5]
        # 2 MS/s capture at +50 kHz for xng and voracious.
        if any(d in decoders for d in ("xng", "voracious")):
            f = self.file()
            gen(f, base + ["--rate", 2_000_000, "--offset", 50_000])
            if "xng" in decoders:
                rows, cpu = run_xng(f, "loc" if kind == "loc" else "gs", 2_000_000, 50_000)
                self.cpu_add("xng:" + kind, cpu, seconds)
                res["xng"] = rows
            if "voracious" in decoders and kind == "loc":
                rows, cpu = run_voracious(f, "loc", 2_000_000, 50_000)
                self.cpu_add("voracious:loc", cpu, seconds)
                res["voracious"] = rows
            os.remove(f)
        # dumpils' fixed front end: 1310720 S/s, channel at +200 kHz.
        if "dumpils" in decoders:
            f = self.file()
            gen(f, base + ["--rate", 1_310_720, "--offset", 200_000])
            rows, cpu = run_dumpils(f, gs=(kind == "gs"))
            self.cpu_add("dumpils:" + kind, cpu, seconds)
            res["dumpils"] = rows
            os.remove(f)
        return res

    def sweep(self, cn0s, cases, fn, scorer, workers=4):
        out = {}
        jobs = [(c, case) for c in cn0s for case in cases]
        with ThreadPoolExecutor(workers) as ex:
            results = list(ex.map(lambda j: (j, fn(*j)), jobs))
        for (cn0, case), res in results:
            for d, rows in res.items():
                out.setdefault(d, {}).setdefault(cn0, []).append(scorer(rows, case))
        return out


def summarize(per):
    """per[decoder][cn0] = [score dicts] → per[decoder][cn0] = mean dict."""
    s = {}
    for d, by in per.items():
        s[d] = {}
        for cn0, lst in sorted(by.items()):
            keys = lst[0].keys()
            s[d][cn0] = {k: (mean([x[k] for x in lst]) if not isinstance(lst[0][k], bool)
                             else sum(x[k] for x in lst) / len(lst)) for k in keys}
    return s


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--quick", action="store_true", help="fewer points (CI smoke)")
    ap.add_argument("--out", default=os.path.join(ROOT, "docs", "notes", "navaid-bench.json"))
    ap.add_argument("--only", default="")
    a = ap.parse_args()
    for t in (VORACIOUS, VORTRACK, DUMPILS):
        if not os.path.exists(t):
            sys.exit(f"missing {t}: run bench/navaid/build-competitors.sh")
    subprocess.run(["cargo", "build", "--release", "-q", "-p", "xng-mode-navaid", "--examples"], cwd=ROOT, check=True)
    only = set(a.only.split(",")) if a.only else None
    want = lambda t: only is None or t in only
    tmp = tempfile.mkdtemp(prefix="navbench-")
    b = Bench(a.quick, tmp)
    q = a.quick
    report = {"note": "synthetic Annex 10 signals, identical cu8 IQ for every decoder; see bench/navaid/run.py"}

    cn0_vor = [30, 34, 38, 42, 46, 50, 55] if not q else [34, 42, 50]
    radials = [17.0, 123.0, 251.0, 338.0] if not q else [123.0]
    dur = 30

    if want("vor_radial"):
        per = b.sweep(cn0_vor, [(r, i) for i, r in enumerate(radials)],
                      lambda cn0, c: b.vor_case(cn0, c[0], dur, 10 + c[1] + cn0),
                      lambda rows, c: score_radial(rows, c[0], dur))
        report["vor_radial"] = summarize(per)
        print(json.dumps(report["vor_radial"], indent=1))

    if want("vor_impair"):
        imp = {
            "carrier_err_3khz": ["--carrier-err", 3000],
            "subcarrier_err_40hz": ["--subcarrier-err", 40],
            "voice_30pct": ["--voice", 0.3],
            "dvor_asym_10pct": ["--dvor", 0.1],
        }
        report["vor_impair"] = {}
        for name, extra in imp.items():
            # 40 dB-Hz (moderate) and 55 dB-Hz (strong, where every decoder
            # holds a clean signal) so each impairment is seen on its own.
            per = b.sweep([40, 55], [(r, i) for i, r in enumerate(radials)],
                          lambda cn0, c, extra=extra: b.vor_case(cn0, c[0], dur, 50 + c[1], extra),
                          lambda rows, c: score_radial(rows, c[0], dur))
            report["vor_impair"][name] = summarize(per)
        print(json.dumps(report["vor_impair"], indent=1))

    if want("vor_ident"):
        idents = ["SAC", "OAK", "LIN", "SWR"] if not q else ["SAC"]
        cn0_id = [30, 34, 38, 42, 46, 50] if not q else [38, 46]
        per = b.sweep(cn0_id, [(s, i) for i, s in enumerate(idents)],
                      lambda cn0, c: b.vor_case(cn0, 90.0, 60, 70 + c[1] + cn0, ident=c[0], decoders=("xng", "voracious")),
                      lambda rows, c: score_ident(rows, c[0]))
        report["vor_ident"] = summarize(per)
        print(json.dumps(report["vor_ident"], indent=1))

    cn0_ils = [30, 34, 38, 42, 46, 50, 55] if not q else [34, 42, 50]
    ddms = [-0.1, 0.0, 0.05, 0.155] if not q else [0.05]

    if want("loc_ddm"):
        per = b.sweep(cn0_ils, [(d, i) for i, d in enumerate(ddms)],
                      lambda cn0, c: b.ils_case("loc", cn0, c[0], 20, 90 + c[1] + cn0),
                      lambda rows, c: score_ddm(rows, c[0], 20))
        report["loc_ddm"] = summarize(per)
        print(json.dumps(report["loc_ddm"], indent=1))

    if want("loc_twofreq"):
        # Course carrier +4 kHz with the true DDM, clearance carrier -4 kHz,
        # 10 dB weaker, with a large DDM (as off the course sector).
        per = b.sweep([40, 50], [(d, i) for i, d in enumerate(ddms)],
                      lambda cn0, c: b.ils_case("loc", cn0, c[0], 20, 110 + c[1], ["--two-freq", "8000:-10:0.3"]),
                      lambda rows, c: score_ddm(rows, c[0], 20))
        report["loc_twofreq"] = summarize(per)
        print(json.dumps(report["loc_twofreq"], indent=1))

    if want("gs_ddm"):
        gddms = [-0.0875, 0.0, 0.04, 0.175] if not q else [0.04]
        per = b.sweep(cn0_ils, [(d, i) for i, d in enumerate(gddms)],
                      lambda cn0, c: b.ils_case("gs", cn0, c[0], 20, 130 + c[1] + cn0, decoders=("xng", "dumpils")),
                      lambda rows, c: score_ddm(rows, c[0], 20))
        report["gs_ddm"] = summarize(per)
        print(json.dumps(report["gs_ddm"], indent=1))

    if want("loc_ident"):
        idents = ["ISAC", "IOAK", "IJFK", "ILAX"] if not q else ["ISAC"]
        cn0_id = [30, 34, 38, 42, 46, 50] if not q else [38, 46]
        per = b.sweep(cn0_id, [(s, i) for i, s in enumerate(idents)],
                      lambda cn0, c: b.ils_case("loc", cn0, 0.0, 60, 150 + c[1] + cn0, ident=c[0], decoders=("xng", "voracious")),
                      lambda rows, c: score_ident(rows, c[0]))
        report["loc_ident"] = summarize(per)
        print(json.dumps(report["loc_ident"], indent=1))

    report["cpu_s_per_signal_s"] = {k: v[0] / v[1] for k, v in sorted(b.cpu.items()) if v[1] > 0}
    print(json.dumps(report["cpu_s_per_signal_s"], indent=1))
    os.makedirs(os.path.dirname(a.out) or ".", exist_ok=True)
    with open(a.out, "w") as fh:
        json.dump(report, fh, indent=1, sort_keys=True)
    print("wrote", a.out)


if __name__ == "__main__":
    main()
