#!/usr/bin/env python3
"""Run OnAir's DME receiver (onair-cli / dect2cli, GPL-3.0) on a cf32 file.

OnAir is used as an installed black box, like gr-dme: no OnAir code is in
xng. Its command line prints one summary line per second, for example

    level -27.6 dBFS | DME X, 2613 pairs/s, ident SFO | dropped 0

so it gives the pair rate over the last second and the ident letters, not
pair times. OnAir plays a file in real time, so a 5 s file takes 5 s.

  onair_run.py <onair-cli> <in.cf32> <rate> <seconds> <freq_hz>

Prints, like dme_bench: `COUNT <pairs>` (the per-second rates summed over
the file), `IDENT <letters>` (spaces removed) and
`CPU <s> WALL <s> SECONDS <s>`. CPU is OnAir's own figure for its receiver
front end and decoder (its `cpu:` line), not the whole process, which also
starts receivers for other modes.
"""
import re
import subprocess
import sys
import time

RATE_RE = re.compile(r"DME [^,]*, (\d+) pairs/s(?:, (?:no ident yet|ident ([^|]*)))?[^|]*\|")
CPU_RE = re.compile(r"front ([\d.]+) s .*?decoder ([\d.]+) s")


def main():
    exe, path, rate, secs, freq = sys.argv[1:6]
    secs = float(secs)
    t0 = time.monotonic()
    proc = subprocess.run(
        [exe, "--standard", "dme", "--file", path, "--format", "cf32",
         "--rate", str(float(rate) / 1e6), "--freq", str(float(freq) / 1e6),
         "--secs", str(secs + 1.5)],
        capture_output=True, text=True, timeout=secs + 120,
    )
    if proc.returncode != 0:
        sys.stderr.write(proc.stderr)
        sys.exit(f"onair-cli exited with {proc.returncode}")
    out = proc.stdout
    wall = time.monotonic() - t0
    rates, ident = [], ""
    cpu = float("nan")
    for line in out.splitlines():
        m = RATE_RE.search(line)
        if m:
            rates.append(int(m.group(1)))
            if m.group(2):
                ident = m.group(2).strip()
        elif "no pulse pairs" in line:
            rates.append(0)
        c = CPU_RE.search(line)
        if line.startswith("cpu:") and c:
            cpu = float(c.group(1)) + float(c.group(2))
    # One line per second of playback; after the file ends the last rate is
    # repeated, so only the first `secs` lines count.
    count = sum(rates[: int(round(secs))])
    print(f"COUNT {count}")
    if ident:
        print(f"IDENT {ident.replace(' ', '')}")
    print(f"CPU {cpu:.4f} WALL {wall:.4f} SECONDS {secs:.3f}")


if __name__ == "__main__":
    main()
