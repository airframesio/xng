# DME benchmark: xng-mode-dme against gr-dme and OnAir

Runs xng's DME receiver, [gr-dme](https://github.com/daniestevez/gr-dme)
(GNU Radio 3.10, GPL-3.0) and the DME mode of
[OnAir](https://github.com/adswill/OnAir) (GPL-3.0) on the same synthetic
X-channel transponder signals. gr-dme and OnAir are used only as installed
black boxes; no code from either is in xng. These are the open-source DME
decoders we found; gr-dme finds pairs only, OnAir and xng also read the
ident.

xng and gr-dme write pair times, which are scored against the true pair
times. OnAir's command line prints only the pair rate of the last second and
the ident letters, so for OnAir the benchmark sums those rates over the file
(pairs counted, false pairs included) and checks the ident. OnAir plays a
file in real time, so a full run takes about 25 minutes with it. Results and method are in
[docs/notes/DME.md](../../docs/notes/DME.md).

Signals (`dme_bench sim`): 2.5 MS/s (gr-dme's example rate), 5 s each, 2700
pp/s random squitter/replies with a 60 µs dead time, a "SFO" Morse ident at
1350 pp/s, Gaussian 3.5 µs pulses, AWGN. SNR is the peak pulse power over the
noise in 1 MHz. A detection counts when it is within 1.5 µs of a transmitted
pair, after removing each decoder's constant timing offset (measured once at
25 dB); anything else is a false pair.

Also: a 20 s noise-only file (false pairs per second, with no beacon) and the
ident at the 0.1 s and 0.16 s dot lengths Annex 10 allows (125 ms
elsewhere).

## Running it

```bash
# gr-dme (outside this repo)
sudo apt install gnuradio gnuradio-dev cmake g++ pybind11-dev libspdlog-dev
git clone https://github.com/daniestevez/gr-dme && cd gr-dme && mkdir build && cd build
cmake .. -DCMAKE_INSTALL_PREFIX=$HOME/grdme -DPYTHON_EXECUTABLE=/usr/bin/python3.12 && make -j && make install

# Then, from the xng checkout
cargo build --release -p xng-mode-dme --example dme_bench
export LD_LIBRARY_PATH=$HOME/grdme/lib/x86_64-linux-gnu
export PYTHONPATH=$HOME/grdme/lib/python3.12/site-packages
# OnAir (optional; outside this repo): its command line receiver, dect2cli
#   (installed as onair-cli). The build needs libhackrf-dev, libusb-1.0-0-dev
#   and the FFmpeg -dev packages; tests stay on, because dect2cli is in them.
git clone --depth 1 https://github.com/adswill/OnAir && cmake -S OnAir -B onair-build -DCMAKE_BUILD_TYPE=Release -DDECT2_HAVE_HAMLIB=OFF
cmake --build onair-build --target dect2cli -j
export ONAIR=$PWD/onair-build/dect2cli

python3 bench/dme/run.py /tmp/dme-bench          # about 10 minutes (25 with OnAir); --quick for 2
python3 bench/dme/plot.py /tmp/dme-bench/results.json dme-bench.png   # needs matplotlib
```

`GRPY` selects the Python that has GNU Radio (default `/usr/bin/python3.12`,
Ubuntu 24.04).

## Files

- `run.py`: generates the signals, runs both decoders, scores, writes
  `results.json`.
- `grdme_run.py`: builds gr-dme's `examples/dme_receiver.grc` chain with
  that example's parameters (threshold 10, 12 µs spacing) and writes the
  pair times.
- `onair_run.py`: runs OnAir's DME receiver on one file and reads its
  per-second summary lines (pair count, ident, its own CPU figure).
- `plot.py`: the chart in the PR and the notes.
- `results.json`: the run quoted in the notes.
