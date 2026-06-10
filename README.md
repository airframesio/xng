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

## Current state (M7 — seven native decode cores; wave 1 complete)

Seven native decode cores are in:

- **VHF ACARS** (ARINC 618): MSK discriminator demod, differential decode,
  sync/parity/CRC deframing, parity-guided single-bit error correction.
  Validated off-air against an RTL-SDR (live United/American frames,
  CRC-verified). Application layer via **`xng-acars`** (libacars port, MIT):
  ARINC 622 envelopes with CRC, full **ADS-C** decode (positions!), media
  advisory, H1 sublabel/MFI — conformance-tested against real off-air
  ADS-C messages.
- **AIS** (ITU-R M.1371): GMSK demod with carrier-offset tracking, HDLC
  deframing with destuffing, CRC-16/X-25, NMEA AIVDM output — verified
  against the canonical published AIVDM test vector.
- **VDL Mode 2** (ICAO Annex 10 Vol III / ETSI EN 301 841): D8PSK burst
  demod with unique-word acquisition and carrier-offset tracking,
  RS(255,249) errors-and-erasures FEC, scrambler, AVLC link layer,
  ACARS-over-AVLC into the shared application layer. Verified against
  spec-derived vectors (scrambler keystream, header FEC, unique word) and
  RF loopback; off-air validation pending usable VDL2 RF at this site.
- **Inmarsat Aero** (ported from MIT-licensed JAERO): L-band A-BPSK/MSK
  P-channels at 600/1200 bps (both rates decoded in parallel per channel)
  and C-band R/T-channel bursts (`--mode aero-c`: burst gating, carrier
  CFO estimation, R-SU and T-burst signal-unit layers), K=7 Viterbi,
  64-row interleaver, ISU/SSU reassembly, ACARS into the shared
  application layer. 10.5 kbps A-QPSK: framing complete (bit-level
  tested); OQPSK demod in progress.
- **Inmarsat STD-C / EGC** (clean-room from cross-verified facts; first
  coherent demod in the codebase — square-law AFC, Costas, Gardner):
  NCS frames (UW sync both polarities, row depermutation, 64×162
  deinterleave, Viterbi, group descrambler), packet layer with Fletcher
  checksums, multiframe and logical-channel assembly, and EGC SafetyNET/
  FleetNET messages with service/priority decoding (`--mode std-c`).
- **HFDL** (ICAO Annex 10 Vol III Ch. 11 / ARINC 635 — the first native
  Rust HFDL decoder): M-PSK burst demod at all four rates (300/600/1200/
  1800 bps; BPSK/4PSK/8PSK with rate-1/4 chip doubling), A1/A2/M1
  preamble acquisition with cyclic-shift rate detection, per-T-segment
  phase tracking, 40-row interleaver, shared Viterbi, SPDU squitters,
  MPDU/LPDU/HFNPDU with enveloped ACARS into the shared application
  layer (`--mode hfdl`).
- **Mode S / ADS-B** (ICAO Annex 10 Vol IV): magnitude-domain PPM demod,
  CRC-24 validation with an ICAO cache for address-overlaid parity,
  extended-squitter ident/altitude decode — verified against published
  Mode S frames; zero false positives on off-air noise.

ACARS and AIS decode any number of channels from one SDR capture;
Mode S consumes the whole capture at 1090 MHz.

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

# ADS-B / Mode S
xng listen --sdr driver=rtlsdr --mode adsb -r 2000000 -c 1090.000M --channels 1090

# VDL Mode 2: four channels incl. the worldwide CSC
xng listen --sdr driver=rtlsdr --mode vdl2 -r 2400000 -c 136.800M \
    --channels 136.650,136.800,136.925,136.975

# Inmarsat Aero L-band P-channels (patch antenna + LNA at 1545-1547 MHz)
xng listen --sdr driver=rtlsdr --mode aero -r 2400000 -c 1546.000M \
    --channels 1545.880,1546.045

# Inmarsat STD-C / EGC (SafetyNET maritime safety broadcasts)
xng listen --sdr driver=rtlsdr --mode std-c -r 2400000 -c 1537.500M \
    --channels 1537.700,1537.100

# HFDL (needs an HF-capable SDR/upconverter; channels from systable)
xng listen --sdr driver=sdrplay --mode hfdl -r 768000 -c 10060.000k \
    --channels 10027k,10060k,10063k,10081k,10084k,10087k

# Generate a synthetic test capture (no hardware needed)
cargo run -p xng-mode-acars --example gen_capture -- /tmp/acars.cf32

xng devices                     # enumerate SDRs via SoapySDR
xng iq-info capture.cf32 -r 2000000 -c 131500000   # power, spectral peaks
xng selftest                    # end-to-end pipeline self-test
```

Outputs: pretty console, raw JSON, JSONL files, acarsdec-compatible JSON
over UDP (`--udp host:port`, `--feed-airframes` → feed.airframes.io:5550),
and the new **asf-2.0** protocol ([docs/ASF2.md](docs/ASF2.md)): one
protobuf schema over gRPC (`--asf2-grpc URL`) and QUIC (`--asf2-quic
host:port`), multiplexing every channel/SDR/mode over a single connection.
`xng ingest` runs the reference ingest server for both transports:

```bash
xng ingest --grpc 0.0.0.0:6001 --quic 0.0.0.0:6011   # receive asf-2.0 feeds
```

Workspace crates so far: `xng-types` (normalized message model),
`xng-proto` (asf-2.0 schema + conversions), `xng-acars` (ACARS
application layer: ARINC 622/ADS-C, shared by five modes),
`xng-dsp` (PFB channelizer, DDC, FIR/NCO, CRCs), `xng-sdr` (SoapySDR +
IQ-file sources), `xng-mode-acars`, `xng-mode-ais`, and
`xng-mode-adsb` (decode cores, each with a spec-faithful modulator for
loopback tests). Remaining modes land
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
