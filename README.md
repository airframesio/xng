# xng

[![Rust](https://github.com/airframesio/xng/actions/workflows/rust.yml/badge.svg?branch=master)](https://github.com/airframesio/xng/actions/workflows/rust.yml)
[![Release](https://img.shields.io/github/v/release/airframesio/xng)](https://github.com/airframesio/xng/releases/latest)

**One native SDR decoder for the whole aviation + maritime radio stack.**

xng decodes **ACARS, VDL Mode 2, HFDL, Inmarsat Aero, Inmarsat STD-C/EGC,
Iridium, AIS, and Mode S/ADS-B** in a single Rust binary — replacing
acarsdec, vdlm2dec, dumpvdl2, dumphfdl, JAERO, Scytale-C, gr-iridium, and
iridium-toolkit with consistent, tested decode cores that share one
capture, one message model, one application layer, and one set of outputs
(including first-class [airframes.io](https://airframes.io) feeding).
All nine modes are implemented, validated, and merged — including the
complete Iridium stack (ring alerts through ACARS-over-SBD with a
wideband burst-hunting front end) — and shipped as tagged releases with
binaries for Linux (x86_64/arm64, tarball + .deb), macOS Apple Silicon,
and multi-arch Docker images.

```bash
# Two ACARS channels from one RTL-SDR, fed to Airframes:
xng listen --sdr driver=rtlsdr -r 2400000 -c 131.500M \
    --channels 131.550,131.725 \
    --feed-airframes --station-id XX-KSEA-ACARS1
```

```text
12:01:13.402 [acars] 131.550 MHz ACARS N401UA UA1989 lbl=H1 ok | #M1B...
12:01:14.118 [acars] 131.725 MHz ACARS N831UA UA0233 lbl=B6 ok [ADS-C 47.5512 -122.3052 34000 ft]
```

## Why xng

| | Existing tools | xng |
|---|---|---|
| **Decoders** | One binary per mode (acarsdec, dumpvdl2, dumphfdl, JAERO, …), each with its own CLI, output format, and quirks | One binary, one CLI, every mode |
| **SDR usage** | One SDR per decoder | Many channels of one mode from a single capture; one dongle can watch 12 ACARS channels |
| **Validation** | Varies | Every decode core is validated against **real off-air recordings** or the reference implementation's own test vectors, with the captures vendored into CI so conventions can never silently regress |
| **Application layer** | libacars bolted on, or nothing | Built-in ARINC 622 (ADS-C positions, **CPDLC rendered as readable text** — `REQUEST CLIMB TO FL360`), media advisory, H1 sublabels — shared by every ACARS carrier (VHF, VDL2, HFDL, Aero, Iridium SBD) |
| **Outputs** | Per-tool formats | Pretty console, JSON/JSONL, acarsdec-compatible UDP, Airframes feeding, Prometheus metrics, and the multiplexed gRPC/QUIC **asf-2.0** protocol — identical across all modes |
| **Tooling** | None | Interactive TUI (spectrum, waterfall, message browser), auto-scanner that proposes ready-to-run configs, site survey/soak reports with gain tuning, IQ-file inspection, built-in self-test |
| **License** | Mostly GPL | MIT/Apache-2.0 dual license; cores are clean-room from public specs or ported from MIT/BSD projects with attribution |

The provenance discipline is part of the engineering: every core has a
`PROVENANCE.md` recording exactly what came from where, and the off-air
validation campaigns are documented finding-by-finding (several on-air
conventions — invisible to loopback testing — were caught only this way).

## Supported modes

| Mode | `--mode` | Band | What you get | Validation |
|---|---|---|---|---|
| VHF ACARS (ARINC 618) | `acars` (default) | 118–137 MHz | ACARS + applications | Live off-air (RTL-SDR), CRC-verified, **fed to production Airframes end-to-end** |
| VDL Mode 2 (ICAO Annex 10) | `vdl2` | 136.6–137 MHz | ACARS-over-AVLC, AVLC link events, XID handoff parameters (incl. ground-station lists), **ATN-B1: X.25/CLNP/COTP transport, protected-mode CPDLC with the full element tables and phraseology, CM logon** | Off-air capture vs dumpvdl2 ground truth |
| HFDL (ARINC 635) | `hfdl` | 2.8–22 MHz | Squitters, logons, positions, ACARS, **over-the-air system table** | Off-air 21 931 kHz capture, field-exact vs dumphfdl |
| Inmarsat Aero L (JAERO port) | `aero` | 1545–1547 MHz | P-channels 600/1200 bps + 10.5 kbps, ACARS/ADS-C/CPDLC | Real Inmarsat recordings: 600 bps + 10.5k both decode off-air |
| Inmarsat Aero C bursts | `aero-c` | C-band | R/T-channel signal units | RF loopback |
| Inmarsat STD-C / EGC | `std-c` | 1537–1542 MHz | NCS frames, EGC SafetyNET/FleetNET text, logical-channel messages | Off-air EGC capture, field-exact vs reference |
| Iridium | `iridium` | 1616–1626.5 MHz | Ring alerts (live satellite positions), broadcasts, **ACARS over SBD**, **pager messages (IMS) with multi-part reassembly**, voice/IP-data tagging with AMBE payload extraction, wideband burst hunting across the band | Every layer validated: bit-perfect demod of gr-iridium's reference burst (direct *and* via the wideband hunter) + field-identical decode vs the iridium-toolkit oracle |
| AIS (ITU-R M.1371) | `ais` | 161.975/162.025 MHz | NMEA AIVDM | Canonical published test vector |
| Mode S / ADS-B | `adsb` | 1090 MHz | **CPR positions** (global+local, airborne+surface), velocity/heading/vertical rate, squawk, altitude replies (Q-bit + Gillham), per-aircraft tracking, **SBS/BaseStation output** | Published reference vectors (1090 Riddle worked examples); zero false positives on noise |

All multi-channel modes decode any number of channels from one capture.
Wrapped external decoders (`xng extern`) remain available as a
second-class path — they get every xng output and the application layer.

## Supported hardware

| Device | `--sdr` | Backend | Notes |
|---|---|---|---|
| RTL-SDR | `driver=rtlsdr` | SoapySDR | The budget workhorse for VHF (ACARS, VDL2, AIS) and — with an L-band antenna + LNA — Aero, STD-C, Iridium, ADS-B |
| Airspy R2 / Mini | `driver=airspy` | **native** (libairspy, `--features airspy`) | 24 MHz–1.75 GHz, 12-bit; `serial=…` (hex) selects a unit, `bias=1` powers an LNA. Validated live (Mini: off-air ACARS at 6 MS/s) |
| Airspy HF+ / Discovery | `driver=airspyhf` | **native** (libairspyhf, `--features airspyhf`) | The classic HFDL receiver; 768 kS/s divides cleanly into every xng HF/VHF channel rate |
| SDRplay (RSP series) | `driver=sdrplay` | SoapySDR | The Soapy module wraps the proprietary API |
| Anything else | per its Soapy module | SoapySDR | HackRF, LimeSDR, USRP, BladeRF, … |

`--gain` is in dB everywhere, and omitting it selects hardware AGC. The
native Airspy backends map dB sensibly onto the actual hardware controls
(R2/Mini: 22-step linearity gain; HF+: attenuator/preamp, bigger = more
gain). With a native backend compiled in, its driver name routes to it
automatically; add `backend=soapy` to force SoapySDR instead. IQ-file
input (`xng decode`) needs no hardware or SDR libraries at all.

## Installing

Grab a [release](https://github.com/airframesio/xng/releases/latest) —
tarballs for Linux x86_64/arm64 and macOS Apple Silicon, `.deb` packages
for Debian/Ubuntu (which declare the runtime libraries), and `SHA256SUMS`.
The binaries need `libsoapysdr` at runtime (plus `libairspy`/`libairspyhf`
for native Airspy):

```bash
sudo apt install ./xng_0.9.0_arm64.deb     # pulls runtime deps
# or
tar xzf xng-v0.9.0-x86_64-unknown-linux-gnu.tar.gz && sudo cp xng-*/xng /usr/local/bin/
```

Multi-arch Docker images (amd64/arm64/armv7) are published per tag:

```bash
docker run --rm ghcr.io/airframesio/xng:latest --version
```

## Building

Requirements: a stable [Rust](https://rustup.rs) toolchain, a **protobuf
compiler**, and (for live SDR use) **SoapySDR** with your vendor module.

```bash
# Debian / Ubuntu
sudo apt install protobuf-compiler libsoapysdr-dev soapysdr-module-all

# macOS
brew install protobuf soapysdr

# Build (binary at ./target/release/xng)
cargo build --release

# Run the test suite (includes the vendored off-air captures)
cargo test --workspace
```

Airspy owners can skip the SoapyAirspy shim: native backends for the
R2/Mini (libairspy) and the HF+ / Discovery (libairspyhf) are built in
with feature flags:

```bash
sudo apt install libairspy-dev libairspyhf-dev   # or: brew install airspy airspyhf
cargo build --release --features airspy,airspyhf
```

No hardware? Everything works from IQ recordings (`xng decode`,
`xng tui --file`), and `cargo build --no-default-features` skips SoapySDR
entirely. A Dockerfile and a Debian packaging script are included.

```bash
docker build -t xng . && docker run --rm xng --version
```

## Quick start

```bash
xng devices                       # what SDRs are attached?
xng selftest                      # end-to-end pipeline sanity check

# No idea what's receivable at your site? Let the scanner find out:
xng scan --sdr driver=rtlsdr --gain 28 --modes acars,vdl2,ais --dwell 120 --out scan.json
# → prints verdicts per channel and ready-to-paste `xng listen` command lines.
#   For HFDL it even learns new frequencies from the over-the-air system table.

# Then qualify the site properly: a 15-minute soak across the whole ACARS
# plan, with an empirical gain sweep first, ending in a per-channel report
# (frames, CRC rate, levels) plus reception advice:
xng survey --sdr driver=rtlsdr --mode acars --tune-gain --out survey.json
```

## Examples

### Live decoding

```bash
# VDL Mode 2: four channels including the worldwide CSC
xng listen --sdr driver=rtlsdr --mode vdl2 -r 2400000 -c 136.800M \
    --channels 136.650,136.800,136.925,136.975

# HFDL on an HF-capable SDR (channels per the public system table)
xng listen --sdr driver=sdrplay --mode hfdl -r 768000 -c 10060.000k \
    --channels 10027k,10060k,10063k,10081k,10084k,10087k

# Same, on an Airspy HF+ Discovery via the native backend (no Soapy
# module needed; build with --features airspyhf). Hardware AGC unless
# --gain is given; add serial=... to pick among several units.
xng listen --sdr driver=airspyhf --mode hfdl -r 768000 -c 10060.000k \
    --channels 10027k,10060k,10063k,10081k,10084k,10087k

# Inmarsat Aero L-band (patch antenna + LNA)
xng listen --sdr driver=rtlsdr --mode aero -r 2400000 -c 1546.000M \
    --channels 1545.880,1546.045

# Inmarsat STD-C: maritime safety broadcasts in plain text
xng listen --sdr driver=rtlsdr --mode std-c -r 2400000 -c 1537.500M \
    --channels 1537.700,1537.100

# Iridium, wideband: point at the band, get everything — bursts are
# hunted across the whole capture (ring alerts, broadcasts, and the
# duplex-hopping SBD/ACARS traffic). Triggered by --channels equal to
# the capture center:
xng listen --sdr driver=rtlsdr --mode iridium -r 2000000 -c 1626.000M \
    --channels 1626.000

# Iridium, fixed channels: just the simplex ring-alert/messaging
# frequencies (cheaper; live satellite positions every few seconds)
xng listen --sdr driver=rtlsdr --mode iridium -r 2000000 -c 1626.250M \
    --channels 1626.271,1626.104

# AIS: both channels from one capture
xng listen --sdr driver=rtlsdr --mode ais -r 2400000 -c 162.000M \
    --channels 161.975,162.025

# Mode S / ADS-B (consumes the whole capture)
xng listen --sdr driver=rtlsdr --mode adsb -r 2000000 -c 1090.000M --channels 1090
```

### Site survey / soak test

`xng survey` qualifies a site for one mode: it monitors every channel in
the mode's plan (rotating capture windows when the plan exceeds the SDR
bandwidth), prints interim tables as it goes, and ends with per-channel
statistics — frames, CRC pass rate, frames/min, levels — plus reception
advice and a ready-to-run `listen` command. Ctrl-C ends early but still
reports.

```bash
# 15-minute ACARS soak over the full plan, gain picked empirically
xng survey --sdr driver=rtlsdr --mode acars --tune-gain --out survey.json

# Scan first, then soak only active channels — plus the mode's core
# worldwide channels, which short scans routinely undersell
xng survey --sdr driver=rtlsdr --gain 28 --mode vdl2 --scan --duration 1800

# Specific channels, live message output, messages archived to JSONL
xng survey --sdr driver=rtlsdr --gain 28 --mode acars --duration 3600 \
    --channels 130.025,131.550,131.725 --show-messages --jsonl soak.jsonl
```

### Recordings

```bash
xng iq-info capture.cf32 -r 2000000 -c 131500000      # duration, power, spectral peaks
xng decode capture.cf32 -r 2400000 -c 131.500M --channels 131.550,131.425
xng decode vdl2.cf32 --mode vdl2 -r 50000 -c 136.975M --channels 136.975 --json
```

### Interactive TUI

Live message browser with a detail pane (press `v` to flip between a
human-readable rendering and raw JSON), per-channel statistics, spectrum
with channel markers, and a waterfall — over a live SDR or a replayed
file:

```bash
# Zero config: channels, center, and sample rate come from the mode's
# built-in plan — as many channels as fit the capture width and CPU.
# Native-backend devices are asked which rates they support (an Airspy
# Mini gets 3 MS/s automatically, not the plan's 2.4):
xng tui --sdr driver=rtlsdr
xng tui --sdr driver=airspy --mode vdl2

# Explicit tuning still works (and is required for --file replay)
xng tui --sdr driver=rtlsdr -r 2400000 -c 131.500M --channels 131.550,131.125
xng tui --file capture.cf32 -r 2400000 -c 131.500M --channels 131.550
```

### Feeding and outputs

Every mode and every command shares the same output options:

```bash
--feed-airframes --station-id XX-KSEA-ACARS1   # Airframes (feed.airframes.io)
--udp host:5550                                # acarsdec-compatible JSON
--json                                         # raw JSON to stdout
--jsonl messages.jsonl                         # JSONL file
--metrics 0.0.0.0:9090                         # Prometheus (frames, CRC, levels)
--sbs 0.0.0.0:30003                            # SBS/BaseStation TCP (Mode S)
--asf2-grpc http://ingest:6001                 # asf-2.0 over gRPC
--asf2-quic ingest:6011                        # asf-2.0 over QUIC (TLS verified)
```

**asf-2.0** ([docs/ASF2.md](docs/ASF2.md)) is xng's multiplexed feeding
protocol: one protobuf schema carrying every channel/SDR/mode over a
single gRPC or QUIC connection, with reconnect and backpressure handling.
`xng ingest` is the reference server:

```bash
xng ingest --grpc 0.0.0.0:6001 --quic 0.0.0.0:6011
```

### Wrapped external decoders

Existing decoder deployments can join the xng bus (and get asf-2.0,
Airframes feeding, and the application layer — ADS-C and CPDLC decode
even from wrapped ACARS):

```bash
dumpvdl2 ... | xng extern --format dumpvdl2 --asf2-grpc http://ingest:6001
xng extern --format dumphfdl --feed-airframes --station-id XX-... \
    -- dumphfdl --soapysdr driver=sdrplay ...
```

## The application layer

ACARS from any carrier flows through one application layer
(`xng-acars`, ported from MIT libacars):

- **ADS-C** (ARINC 622): full decode — positions, altitudes, contract
  tags — conformance-tested against real off-air messages.
- **CPDLC, both dialects**: FANS-1/A (over ACARS) and ATN-B1
  (over VDL2/CLNP) rendered as readable text with decoded arguments —
  `REQUEST CLIMB TO FL360`, `AT 14:32 EXPECT M0.84`, free text,
  vertical rates, headings, multi-element messages, and **route
  clearances** (`ASSIGNED ROUTE DEST KSFO, ROUTE J501 OAK`).
- **MIAM** (ARINC 841): single-transfer CORE PDUs decompressed
  (DEFLATE), file-transfer signalling decoded.
- **OHMA**: Boeing aircraft-health JSON unwrapped (base64 + zlib) into
  structured output.
- **Multi-block reassembly**: long messages spanning ACARS blocks are
  stitched back together (libacars-equivalent keying and timeouts), and
  the application layer re-runs over the complete text.
- Media advisory, H1 sublabel/MFI handling.

## Workspace layout

| Crate | Role |
|---|---|
| `xng-types` | Normalized message model shared by everything |
| `xng-dsp` | Channelizer, DDC, FIR/NCO, Viterbi, Reed-Solomon, CRCs, scramblers |
| `xng-sdr` | SoapySDR, native Airspy (libairspy/libairspyhf), and IQ-file sample sources |
| `xng-acars` | ACARS application layer (ARINC 622, ADS-C, CPDLC, media advisory) |
| `xng-proto` | asf-2.0 protobuf schema + conversions |
| `xng-mode-*` | One decode core per mode, each with a spec-faithful modulator for loopback tests, vendored validation fixtures, and a `PROVENANCE.md` |

Each core also ships `examples/` harnesses (`offair`, `dumpbits`, …) used
for the validation campaigns — point them at your own captures.

Architecture, research notes, and the roadmap live in
[`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) and
[`docs/notes/`](docs/notes/). The pre-rewrite xng (a dumphfdl session
wrapper) is preserved in [`legacy/`](legacy/).

## License

Dual-licensed under [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE), at
your option. Decode cores are implemented clean-room from public standards
(ICAO, ARINC, ITU-R, ETSI) or ported from permissively licensed projects
(libacars, JAERO, iridium-toolkit — MIT/BSD) with attribution; GPL
projects are used as *fact* references only, and the full sourcing record
is in [`docs/REFERENCES.md`](docs/REFERENCES.md) plus per-crate
`PROVENANCE.md` files.
