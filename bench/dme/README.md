# DME benchmark: xng-mode-dme against gr-dme

Runs xng's DME receiver and [gr-dme](https://github.com/daniestevez/gr-dme)
(GNU Radio 3.10, GPL-3.0, used only as an installed black box) on the same
synthetic X-channel transponder signals and scores both against the true
pulse-pair times. Results and method are in
[docs/notes/DME.md](../../docs/notes/DME.md).

Signals (`dme_bench sim`): 2.5 MS/s (gr-dme's example rate), 5 s each, 2700
pp/s random squitter/replies with a 60 µs dead time, a "SFO" Morse ident at
1350 pp/s, Gaussian 3.5 µs pulses, AWGN. SNR is the peak pulse power over the
noise in 1 MHz. A detection counts when it is within 1.5 µs of a transmitted
pair, after removing each decoder's constant timing offset (measured once at
25 dB); anything else is a false pair.

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
python3 bench/dme/run.py /tmp/dme-bench          # about 10 minutes; --quick for 2
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
- `plot.py`: the chart in the PR and the notes.
- `results.json`: the run quoted in the notes.
