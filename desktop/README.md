# xng desktop

A desktop app for xng on macOS, Windows and Linux: pick a radio or an IQ
recording, pick a signal, and watch a live map, message stream, spectrum and
per-channel statistics.

It is a [Tauri 2](https://tauri.app) shell around the `xng` library. Decoding
runs in-process through `xng::engine`, the same decoders, reassembly, filters
and outputs as the CLI, so decode results and throughput match `xng listen`.
The UI is Svelte 5 with MapLibre GL, rendered by the system webview.

## Layout

| Path | What |
|---|---|
| `src-tauri/` | Rust side: commands, the request to `SessionConfig` mapping, and the pump that batches engine output for the UI |
| `src/` | Svelte UI |
| `src/lib/demo.ts` | Simulated backend used only when the UI runs in a plain browser |

`desktop/` is its own Cargo workspace; the root workspace excludes it so the
CLI build never needs a webview toolchain.

## Develop

Prerequisites: Rust, Node 20.19+ or 22.12+, the CLI's native dependencies (SoapySDR,
protoc), and Tauri's system dependencies for your OS
(<https://tauri.app/start/prerequisites/>). On Debian/Ubuntu:

```bash
sudo apt install libsoapysdr-dev protobuf-compiler libwebkit2gtk-4.1-dev \
  libayatana-appindicator3-dev librsvg2-dev libxdo-dev
```

```bash
cd desktop
npm install
npm run tauri dev          # the app, with hot reload
npm run dev                # UI only, in a browser, with simulated data
npm run check              # type-check the UI
(cd src-tauri && cargo test)
npm run tauri build        # installers for the current OS
```

SDR backends are Cargo features, as in the CLI: `soapy` (default), `airspy`,
`airspyhf`. For example `npm run tauri build -- --features airspy,airspyhf`,
or `npm run tauri build -- -- --no-default-features` for a build that only
decodes recordings.

## Installers

The `Desktop` workflow builds, tests and packages the app on every pull
request that touches it, and the `Release` workflow attaches the same
installers to each tagged release:

| Platform | Installer | SDR support |
|---|---|---|
| Debian/Ubuntu x86_64 and arm64 | `.deb` | SoapySDR, native Airspy and Airspy HF+ |
| macOS Apple Silicon | `.dmg` | SoapySDR, native Airspy and Airspy HF+ (`brew install soapysdr airspy airspyhf`) |
| Windows x86_64 | `.exe` setup | IQ recordings only for now |

The app is named XNG everywhere. Its Debian package is `xng-desktop`
(set in `src-tauri/tauri.linux.conf.json`) so it installs next to the CLI's
`xng` package.

macOS builds are signed with the project's Developer ID certificate and
notarized when the `APPLE_*` repository secrets listed at the top of
`.github/workflows/desktop.yml` are set, on master pushes, releases and manual
runs. Without them (and on pull requests) the app gets an ad-hoc signature:
open it once, then allow it under System Settings > Privacy & Security > Open
Anyway. `src-tauri/entitlements.plist` lets the signed app load Homebrew's
SoapySDR and Airspy libraries.

The Windows installer is not code-signed yet: choose "More info" then "Run
anyway" on the SmartScreen prompt.

## How it stays fast

- The engine decodes on its own threads exactly as the CLI does. Nothing in
  the UI path can back-pressure it: the message bus drops for slow readers.
- One pump thread drains the bus and sends the UI batched updates at fixed
  rates (messages ~8/s, spectrum and channel levels 10/s, map entities 1/s),
  so a burst of thousands of messages costs a handful of IPC calls.
- The message list is virtualized and capped at 5,000 rows; spectrum and
  waterfall draw straight to canvas outside the reactive graph.

## Everything the CLI can do

The Advanced panel maps one-to-one onto `xng listen` / `xng station` session
options: sample rate, center, channels, gain, receiver position, demod
effort, max ppm, decode threads, ACARS label and AIS filters, station ID,
Airframes feeding, asf-2.0 gRPC/QUIC, UDP, MQTT, ZMQ, JSONL, the web
dashboard, Prometheus, SBS, Beast, NMEA TCP/UDP, GSMTAP, the Iridium
satellite map, ground-station names and the aircraft database.

Not in the app yet: multi-session stations, `scan`, `survey`, `ingest` and
`extern`. Use the CLI for those.
