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

## Current state (M0 — foundation)

- Cargo workspace with the first foundation crates:
  - **`xng-types`** — the normalized message model (in-process form of the
    future asf-2.0 envelope)
  - **`xng-dsp`** — polyphase filter-bank channelizer, FIR/NCO primitives,
    CRC variants used by the decode cores
  - **`xng-sdr`** — `IqSource` abstraction: SoapySDR capture (feature
    `soapy`, on by default) and IQ file replay (cf32/cs16/cs8/cu8)
- `xng` binary with the message bus and console/JSONL outputs

```bash
xng devices                                   # enumerate SDRs via SoapySDR
xng iq-info capture.cf32 -r 2000000 \
    --center-freq 131550000                   # inspect a recording: power, peaks
xng selftest --jsonl out.jsonl                # end-to-end pipeline self-test
```

Native decode cores land in milestone order (ACARS first): see the
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
