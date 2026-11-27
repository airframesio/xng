# Meshtastic and MeshCore (LoRa mesh)

xng has a native LoRa receiver (`crates/xng-mode-lora`) with two packet
decoders on top: Meshtastic (sync word 0x2B) and MeshCore (sync word 0x12).
The sync word picks the decoder, so one channel can hear both networks if
they share radio settings.

## Usage

```sh
# Meshtastic LongFast in the US (default preset, 906.875 MHz)
xng listen --sdr driver=rtlsdr --mode meshtastic -r 1000000 -c 906.875M \
    --channels 906.875 --http 0.0.0.0:8080

# MeshCore, US suggested settings (910.525 MHz, 62.5 kHz, SF7)
xng listen --sdr driver=rtlsdr --mode meshcore -r 1000000 -c 910.525M \
    --channels 910.525

```

`xng scan` includes both modes (Meshtastic at 906.875 MHz, MeshCore at
910.525 MHz).

### Options

| Option | Meaning |
| --- | --- |
| `--lora-preset NAME` | Meshtastic `LongFast`, `LongSlow`, `LongModerate`, `LongTurbo`, `MediumSlow`, `MediumFast`, `ShortSlow`, `ShortFast`, `ShortTurbo`, or `meshcore-us`, `meshcore-eu`, `meshcore-anz` |
| `--lora-sf 7,8` | Spreading factor(s) to listen for. Each one adds a demodulator |
| `--lora-bw 125` | Bandwidth in kHz |
| `--meshtastic-key NAME=BASE64` | Extra Meshtastic channel to decrypt (repeatable) |
| `--meshcore-channel '#name'` or `NAME=BASE64` | Extra MeshCore group channel (repeatable) |

Defaults: Meshtastic uses LongFast (SF11, 250 kHz). MeshCore uses 62.5 kHz
at SF7, and listens on SF7 and SF8 in the 863 to 870 MHz band where both
are in use. The coding rate and payload length come from the LoRa header,
so they need no setting. Low data rate optimization turns on by itself
when a symbol lasts longer than 16 ms.

## What is decoded

**Meshtastic**: the 16-byte header (to, from, packet id, hop limit and hop
start, want ack, via MQTT, channel hash, next hop, relay node), then the
payload is decrypted with the default key on every preset's channel name
plus any `--meshtastic-key`. Decoded ports: text messages, positions, node
info (long and short name, hardware model, role), routing (ack, nak,
errors), waypoints, range test, telemetry (device, environment, power, air
quality and local stats), traceroute, neighbor info and map reports.
Other ports are reported by name with their bytes. Packets on channels
without a known key are reported as `encrypted`, and PKI direct messages
(channel hash 0) as `direct_encrypted`.

**MeshCore**: the header (route type, payload type, version), transport
codes and path. Adverts give the node's public key, name, type (chat,
repeater, room, sensor) and position, and their Ed25519 signature is
checked. Group text on the Public channel and any `--meshcore-channel` is
decrypted after its MAC checks. Direct messages, requests and responses
are encrypted per peer and are reported with their source and
destination hashes. Path, ack and trace packets are decoded too.

Only public keys are built in. Private channels stay encrypted unless you
add their key.

## Outputs

Messages carry `mode: meshtastic` or `meshcore` with a `kind` and
`details`, plus a `lora` object (SF, bandwidth, CR, sync word, preset, SNR,
carrier offset). They go to every output except the Airframes feed, which
does not ingest mesh traffic. The web dashboard has a Mesh tab listing
nodes (name, hardware, battery, SNR, hops, last text) and a Mesh nodes
map layer: green for Meshtastic, pink for MeshCore.

## Verification

- LoRa coding: whitening, Hamming, interleaver and header checked against
  the published gr-lora_sdr tables; every SF/CR/LDRO combination
  round-trips.
- Demodulator: synthetic IQ with carrier offset, clock drift and noise.
  SF7 to SF12 decode cleanly, SF11 down to −12 dB SNR.
- Meshtastic: LongFast channel hash 8 and the US LongFast frequency
  906.875 MHz match published values.
- MeshCore: Public channel hash 0x11 matches the published value.
- No real-RF capture has been checked yet.

![Mesh tab and map layer](../screenshots/mesh-dashboard.png?raw=true)
