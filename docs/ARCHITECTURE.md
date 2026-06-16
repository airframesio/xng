# xng — architecture

xng is one Rust binary that decodes ACARS (POA), VDL Mode 2, HFDL,
Inmarsat Aero (L and C), Inmarsat STD-C/EGC, Iridium, AIS, and Mode
S/ADS-B. Every mode shares one capture layer, one normalized message
model, one application layer, and one set of outputs. Native cores are
self-contained (no external decoder binaries or libraries); wrapping an
external client is supported but second-class.

This document is the system architecture. Per-mode PHY/FEC/protocol
detail lives in [`docs/notes/`](notes/); sourcing in
[`docs/REFERENCES.md`](REFERENCES.md) and per-crate `PROVENANCE.md`; the
feed protocol in [`docs/ASF2.md`](ASF2.md).

## Licensing model

Dual MIT OR Apache-2.0. The discipline is enforced per source:

- **GPL projects** (dumphfdl, dumpvdl2, AIS-catcher, gr-iridium, readsb,
  SatDump, Scytale-C) are used as *fact* references only. Protocol facts
  are not copyrightable; the code is clean-roomed from public specs
  (ICAO, ARINC, ITU-R, ETSI) and the facts.
- **MIT/BSD projects** (libacars, JAERO, iridium-toolkit) are ported
  directly with attribution.

Every mode crate carries a `PROVENANCE.md` recording exactly what came
from where.

## Pipeline

```
                    ┌───────────────────────────────────────────────────────────┐
                    │                       xng runtime                         │
  SDR ──Soapy/───►  │  Capture ──► channelizer ──► ch0..chN (narrowband IQ)     │
       native       │  Capture ──► channelizer ──► ...                          │
  file/stdin ─────► │  (IQ replay)                                              │
                    │        ▼ per channel                                      │
                    │  DemodCore (MSK / D8PSK / PSK / GMSK / BPSK / PPM / DQPSK) │
                    │        ▼                                                  │
                    │  FrameCore (AVLC / MPDU-LPDU / SU / HDLC / Mode S / ...)   │
                    │        ▼                                                  │
                    │  AppLayer (xng-acars: ACARS, ADS-C, CPDLC, MIAM, ARINC620)│
                    │        ▼                                                  │
                    │  Normalized Message ──► message bus                       │
                    └───────────────┬───────────────────────────────────────────┘
                                    │ fan-out (broadcast)
      ┌──────────┬─────────────┬────┴──────────┬──────────────┬───────────────┐
      ▼          ▼             ▼               ▼              ▼               ▼
  asf-2.0     asf-2.0      legacy JSON     console/log    metrics         local sinks
  gRPC        QUIC         (acarsdec/      (pretty/JSON)  (Prometheus,    (JSONL, SBS,
  (tonic)     (quinn)      dumpvdl2/...)                  web dashboard)  Beast, NMEA)
```

Key properties:

- **Capture sharing.** One SDR capture feeds many channels, and channels
  of different modes can share a capture when their bands overlap (ACARS
  + VDL2 in 136-137 MHz; Aero + STD-C in 1537-1547 MHz). The channelizer
  is mode-agnostic; mode cores subscribe to channel outputs at their
  required rates. This is the central win over one-decoder-per-SDR tools.
- **Per-mode decode contract.** Each mode is a crate that turns channel
  IQ into raw frames and raw frames into normalized messages. Adding a
  mode is adding a crate; the runtime, outputs, stats, TUI, and feeding
  all work unchanged.
- **Channelizer vs burst pipeline.** Fixed-channel modes run a DDC per
  channel (two-stage windowed-sinc). Iridium instead runs a wideband
  FFT burst detector across the whole capture and channelizes each
  detected burst on demand (see [IRIDIUM.md](notes/IRIDIUM.md)); the
  downstream contract is identical.
- **Extern wrappers (second-class).** `xng extern` spawns an external
  client (dumphfdl, dumpvdl2, acarsdec, JAERO), parses its output, and
  injects normalized messages onto the same bus, so wrapped decoders get
  every xng output, the application layer (ADS-C/CPDLC even from wrapped
  ACARS), and stats. Used for gap-bridging and A/B validation, not
  encouraged for primary decode.

## Workspace layout

```
xng/
├── crates/
│   ├── xng-types          # normalized message model, ids, time, units
│   ├── xng-dsp            # channelizer, DDC, FIR/NCO, Viterbi, Reed-Solomon,
│   │                      #   interleavers, scramblers, CRCs, sync/AGC
│   ├── xng-sdr            # SoapySDR + native Airspy (libairspy/libairspyhf)
│   │                      #   capture, device enumeration, IQ replay, rate negotiation
│   ├── xng-acars          # application layer: ARINC 618/620/622, ADS-C, CPDLC, MIAM
│   ├── xng-proto          # asf-2.0 .proto + prost/tonic codegen + conversions
│   └── xng-mode-*         # one decode core per mode (acars, vdl2, hfdl, aero,
│                          #   ais, adsb, stdc, iridium), each with a spec-faithful
│                          #   modulator for loopback tests, vendored fixtures, PROVENANCE.md
├── src/                   # the xng binary
│   ├── main.rs, commands/ # CLI (listen, scan, survey, decode, iq-info, devices,
│   │                      #   selftest, tui, station, status, ingest, extern, config)
│   ├── runtime.rs, bus.rs # session supervisor + message bus
│   ├── outputs/           # console, JSON/JSONL, acarsdec UDP, Airframes, Prometheus,
│   │                      #   SBS/Beast, NMEA, MQTT, asf-2.0, and the web dashboard
│   ├── tui.rs             # ratatui TUI
│   ├── beam.rs, satmap.rs # Iridium beam-pattern reconstruction + satellite naming
│   └── freq.rs, sdr_args.rs
├── proto/                 # asf-2.0 protobuf sources
├── bench/                 # off-air decode-count regression gate (run.sh + baselines.json)
└── docs/
```

The library crates useful beyond xng (`xng-acars`, `xng-dsp`,
`xng-types`, `xng-proto`) are the permissive building blocks the
ecosystem otherwise lacks. Outputs, stats, the TUI, and extern wrappers
live in the binary rather than as separate crates.

## Application layer

ACARS from every carrier (VHF POA, VDL2, HFDL, Aero, Iridium SBD) flows
through `xng-acars` (ported from MIT libacars): ARINC 618/620 text,
ARINC 622 ADS-C, CPDLC in both dialects (FANS-1/A over ACARS, ATN-B1
over VDL2/CLNP) rendered as readable text, MIAM (DEFLATE), OHMA, media
advisory, H1 sublabels, and multi-block reassembly. One implementation
serves five carriers.

## Outputs and feeding

Every mode and command shares one output set: pretty console, JSON/JSONL,
acarsdec-compatible UDP, Airframes feeding, Prometheus `/metrics`,
SBS/Beast (Mode S), NMEA AIVDM (AIS), MQTT, and **asf-2.0** — one
protobuf schema multiplexing every channel/SDR/mode over a single gRPC
(tonic/HTTP/2) or QUIC (quinn) connection, with the raw payload always
preserved for server-side re-decode. `xng ingest` is the reference
server. Legacy decoder-native JSON is retained so existing Airframes
ingests work unchanged. Full protocol in [ASF2.md](ASF2.md).

## Statistics and control

Per-(sdr, channel, mode, output) counters track messages, frames, FEC
corrections, CRC failures, signal/noise, bandwidth, sample drops, and
uptime. They surface as log lines, JSONL, Prometheus (label families
compatible with acarshub's acars/vdlm/hfdl/imsl/irdm), `StationStats`
frames in asf-2.0, the TUI, and the web dashboard. `xng status` queries
a running station's dashboard endpoint for a live per-session table.

## Interfaces

- **CLI** (clap): `listen` (one or many modes), `scan` (auto-detect
  receivable channels and propose configs), `survey` (qualify a site
  with a gain sweep and per-channel report), `decode`/`iq-info`
  (IQ files), `tui`, `station` (whole-site TOML config), `status`,
  `ingest`, `extern`, `devices`, `selftest`, `config`.
- **TUI** (ratatui): live message browser with a detail pane, per-channel
  stats, spectrum with channel markers, and a waterfall, over a live SDR
  or a replayed file.
- **Web dashboard** (`--http`): an embedded dark map of decoded aircraft
  (Mode S) and vessels (AIS) with trails, an entity table, a filterable
  streaming message panel, and Iridium overlays (satellite tracks,
  spot-beam footprints, the reconstructed 48-beam pattern, mobile
  terminals). Assets are embedded in the binary; RF-sourced strings are
  HTML-escaped.
- **Station mode**: one process runs a whole receive site — several
  modes on several SDRs sharing one feed, one output set, and one
  metrics endpoint, described by a TOML config (sessions can also replay
  IQ files for regression runs).

## DSP building blocks (`xng-dsp`)

Written in-house where no suitable permissive crate exists: a polyphase
channelizer and a two-stage DDC, a soft-decision K=7 Viterbi, Reed-
Solomon errors-and-erasures decoders and the per-mode interleavers and
scramblers, plus sync/AGC/NCO primitives. `rustfft`/`realfft` provide
the FFT; the `crc` crate provides CRC variants. Decode cores never see
the SDR backend — `xng-sdr` presents one `IqSource` trait over SoapySDR,
the native Airspy backends, and IQ replay.
