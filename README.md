# xng

[![Rust](https://github.com/airframesio/xng/actions/workflows/rust.yml/badge.svg?branch=master)](https://github.com/airframesio/xng/actions/workflows/rust.yml)

Next-generation **multi-mode SDR decoder**, written in Rust.

One binary that natively decodes ACARS, VDL Mode 2, HFDL, Inmarsat Aero +
STD-C, AIS, ADS-B (and later Iridium) — replacing acarsdec, vdlm2dec,
dumpvdl2, dumphfdl, JAERO, and friends with consistent decode cores, shared
SDR captures with multi-channel decode, first-class
[airframes.io](https://airframes.io) feeding (including the new gRPC/QUIC
`asf-2.0` output), rich statistics, a strong CLI, and an interactive TUI with
realtime diagnostics and auto-scanning.

**Status: ground-up rewrite in progress.** The architecture, research, and
roadmap live in [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md). The previous
xng (a dumphfdl session wrapper) is preserved in [`legacy/`](legacy/) and
still buildable standalone.

## Current state (M2 in progress — ACARS + AIS native)

Two native decode cores are in, both clean-room and both decoding any
number of channels from one SDR capture:

- **VHF ACARS** (ARINC 618): MSK discriminator demod, differential decode,
  sync/parity/CRC deframing, parity-guided single-bit error correction.
  Validated off-air against an RTL-SDR (live United/American frames,
  CRC-verified).
- **AIS** (ITU-R M.1371): GMSK demod with carrier-offset tracking, HDLC
  deframing with destuffing, CRC-16/X-25, NMEA AIVDM output — verified
  against the canonical published AIVDM test vector.

```bash
# Live: two ACARS channels from one RTL-SDR capture, feeding Airframes
xng listen --sdr driver=rtlsdr -r 2400000 -c 131.500M \
    --channels 131.550,131.725 \
    --feed-airframes --station-id XX-KSEA-ACARS1

# From a recording
xng decode capture.cf32 -r 2400000 -c 131.500M --channels 131.550,131.425

# AIS: both channels from one capture
xng listen --sdr driver=rtlsdr --mode ais -r 2400000 -c 162.000M \
    --channels 161.975,162.025

# Generate a synthetic test capture (no hardware needed)
cargo run -p xng-mode-acars --example gen_capture -- /tmp/acars.cf32

xng devices                     # enumerate SDRs via SoapySDR
xng iq-info capture.cf32 -r 2000000 -c 131500000   # power, spectral peaks
xng selftest                    # end-to-end pipeline self-test
```

Outputs: pretty console, raw JSON, JSONL files, and acarsdec-compatible
JSON over UDP (`--udp host:port`, `--feed-airframes` →
feed.airframes.io:5550). The asf-2.0 gRPC/QUIC output lands in M3.

Workspace crates so far: `xng-types` (normalized message model),
`xng-dsp` (PFB channelizer, DDC, FIR/NCO, CRCs), `xng-sdr` (SoapySDR +
IQ-file sources), `xng-mode-acars` and `xng-mode-ais` (decode cores, each
with a spec-faithful modulator for loopback tests). Remaining modes land
in milestone order: see the
[roadmap](docs/ARCHITECTURE.md#5-roadmap).

## Building

1. Install a stable [Rust](https://www.rust-lang.org/learn/get-started) toolchain.
2. Install SoapySDR development files (`libsoapysdr-dev` on Debian/Ubuntu,
   `soapysdr` via Homebrew on macOS) plus the vendor modules for your
   hardware. Or build without hardware support: `cargo build --no-default-features`.
3. Build:

```bash
cargo build --release    # binary at ./target/release/xng
cargo test --workspace
```

## License

Dual-licensed under [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE), at
your option. Decode cores are implemented clean-room from public standards
(ICAO, ARINC, ITU-R) or ported from permissively licensed projects — see
`docs/ARCHITECTURE.md` §6 for provenance rules.
