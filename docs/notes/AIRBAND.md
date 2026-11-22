# Airband voice — implementation notes

`--mode airband` (aliases `voice`, `atc`) receives AM airband and NFM voice
channels. Unlike the data modes its product is audio, cut into
transmissions by a squelch. Code: `crates/xng-mode-airband` (DSP, squelch,
scanner logic, Ogg/WAV framing) and `src/airband` plus `src/outputs/airband_*`
and `src/outputs/atc_feed.rs` (runtime, outputs, ATC feeder). Clean-room:
RTLSDR-Airband and airband-hub (both GPL) were read for the feature list
only; see `crates/xng-mode-airband/PROVENANCE.md`.

**Status:** end to end on synthetic IQ (unit and integration tests), feeding
verified against `aggregator-atc --dev` over QUIC and TLS/TCP. No off-air
airband capture is vendored yet.

## Signal chain (per channel)

```
capture IQ ─► front end ─► 16 kHz channel ─► AFC ─► AM envelope + AGC ─► 150 Hz HPF ─► 3.4 kHz LPF ─► notch ─► gain
               (shared)                        │      or NFM discriminator + de-emphasis
                                               └─► 10 ms block power ─► squelch (+ CTCSS gate) ─► events
```

- **Front end.** One channel: a plain DDC. Several channels: xng-dsp's
  polyphase FFT channelizer (`ChannelizedDdc`), or the shared-decimation
  front (`SharedDdc`) when the channel layout does not fit the FFT grid. The
  front keeps ±7 kHz around each channel; a 95-tap FIR per channel then sets
  the selectivity (default 8 kHz wide for AM, 12.5 kHz for NFM). A narrower
  front let a strong channel 8.33 kHz away fold into its neighbour.
- **AM.** Envelope minus a carrier estimate, divided by the carrier (AGC), so
  weak and strong stations play at the same loudness. The carrier follower
  has a 50 ms time constant plus a fast attack, so a key-up is followed within
  a few ms. The first 20 ms of each transmission fade in to remove the
  key-up thump. `agc = false` gives fixed gain instead.
- **NFM.** Phase discriminator scaled so `nfm-deviation-hz` (2.5 kHz) is full
  scale, then first-order de-emphasis (`tau-us`, 200 µs; 0 disables).
- **AFC** (`afc-max-hz`, off by default). Tracks the carrier offset only on
  blocks that carry signal (above the threshold and within 2 dB of the
  transmission's mean), slew-limited to 25 Hz per block, so noise at the end
  of a transmission cannot drag it away.
- **CTCSS** (`ctcss-hz`, NFM). Goertzel bank over the EIA/TIA-603 tone list;
  the gate opens only when the configured tone dominates its neighbours.

## Squelch

Measured on 10 ms blocks of channel power.

- `auto[:snr_db]` (default `auto:9`): opens when a block is `snr_db` above the
  tracked noise floor. The floor falls quickly toward quieter blocks and
  rises slowly (5 s time constant while closed, 60 s while open), so a long
  transmission does not raise its own threshold and a stuck carrier
  eventually closes.
- `manual:<dBFS>`: a fixed open level.
- `off`: always open (continuous audio).

Close threshold = open threshold − 3 dB (hysteresis). `open-ms` (20 ms) is
how long a signal must last before opening; the blocks that waited are kept
as pre-roll so the first syllable is not lost. `hang-ms` (250 ms) keeps the
squelch open over short gaps.

## Streams and the audio hub

Audio does not go on the message bus. Each transmission puts a `tx_start` and
a `tx_end` message on the bus (`MessageBody::Airband`, with label, level,
noise, duration and ATC frequency key) so console, JSONL, MQTT, ZMQ and
asf-2.0 see activity. The audio itself goes on a process-wide hub
(`src/airband/hub.rs`) as these streams:

- **channel** streams, one per frequency;
- **mix**: every channel of a session summed (`--mix`);
- **scan**: follows one channel at a time (`--scan`). Priority channels
  (`--scan-priority`, highest first) preempt lower ones unless `preempt =
  false`; after a transmission the scanner stays `--scan-hold-ms` (2 s) on that
  channel for the reply; channels can be locked out (`--scan-lockout`) or the
  scanner pinned to one channel, live from the dashboard. The scan stream
  spans every session, so several SDRs scan as one receiver.

Timestamps are capture time (sample clock anchored to UTC, re-anchored if it
drifts more than 1.5 s behind the wall clock).

## Outputs

| Output | Flag / TOML | Notes |
|---|---|---|
| Recordings | `--record-dir`, `[outputs.airband] record-dir` | `<dir>/<label_MHz>/<date>/<HHMMSS>_<MHz>.wav` plus a JSON sidecar per transmission; `--record-format ogg` for Opus; `--record-min-ms` drops key-clicks, `--record-max-s` splits long ones; `--record-continuous` writes one file per channel per hour with silence between transmissions; `--record-streams` picks channels, `mix` or `scan` |
| UDP PCM | `--audio-udp host:port[,stream=..][,raw]` | 16 kHz s16le; a 24-byte `XNGA` header (flags, frequency, capture time) unless `raw` |
| Icecast | `--icecast icecast://user:pass@host:port/mount?stream=scan&bitrate=32000&name=..` | continuous Ogg/Opus with silence between transmissions; defaults to `scan` when the scanner is on, else `mix` |
| Dashboard | `--http` | `/airband`: levels, squelch, scanner controls, history, listen in the browser |
| Prometheus | `--metrics` | `xng_airband_{level,noise,squelch}_dbfs`, `xng_airband_open`, `xng_airband_afc_hz`, `xng_airband_transmissions_total`, `xng_airband_airtime_seconds_total` per channel |
| Airframes ATC | `--atc-feed`, `[outputs.airband.atc]` | below |
| Transcripts | `--transcribe`, `--whisper-model`, `--whisper-url`, `[outputs.airband.transcribe]` | Whisper speech-to-text; below |

Opus needs the default `opus` feature (libopus, or the bundled build).

## Airframes ATC feeder

Implements aggregator-atc protocol v0.2 (`docs/PROTOCOL.md` §4 in
airframesio/aggregator-atc): ALPN `airframes-atc/1`, port 4433.

- **Transport**: QUIC first (control stream + AUDIO as datagrams); after two
  failed QUIC attempts in `auto` it falls back to TLS 1.3 over TCP. Reconnects
  with backoff from 1 s to 60 s plus jitter.
- **Session**: HELLO (token, feeder id, station name and position rounded to
  `location-precision-m`, and `hide_username` when `--atc-hide-username` /
  `hide-username` is set) → WELCOME → one OPEN per fed stream → OPENED. A
  channel stream is `mode: single`; the scan stream is `mode: scan` with up to
  16 frequencies. A channel with `atc-frequency` sends that key; otherwise the
  server resolves `freq_hz` from the station position, and a rejection lists
  candidate keys in the log.
- **Per transmission**: SQUELCH open (signal and noise dBFS), Opus AUDIO
  packets (16 kHz mono, 20 ms, default 24 kbit/s) with TX_START on the first
  and TX_END on the last, then SQUELCH close. Audio is squelch-gated, so a
  quiet channel sends nothing. STATS every 10 s, PING every heartbeat.
- **Sign-in**: `xng atc login` runs the device flow (`/auth/device/code`,
  then polling `/auth/token`) or `--email` with a password (`/auth/login`),
  client id `airframes-feeder`. `~/.config/xng/atc.json` keeps the API URL,
  username, user id, feeder id and the rotating refresh token; the refresh
  token is the only credential stored (the file is mode 0600 on Unix; on other
  systems it relies on the user profile's permissions). Access tokens (1 h) are
  minted before each connect and replaced in-session with REAUTH. `xng atc
  status` checks the sign-in; `xng atc logout` forgets it and keeps the feeder
  id. `--atc-token dev:<user>` with `--atc-insecure` talks to a local
  `aggregator-atc --dev`.
- **Transcripts**: with transcription on, each OPEN carries
  `"transcript": {"model", "lang"}`, and each transcribed transmission is
  followed by a TRANSCRIPT frame (`ch`, `tx`, `final`, `text`, `start_us`,
  `end_us`, `model`, `confidence`). On a scan feed `tx` is the scan
  transmission that carried the channel at the time. Which engine made the
  transcripts is declared in each OPEN's `transcript` object (`engine`,
  `engine_version`, `model`, `lang`, e.g. `{"engine": "sherpa-onnx",
  "engine_version": "2.0.2", "model": "parakeet-110m", "lang": "en"}`); a
  TRANSCRIPT repeats `engine`, `engine_version` or `lang` only when they
  differ from what its feed's OPEN said.
- The mix stream cannot be fed: listeners need to know which frequency is
  active.

## Transcription

`--transcribe` turns on speech-to-text for every channel (or the ones in
`--whisper-streams`). Each transmission's audio is collected from squelch
open to close and handed to the engine once it ends; transmissions longer
than `max-s` (28 s) go in pieces. One worker runs the engine, oldest first;
when it falls behind, the oldest waiting transmissions are dropped (queue of
16), and the log says so with a hint to pick a smaller model.

Engines (all opt-in; release binaries and Docker images include none of the
local ones):

- **sherpa-onnx** (`sherpa` build feature: `cargo build --release --features
  sherpa`). Moonshine, NVIDIA Parakeet and SenseVoice on ONNX Runtime. These
  models cost compute in proportion to the audio, where Whisper pads every
  transmission to a 30 s window, so a 4 s call is several times cheaper. The
  build links a prebuilt static sherpa-onnx library downloaded from its
  GitHub releases (Linux x86_64/aarch64, macOS, Windows; no 32-bit ARM). To
  build offline, put the archive in a folder and set
  `SHERPA_ONNX_ARCHIVE_DIR`, or point `SHERPA_ONNX_LIB_DIR` at an unpacked
  library. Models (`--transcribe-model`, downloaded once into
  `<model cache>/sherpa`; `XNG_SHERPA_MODELS_URL` and `XNG_HF_URL` point at
  mirrors of the GitHub release and Hugging Face), or a directory holding an
  exported sherpa-onnx model:

  | Model | Download | Languages | Notes |
  |---|---|---|---|
  | `skycontrol-atc-110m` (default) | 137 MB | English ATC | Parakeet-110m fine-tuned on ATC speech; far the best on real ATC audio, same CPU as parakeet-110m |
  | `moonshine-tiny` | 108 MB | English | lowest CPU, Raspberry Pi 3/4 |
  | `moonshine-base` | 250 MB | English | |
  | `moonshine-tiny-v2`, `moonshine-base-v2` | 40, 125 MB | English | Moonshine v2, smaller downloads |
  | `parakeet-110m` | 108 MB | English | general-purpose English |
  | `parakeet-tdt-0.6b-v2` | 600 MB | English | most accurate, 4x the CPU of parakeet-110m |
  | `parakeet-tdt-0.6b-v3` | 640 MB | 25 European languages | |
  | `sense-voice` | 160 MB | zh, en, ja, ko, yue | |

  `--whisper-threads` sets CPU threads (default half the cores, 1 to 4).
  These models take no text prompt. `skycontrol-atc-110m` is
  [srinath-exe/skycontrol-atc-asr-110m](https://huggingface.co/srinath-exe/skycontrol-atc-asr-110m)
  (CC-BY-4.0, based on NVIDIA's `parakeet-tdt_ctc-110m`), fetched from a
  pinned commit and checked by sha256. With `--transcribe` and no model, a
  build with the `sherpa` feature uses it; otherwise whisper.cpp `base.en`.
- **Local whisper.cpp** (`whisper` build feature: `cargo build --release
  --features whisper`, which compiles whisper.cpp and needs cmake and
  libclang). `--transcribe-model` (alias `--whisper-model`) is a ggml model
  file or a name (`tiny.en`, `base.en` default, `small.en`, `medium.en`,
  `large-v3-turbo`, with an optional `-q5_1`-style suffix), downloaded once
  from the whisper.cpp model repository into `$XNG_WHISPER_MODELS`, else
  `$XDG_CACHE_HOME/xng/whisper`, else `~/.cache/xng/whisper`.
  `--whisper-threads` sets CPU threads (default: all cores but one, up to
  8). GPU builds: `--features whisper-metal`, `whisper-cuda` or
  `whisper-vulkan`.
- **Whisper HTTP server** (`--whisper-url`): a multipart WAV upload with
  `response_format=verbose_json`. A bare `http://host:port` gets
  `/v1/audio/transcriptions` appended (OpenAI, faster-whisper servers,
  LocalAI, whisper.cpp `server --inference-path /v1/audio/transcriptions`);
  a URL with a path is used as is (whisper.cpp's own `/inference`). The key,
  if any, comes from `XNG_WHISPER_API_KEY` or `api-key` in TOML. `model` is
  the name sent to the server (default `whisper-1`). `--transcribe-engine`
  names what the server runs for Airframes ATC (`faster-whisper`, ...;
  default `openai-api`).

The engine is picked from the model name (a sherpa-onnx name or a directory
means sherpa-onnx, another name whisper.cpp; no name means sherpa-onnx when
it is built in); `--transcribe-engine
whisper.cpp|sherpa-onnx` forces it.

### Choosing a model

**Real ATC audio.** Measured on an Apple M2 Max, one thread, on 871
transmissions from the ATCO2 1-hour test set, 400 from the UWB-ATCC test
set, and 75 unlabeled LiveATC transmissions (LAX tower, JFK departure). The
audio, manifests, scripts and full side-by-side transcripts are in the
private `airframesio/xng-corpus` repository under `atc-asr/` (the licences
do not allow publishing it). CPU is CPU seconds per second of audio.

| Model | WER ATCO2 | WER UWB | CPU s/s | RSS |
|---|---|---|---|---|
| **skycontrol-atc-110m** | 25.4% | **19.9%** | 0.045 | 793 MB |
| Whisper base.en ATC (TU Delft, CTranslate2 int8) | 19.3%\* | 51.2% | 0.61 | 728 MB |
| wav2vec2 XLS-R 300M ATC (Idiap, no LM) | 45.6% | 24.4%\* | 0.18 | 3.6 GB |
| Whisper tiny.en ATC (TU Delft) | 63.0%\* | 80.6% | 0.66 | 456 MB |
| parakeet-tdt-0.6b-v2 | 55.0% | 71.3% | 0.12 | 1.6 GB |
| parakeet-110m | 62.3% | 81.9% | 0.044 | 794 MB |
| sense-voice | 62.6% | 69.8% | 0.082 | 677 MB |
| whisper.cpp base.en with xng's ATC prompt | 66.3% | 79.0% | 0.46 | 496 MB |
| Whisper base.en | 73.8% | 84.4% | 0.21 | 700 MB |
| moonshine-base | 77.3% | 88.9% | 0.067 | 762 MB |
| moonshine-tiny | 88.3% | 98.9% | 0.037 | 594 MB |

\* trained on that corpus, so optimistic. SkyControl's training data is not
published; its spellings suggest some ATCO2 overlap, so UWB and the US
LiveATC audio are the fairer tests for it. On JFK departure it gave "delta
one fifteen heavy turn left heading one three zero" where whisper.cpp
base.en gave "Delta 115 heavy turn left to re-zero." It was trained on
European audio and sometimes reaches for European words on US callsigns
("praha" for a JFK callsign). The Whisper ATC fine-tunes cost 10 to 15
times its CPU; small.en and medium.en ATC fine-tunes run slower than real
time on one core.

**Synthetic speech.** Earlier, in the cloud sandbox (x86_64, one thread) on 120 clips of ATC
phrases (20 phrases, 2 synthetic voices, read at controller pace) in three
conditions: clean; "radio" (300 to 2700 Hz, AGC clipping, 12 dB SNR); and
"weak" (4 dB SNR). Word error rate after normalising numbers; CPU is CPU
seconds per second of audio. Whisper here is sherpa-onnx's Whisper export,
which takes no prompt; xng's whisper.cpp engine adds an ATC prompt, which
should help it somewhat. These are synthetic voices, not off-air audio, so
read the ranking more than the numbers.

| Model | WER clean | WER radio | WER weak | CPU s/s | RSS |
|---|---|---|---|---|---|
| moonshine-tiny | 21.1% | 30.9% | 68.3% | 0.023 | 242 MB |
| moonshine-tiny-v2 | 19.5% | 36.5% | 82.7% | 0.024 | 175 MB |
| moonshine-base | 18.3% | 23.3% | 60.8% | 0.048 | 423 MB |
| moonshine-base-v2 | 21.7% | 23.7% | 55.2% | 0.046 | 384 MB |
| **parakeet-110m** | 15.3% | 21.5% | 45.2% | 0.042 | 333 MB |
| parakeet-tdt-0.6b-v2 | 7.4% | 11.4% | 27.9% | 0.164 | 938 MB |
| sense-voice | 10.8% | 21.1% | 34.1% | 0.087 | 361 MB |
| Whisper tiny.en | 15.3% | 28.3% | 68.7% | 0.099 | 317 MB |
| Whisper base.en | 12.4% | 23.1% | 44.2% | 0.165 | 471 MB |

So: `parakeet-110m` matches Whisper base.en at a quarter of its CPU, and
`parakeet-tdt-0.6b-v2` halves Whisper base.en's errors for the same CPU.
`moonshine-tiny` is the floor for weak hardware. A Raspberry Pi 4 core is
roughly four to six times slower than the sandbox's.

### ATC-tuned Whisper models

`skycontrol-atc-110m` (above) is the ATC model xng loads directly. The
Whisper alternatives are fine-tunes on public ATC corpora (ATCO2, UWB-ATCC), all MIT
licensed:

- `jacktol/whisper-medium.en-fine-tuned-for-ATC` (WER 15% on its ATC test
  set against 95% for stock medium.en). A faster-whisper (CTranslate2) build
  is `jacktol/whisper-medium.en-fine-tuned-for-ATC-faster-whisper`: serve
  it with a faster-whisper server and use `--whisper-url http://host:8000
  --transcribe-engine faster-whisper --transcribe-model <name>`. For
  whisper.cpp, convert the Hugging Face checkpoint with whisper.cpp's
  `models/convert-h5-to-ggml.py` and pass the file to `--transcribe-model`.
- `jacktol/whisper-large-v3-finetuned-for-ATC` (self-reported WER 6.5%),
  Transformers weights; a GPU-sized model.
- `thomaseibner/whisper-large-v3-turbo-us-atc-v3`, a LoRA adapter trained
  on Minneapolis (KMSP) audio.

These run at Whisper medium/large cost, so they suit a GPU box serving
several feeders rather than the receiver itself.

Whisper (whisper.cpp and servers) gets a prompt ahead of each transmission: the channel label and ATC
phraseology hints (`--whisper-prompt` replaces the hints). Output is cleaned
before use: segments Whisper itself rates as probably not speech are
dropped, bracketed noise marks (`[BLANK_AUDIO]`, `(static)`) go, phrases
Whisper invents on static ("Thank you.", "Thanks for watching") are
discarded, and looping repeats are cut to three.

Where transcripts go:

- the message bus as an airband message with `kind = "transcript"`
  (`details`: `tx`, `label`, `start`, `end`, `text`, `engine`,
  `engine_version`, `model`, `language`, `confidence`, `atc_frequency`), so console, JSONL, MQTT, ZMQ and asf-2.0
  get them;
- the dashboard's transmission history;
- recordings: the transmission's JSON sidecar gains `transcript`,
  `transcript_engine` and `transcript_model` (split mode), or a line in `<hour file>.transcripts.jsonl` (continuous mode);
- the Airframes ATC feed (TRANSCRIPT frames, above).

## Configuration

CLI flags are listed under "Airband (--mode airband)" in `xng decode --help`
and `xng listen --help`. Station files: `[session.airband]` (`mix`,
`[session.airband.defaults]`, `[[session.airband.channel]]` tables with
`freq`, `label`, `atc-frequency`, `key` and any per-channel override) and
`[outputs.airband]` (recordings, `udp`, `icecast`, `[outputs.airband.scan]`,
`[outputs.airband.atc]`, `[outputs.airband.transcribe]`). An airband session may list its channels only as
tables. See `contrib/station.airband.example.toml`.

## Not done yet

- Retune scanning (rtl_airband's scan mode: hopping one narrow SDR across
  frequencies spread wider than its bandwidth). xng covers a band by
  channelizing one wide capture instead; hopping needs retune support in
  the SDR sources.
- An ARTCC (en-route centre) recording in the ATC speech corpus; the
  corpus has tower and departure audio only.
- US-trained ATC model: SkyControl was trained on European audio; a
  Parakeet-110m fine-tune on US audio (or Parakeet hotwords, below) should
  fix its European guesses on US callsigns.
- Hotwords for Parakeet (sherpa-onnx can bias a transducer towards a word
  list, e.g. the local airport's callsigns and fixes).
