# xng — Next-Generation Multi-Mode SDR Decoder

**Status:** Architecture blueprint (2026-06-09). This document records the research,
the decisions made, and the target architecture for the xng rewrite.

## 1. Vision

One Rust binary that replaces acarsdec, vdlm2dec, dumpvdl2, dumphfdl, JAERO,
iridium-toolkit, satdump (Inmarsat), and AIS decoders with native, accurate,
consistent decode cores under a single decoding structure — with first-class
Airframes integration, a new gRPC+QUIC multiplexed output, multi-channel decode
from shared SDR captures, strong statistics, a CLI and an interactive TUI with
realtime diagnostics and auto-scanning (a foundation for Airwaves OS).

Native cores are first-class and fully self-contained (no external decoder
binaries or libraries required). Wrapping existing external clients is
supported but second-class and not encouraged.

## 2. Decisions (locked 2026-06-09)

| Decision | Choice |
|---|---|
| License | **MIT OR Apache-2.0** (dual). GPL-3 sources (dumphfdl, dumpvdl2, AIS-catcher, gr-iridium, readsb, SatDump) must be clean-roomed from public specs, not ported. MIT/BSD sources (JAERO, libacars, iridium-toolkit, ship162, rs1090) may be ported/used directly with attribution. |
| Wave-1 native modes | ACARS POA, VDL Mode 2, HFDL, Inmarsat Aero + STD-C, AIS, ADS-B/Mode S (sequenced by difficulty within the wave; see roadmap). Iridium is wave 2. |
| Airframes transport | **New `asf-2.0` protobuf schema**, served as tonic gRPC (HTTP/2) and as length-prefixed prost frames over quinn QUIC. Legacy decoder-native JSON feeding retained as compatibility outputs. |
| Codebase | Fresh architecture in this repo. The current dumphfdl wrapper returns later as a second-class `extern` decoder module. |

## 3. Research findings (condensed)

### 3.1 Mode matrix

| Mode | PHY | FEC / coding | Upper layers | Clean-room spec | Reference impl (license) | Rust prior art |
|---|---|---|---|---|---|---|
| ACARS POA | AM + MSK 2400 bd, 25 kHz ch, ~16 freqs in 129–137 MHz | none (parity + CRC-16-CCITT) | ARINC 618/620, ARINC 622 via app layer | ARINC 618/620 | acarsdec (LGPL-2/GPL-2 fork) | none usable |
| VDL Mode 2 | D8PSK 10,500 sym/s (31.5 kbps), 25 kHz ch, CSMA, scrambler | RS(255,249), header FEC, CRC | AVLC (HDLC) → ACARS-over-AVLC + ATN (X.25/CLNP/COTP, 8885 XID) | ICAO Doc 9776, Annex 10 Vol III | dumpvdl2 (GPL-3), vdlm2dec (LGPL-2) | none |
| HFDL | USB voice-ch ~2.8 kHz, 1800 Bd B/Q/8PSK → 300–1800 bps, TDMA 32 s frames | K=7 r=1/2 conv + Viterbi, block interleave, scramble | SPDU/MPDU/LPDU → HFNPDU (incl. ACARS), squitters, systable | ARINC 635 | dumphfdl (GPL-3) | none — xng will be first |
| Aero (Inmarsat L/C) | A-BPSK 600/1200 bps, A-QPSK 10.5 kbps; C-band burst variants | K=7 r=1/2 conv + Viterbi, interleave | SU framing → ACARS/Data-2, ADS-C, CPDLC | Inmarsat SDM, ARINC 741/761 | **JAERO (MIT — directly portable)** | none |
| STD-C / EGC | BPSK 1200 sym/s (600 bps), continuous, L-band | K=7 r=1/2 conv + Viterbi, 10368-bit interleave | EGC/SafetyNET packet types | Inmarsat SDM | Scytale-C / SatDump (GPL-3) | none |
| AIS | GMSK 9600 bps, 2×25 kHz ch (161.975/162.025) | none (HDLC + CRC-16) | ITU-R M.1371-5 → NMEA !AIVDM | ITU-R M.1371 (free) | AIS-catcher (GPL-3); **ship162 (MIT, Rust)** | **ship162** — integrate/port |
| ADS-B / Mode S | PPM 1 Mbps @ 1090 MHz, magnitude-domain | CRC-24 w/ limited correction | Mode S / BDS registers | ICAO Annex 10 Vol IV | readsb/dump1090-fa (GPL) | **rs1090/jet1090, adsb_deku (MIT)** — use as dep/reference |
| Iridium (wave 2) | DE-QPSK/BPSK 25 ksym/s bursts across 8.5 MHz @ 1616–1626.5 MHz, ≥10 MS/s | BCH per frame type (reverse-engineered) | IRA/IBC/IDA frames → ACARS/SBD reassembly | community docs (no official spec) | gr-iridium (GPL-3 — clean-room); **iridium-toolkit (BSD-2 — portable parsers)** | none |

DSP difficulty ranking: AIS ≈ ACARS < STD-C < VDL2 ≈ Aero < HFDL < Iridium.

Capture sharing: ACARS+VDL2 share one VHF capture; Aero+STD-C share one L-band
capture (replacing 10–30 parallel JAERO instances — the biggest single UX win);
AIS shares the maritime VHF capture; Iridium needs its own wideband pipeline.

### 3.2 Application layer (the keystone)

libacars (C, **MIT**) covers ARINC 618 parsing + reassembly, ARINC 622 ATS
(FANS-1/A ADS-C, CPDLC with ASN.1 PER), MIAM (incl. DEFLATE), Media Advisory,
OHMA — and is shared by POA/VDL2/HFDL/Aero/Iridium. Because it is MIT, a direct
Rust port (`xng-acars`) is legally clean and serves five modes at once.
Watch: xoolive published an `acars` crate v0.1.0 (MIT, 2026-06-09, "ACARS, VDL2,
ADS-C, CPDLC") — evaluate for reuse/collaboration before porting from scratch.
Airframes' acars-decoder-typescript (MIT) is the reference for label-level
text decoding (ARINC 620 payloads).

### 3.3 Rust ecosystem choices

- **SDR I/O:** the `soapysdr` crate (BSL-1.0/Apache) directly, behind xng's
  own `IqSource` trait (`xng-sdr`) — SoapySDR is mandatory anyway for SDRplay
  (no native API crate; RSPs are the HFDL workhorse). Decode cores never see
  the backend, so `seify` (Apache-2.0) or pure-Rust drivers (`rtl-sdr-rs`,
  MPL-2.0) can slot in later without touching them. Avoid the `seify-rtlsdr`
  GPL-3 fork on crates.io in this permissive project.
- **DSP:** `rustfft` + `realfft` + `num-complex`; borrow kernel patterns from
  `futuredsp` (Apache-2.0). **We write our own:** polyphase filter-bank
  channelizer (no crate exists — ~200 lines on rustfft), soft-decision K=7
  Viterbi (no maintained crate; SIMD branch metrics), per-mode RS error decoders
  and interleavers. `crc` crate covers all CRC variants.
- **Transport:** `tonic` 0.14+ (gRPC org/CNCF) + `prost` for gRPC over HTTP/2;
  `quinn` for QUIC with the same prost-encoded payloads (gRPC-over-HTTP/3 is
  still experimental ecosystem-wide; revisit `tonic-h3` when `h3` stabilizes).
- **TUI:** `ratatui` + `crossterm`. Spectrum via `Chart`, waterfall via colored
  half-block cells, `Canvas` for maps/radar (pattern proven by sdrrat — no
  license, study only — and adsb_deku's radar).
- **Threading:** DSP hot path on dedicated OS threads with `crossbeam-channel`
  / SPSC rings between stages; tokio strictly for control plane and network I/O;
  bridge DSP→async via `tokio::sync::mpsc` `try_send` with drop policy (DSP
  never blocks).

### 3.4 Airframes ingest today (compatibility surface)

Decoder-native JSON, port-per-decoder at feed.airframes.io: acarsdec UDP 5550,
dumpvdl2 UDP 5552 / TCP 5553, vdlm2dec UDP 5555, dumphfdl TCP/UDP 5556, JAERO
C/L UDP 5561/5562/5571, iridium-toolkit TCP 5590, plus the asf-1.0 JSON
envelope (TCP 6000) and a dormant gRPC prototype (port 6001,
airframes-client proto). New Go/NATS stack ingests decoder-native JSON → NATS.
Station ID convention `XX-YYYY-TYPE` moving toward UUIDs (≥36 chars).
Required metadata: accurate timestamps (NTP), freq, channel, signal/noise
levels, error counts, `source.app {name, version}`.

## 4. Architecture

### 4.1 Pipeline

```
                    ┌──────────────────────────────────────────────────────────────┐
                    │                        xng runtime                           │
  SDR A ──seify──►  │  Capture ──► PFB Channelizer ──► ch0..chN (narrowband IQ)    │
  SDR B ──seify──►  │  Capture ──► PFB Channelizer ──► ...                         │
  file/stdin ─────► │  (IQ replay)                                                 │
                    │        │                                                     │
                    │        ▼ per channel                                         │
                    │  DemodCore (mode-specific: MSK/D8PSK/PSK/GMSK/BPSK/PPM)      │
                    │        ▼                                                     │
                    │  FrameCore (AVLC / MPDU-LPDU / SU / HDLC / Mode S ...)       │
                    │        ▼                                                     │
                    │  AppLayer (xng-acars: ACARS, ADS-C, CPDLC, MIAM, ARINC 620)  │
                    │        ▼                                                     │
                    │  Normalized Message (asf-2.0 model) ──► Message Bus          │
                    └───────────────┬──────────────────────────────────────────────┘
                                    │ fan-out (broadcast)
        ┌──────────┬────────────┬───┴─────────┬─────────────┬──────────────┐
        ▼          ▼            ▼             ▼             ▼              ▼
   asf-2.0 gRPC  asf-2.0     legacy JSON   console/log   stats engine   local sinks
   (tonic)       QUIC        compat        pretty/JSON   (logs, files,  (sqlite, ES,
                 (quinn)     (acarsdec/                   Prometheus,   JSONL, sbs)
                             dumpvdl2/                    Web API/TUI)
                             dumphfdl/...)
```

Key properties:

- **Capture sharing.** One SDR capture feeds many channels, and channels of
  *different modes* may share a capture when bands overlap (ACARS + VDL2 in
  136–137 MHz; Aero + STD-C in 1537–1547 MHz). The channelizer is mode-agnostic;
  mode cores subscribe to channel outputs at their required rates.
- **DecoderCore trait.** Every mode implements the same contract:
  `fn spec() -> ModeSpec` (bandwidth, sample rates, default freqs/bands),
  `fn process(&mut self, iq: &[Complex<f32>]) -> Vec<RawFrame>`, plus a
  frame→message stage. Adding a mode = adding a crate that implements the trait.
- **Burst-pipeline variant.** Iridium (wave 2) gets a wideband burst-detector
  front end instead of the fixed channelizer; same trait downstream.
- **Extern wrappers (second-class).** An `ExternDecoder` adapter spawns an
  external client (dumphfdl, dumpvdl2, acarsdec, JAERO...), parses its JSON, and
  injects normalized messages into the same bus — so all outputs/stats/TUI work
  identically. Today's xng HFDL wrapper code is the seed for this module. Not
  encouraged; exists for gap-bridging and validation (A/B against native cores).

### 4.2 Workspace layout

```
xng/
├── Cargo.toml                  # workspace
├── crates/
│   ├── xng-types               # normalized message model, ids, time, units
│   ├── xng-dsp                 # PFB channelizer, filters, NCO, Viterbi, RS, Golay,
│   │                           #   interleavers, scramblers, sync/AGC primitives
│   ├── xng-sdr                 # seify/Soapy capture, device enumeration, IQ replay,
│   │                           #   sample-rate negotiation, multi-capture manager
│   ├── xng-acars               # libacars-rs: ARINC 618/620/622, ADS-C, CPDLC, MIAM
│   ├── xng-mode-acars          # POA demod + frame core
│   ├── xng-mode-vdl2           # D8PSK + AVLC + (AOA, ATN subset)
│   ├── xng-mode-hfdl           # HF PSK + ARINC 635 + systable
│   ├── xng-mode-aero           # Aero L/C-band (JAERO port) + STD-C/EGC
│   ├── xng-mode-ais            # GMSK + M.1371 (ship162 integration/port)
│   ├── xng-mode-adsb           # Mode S/ADS-B (rs1090 as dep or thin native core)
│   ├── xng-proto               # asf-2.0 .proto + prost/tonic codegen
│   ├── xng-outputs             # airframes gRPC/QUIC, legacy JSON compat, ES, files
│   ├── xng-extern              # second-class external client wrappers
│   └── xng-stats               # counters, rates, signal stats, Prometheus, StatsD
├── src/                        # xng binary: CLI, TUI, config, supervisor, web API
├── proto/                      # asf-2.0 protobuf sources
└── docs/
```

Crates that are useful beyond xng (`xng-acars`, `xng-dsp`, `xng-types`,
`xng-proto`) are published to crates.io — they are the permissive building
blocks the ecosystem lacks.

### 4.3 asf-2.0 protocol (sketch — full design doc to follow)

One protobuf schema, two transports:

- **gRPC (tonic, HTTP/2):** `service AirframesFeed { rpc Stream(stream ClientEnvelope) returns (stream ServerEnvelope); rpc Send(MessageBatch) returns (Ack); }`
  — bidirectional stream for feed + acks + server hints (e.g. active HFDL
  frequencies pushed down instead of polled).
- **QUIC (quinn):** same prost messages, length-prefixed, over a lightweight
  session: stream 0 = control/hello/auth, then either one multiplexed message
  stream or one QUIC stream per (sdr, channel) — natural multiplexing, no
  head-of-line blocking, connection migration for flaky feeder links.

Envelope content: station identity (UUID + human ident), app name/version,
SDR + channel provenance, mode, precise timestamps (ns), frequency (Hz, u64),
signal/noise/skew, decode quality (FEC corrections, CRC status), the normalized
message body (typed per mode, raw payload always preserved), and periodic
`StationStats` frames (message rates, decode errors, per-channel SNR, sample
drops, bandwidth). Multiplexing: multiple channels and/or multiple SDRs over a
single connection by default; separate connections remain an option per output
config. Legacy compatibility outputs emit acarsdec/dumpvdl2/dumphfdl/JAERO-
native JSON to the existing ports so current ingests work unchanged.

### 4.4 CLI and TUI

CLI (clap): `xng listen <mode...>` (one or many modes, each with SDR/band/
channel config), `xng scan`, `xng devices`, `xng decode <iq-file>`,
`xng config check/save`, `xng tui`. Config file (TOML) describes stations,
SDRs, mode sessions, and outputs; every TUI-built setup can be saved as config.

TUI (ratatui):
- **Dashboard:** per-session message rates, decode quality, output health.
- **Spectrum/waterfall** per capture with channel overlays (which slices are
  being decoded, per-channel SNR/activity).
- **Message browser:** live tail + scrollback, filter by mode/freq/tail/label,
  raw + decoded views.
- **Channel builder:** interactively add/remove channels on a capture, confirm
  decode in realtime, save as configuration.
- **Scanner:** auto-scanning mode — steps captures across each selected mode's
  known bands/frequency tables, runs lightweight detectors (energy + mode
  signature: MSK/D8PSK/PSK presence, squitter detection for HFDL, RA detection
  for Iridium), scores what is heard, and proposes a decode configuration.
  This is the foundation for Airwaves OS site-survey behavior.
- **Diagnostics:** sample-drop counters, DSP load per stage, device settings.

### 4.5 Statistics

- In-process counters per (sdr, channel, mode, output): messages, frames, FEC
  corrections, CRC failures, signal/noise distributions, bandwidth in/out,
  sample drops, uptime.
- Surfaced as: periodic log lines, local JSON/JSONL file output, Prometheus
  `/metrics` (label conventions compatible with acarshub's
  acars/vdlm/hfdl/imsl/irdm families), optional StatsD (dumpvdl2 parity),
  the Web API, the TUI, and `StationStats` frames in asf-2.0.

### 4.6 Web API

Carry forward the existing actix-web API concept (settings, session control,
stats endpoints) on the new core; the TUI and Web API read the same stats
engine and control the same supervisor.

## 5. Roadmap

Wave 1 is sequenced easiest-first so shared infrastructure hardens before the
hard DSP lands. Each mode core ships with IQ-file regression tests (recorded
captures, golden decoded output) and A/B validation against the reference
decoder via the extern wrapper.

| Milestone | Deliverable |
|---|---|
| **M0 Foundation** | Workspace restructure; `xng-types`, `xng-sdr` (seify/Soapy capture + IQ replay), `xng-dsp` (channelizer, FIR/NCO, CRC); message bus; console + JSONL outputs; CLI skeleton; LICENSE-MIT + LICENSE-APACHE. |
| **M1 First decode: ACARS POA** | MSK demod + ARINC 618 framing; multi-channel from one VHF capture; legacy acarsdec-JSON output; feeds Airframes (UDP 5550). Proves the whole pipeline. |
| **M2 AIS + ADS-B** | ship162 integration/port (GMSK + NMEA out); rs1090-based Mode S core; maritime + 1090 feeds. Cheap wins that exercise multi-mode runtime. |
| **M3 asf-2.0** | Protocol design doc + `proto/`; tonic gRPC + quinn QUIC outputs with multiplexing; server-side ingest stub for the Airframes stack (NATS publisher). |
| **M4 xng-acars (libacars-rs)** | ARINC 618/620/622, ADS-C, CPDLC, MIAM port (MIT source; evaluate xoolive `acars` crate first). Published as a standalone crate. |
| **M5 VDL Mode 2** | D8PSK burst demod, RS(255,249), AVLC, ACARS-over-AVLC (+ ATN XID basics); shares VHF capture with M1; dumpvdl2-JSON compat output. Clean-room from ICAO 9776. |
| **M6 STD-C + Aero** | Viterbi + interleavers in `xng-dsp`; STD-C/EGC core (clean-room); Aero P/R/T channels ported from JAERO (MIT); multi-channel L-band capture replacing parallel JAERO instances; JAERO-JSON compat output. |
| **M7 HFDL** | ARINC 635 clean-room core: HF PSK demod + equalization, deinterleave, Viterbi, SPDU/MPDU/LPDU, systable + Airframes GS API; dumphfdl-JSON compat; A/B against dumphfdl via extern wrapper. |
| **M8 TUI** | ratatui app: dashboard, spectrum/waterfall, message browser, channel builder, config save. |
| **M9 Scanner** | Auto-scan across mode bands with signature detectors; proposed-config output; TUI integration. (Airwaves OS foundation.) |
| **M10 Extern wrappers + parity** | Second-class wrappers (dumphfdl/dumpvdl2/acarsdec/JAERO) for gap-bridging and A/B validation; stats/Prometheus polish; packaging (deb, Docker, ARM builds). |
| **Wave 2** | Iridium burst pipeline (≥10 MS/s, BSD-2 iridium-toolkit parsers, clean-room demod), ATN/CPDLC depth, VDL2 ground-network decoding depth, satdump-adjacent Inmarsat extras, Airwaves OS integration hooks. |

## 6. Risks & watch items

- **HFDL DSP under fading HF channels** is the accuracy-critical core and must
  be clean-roomed (dumphfdl is GPL-3). Mitigation: ARINC 635 is well specified;
  build an IQ corpus early (record now, decode later); A/B harness vs dumphfdl.
- **GPL contamination**: never port from dumphfdl/dumpvdl2/AIS-catcher/
  gr-iridium/readsb/SatDump/Scytale-C. Specs and papers only. Keep a
  PROVENANCE.md per mode crate documenting sources used.
- **Pre-1.0 deps**: seify/futuredsp/FutureSDR APIs move — pin versions.
  tonic master has breaking changes pending. `h3` not ready — QUIC transport is
  custom prost framing, not gRPC-over-HTTP/3, until the ecosystem settles.
- **xoolive `acars` crate** (published 2026-06-09) overlaps `xng-acars` —
  evaluate/contact before building; collaboration may save the M4 port.
- **SDRplay** requires SoapySDR system libs forever (proprietary API) — keep
  Soapy the default backend; document per-vendor setup.
- **Server side**: asf-2.0 needs an ingest in the Airframes Go/NATS stack;
  legacy JSON compat outputs de-risk adoption until that lands.

## 7. Open questions (defaults assumed until revisited)

- **Platforms:** Linux x86_64/aarch64 first-class (incl. RPi), macOS supported
  for dev/TUI; Windows best-effort later.
- **Existing features:** Web API, SQLite state DB, and Elasticsearch output are
  carried forward as optional outputs/services on the new core.
- **ADS-B depth:** start with rs1090 as a dependency for Mode S decode;
  revisit a fully native core only if dependency friction appears.
- **Naming:** protocol name `asf-2.0` (Airframes Standard Format), subject to
  rename when the protocol doc is written.
