# asf-2.0 — Airframes Standard Format, version 2

**Status:** implemented by xng (client + reference ingest); schema
`proto/asf2.proto` is canonical. Per-mode bodies: ACARS,
AIS (incl. field-decode JSON), Mode S (incl. Comm-B), STD-C, HFDL,
Iridium, VDL2 (AVLC link events / XID / ATN), Aero (C-channel
assignments and other non-ACARS structures), plus `undecoded` with
raw bytes always preserved.

One protobuf schema (`proto/asf2.proto`), two transports. Replaces the
port-per-decoder JSON zoo with a single multiplexed, typed, versioned feed
while preserving full raw payloads for server-side re-decoding.

## Goals

- **One connection, everything**: multiple channels, multiple SDRs, and
  multiple modes from one station multiplex over a single connection
  (separate connections remain possible — each connection is independent).
- **Typed but lossless**: normalized per-mode bodies for immediate use,
  plus the raw link-layer payload on every message so the server can
  re-decode with newer logic.
- **Rich provenance**: station UUID + human ident, app, SDR, channel,
  ns-precision timestamps, signal/decode quality on every message.
- **Stats are first-class**: periodic per-channel counters travel in-band.
- **Server hints** (future): the server can push active-frequency maps and
  configuration suggestions down the same stream (the HFDL ground-station
  map case — replacing API polling).

## Wire format

All payloads are protobuf messages from `asf.v2`, wrapped in an `Envelope`
(`oneof`: `Hello`, `MessageBatch`, `StationStats`, `Ack`, `ServerHint`).

### Session flow (both transports)

1. Client sends `Hello` (protocol version, station UUID + ident, app
   name/version, optional auth token).
2. Client streams `MessageBatch` (decoded messages, batch sequence number)
   and periodic `StationStats`.
3. Server may send `Ack {seq}` (flow/health signal, not a delivery
   guarantee) and `ServerHint`.

### Transport A: gRPC (tonic, HTTP/2)

`service AirframesFeed { rpc Stream(stream Envelope) returns (stream Envelope); }`

Standard gRPC ecosystem: load balancers, TLS termination, other-language
clients/servers. This is the path the Airframes Go/NATS stack ingests.

### Transport B: QUIC (quinn)

The same `Envelope` bytes, length-prefixed (`u32` big-endian length, then
the protobuf frame), over one bidirectional QUIC stream (stream opened by
the client; server replies on the same stream). Connection migration and
no head-of-line blocking make this the preferred transport for flaky
feeder links. Per-channel QUIC streams are reserved for a future revision
(`Hello.flags`).

ALPN: `asf2`.

**TLS trust** (mandatory in QUIC): verification is on by default against
system roots. Self-hosted ingests with self-signed certificates are
pinned via `--asf2-quic-ca <pem>` (the reference ingest exports its
certificate with `--quic-cert-out`). `--asf2-quic-insecure` disables
verification entirely, exists for throwaway lab setups, warns loudly,
and is mutually exclusive with `--asf2-quic-ca`.

## Compatibility

- asf-2.0 does not replace legacy feeding: xng keeps emitting
  decoder-native JSON (acarsdec et al.) so existing ingests work
  unchanged. The ingest reference implementation (`xng ingest`) shows the
  server side end-to-end and is the template for the Go-stack ingest
  (Envelope → NATS subject per mode, e.g. `asf2.msg.acars`).
- Protocol versioning: `Hello.protocol_version` (currently 2); unknown
  `Envelope` variants must be ignored, fields are append-only.

## Open items

- Auth: token issuance/validation (Airframes account service) — schema
  field exists, enforcement TBD. Tokens are bearer credentials: they must
  be CSPRNG-generated and only ever sent on a certificate-verified
  connection (never with `--asf2-quic-insecure`).
- Server hints: schema includes the variant; semantics defined per-mode as
  the server side lands.
- Per-channel QUIC streams and batching/compression tuning after real
  deployment measurements.
