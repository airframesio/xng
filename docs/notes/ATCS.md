# ATCS (Advanced Train Control System) — implementation notes

Native decode core for the North-American rail data radio (AAR
Specification 200) in `crates/xng-mode-atcs`. ATCS links a dispatch
office / ground network to wayside field equipment (MCPs) over a pair of
900 MHz channels at 4800 bps FSK. The RF link carries a synchronous
**HDLC-LAPB** bit stream; inside each HDLC frame is a **Spec-200**
(X.25-style) Layer-3 packet whose header carries the source/destination
ATCS addresses, a priority/ARQ control field, and the message routing.
Clean-room: every protocol fact is typed from the AAR Standard Manual of
ATCS, the Signal Identification Wiki ATCS page, and the ATCS Monitor
address documentation — **no decoder code was ported or copied** (see
`PROVENANCE.md`).

This crate delivers the **decode layer only** — bits/bytes → structured
fields. Two follow-ups are out of scope and explicitly deferred: the IQ →
bits FSK front end, and the vendor codeline payload protocols (Genisys /
ARES) carried inside the user data. xng surfaces the raw payload bytes
and stops at the Spec-200 header.

## Status: DECODE-CORE (not wired to `--mode`)

This is a verified, externally-anchored decode core. It is **not yet
integrated** into the runtime: `xng-mode-atcs` is not a workspace
dependency of the CLI (absent from the `crates/*` dependency block in the
root `Cargo.toml`), there is no `Mode::Atcs`, no `--mode atcs`, no scan
plan, no `to_message` mapping, and no `xng_types::Message` variant. The
decode layer ships standalone (consumed via its public API
`HdlcDeframer` / `decode_frame` / `decode_packet`) and is fenced by its
own tests; nothing feeds it live samples yet.

## Pipeline

NRZI-decoded link bits → `frame::HdlcDeframer` (flag hunt, bit
destuffing, FCS = CRC-16/X-25 check) → raw frame bytes → `spec200::
decode_packet` (control octet → priority / ARQ / service flags; BCD
address-length octet; source + destination addresses via `address`) →
`Spec200Packet { control fields, source, destination, direction,
user_data }`. The `decode_frame` convenience in `lib.rs` chains frame →
packet.

The **input is NRZI-decoded bits**, not IQ — the FSK discriminator,
NRZI decode, and bit-sync on the 40-alternating-bit preamble are the
deferred front end (see below).

## Layer 2 — HDLC / LAPB deframing (`frame.rs`)

ISO/IEC 3309 / 13239 synchronous HDLC, as the AAR standard specifies for
the Spec-200 RF link. The streaming `HdlcDeframer` consumes one link bit
at a time:

| Element | Value / behaviour |
|---|---|
| Flag | `0x7E` (`0 1 1 1 1 1 1 0`), hunted via a rolling 8-bit window |
| Octet bit order | **LSB-first** on the wire (bit `i` of a byte = `i`-th bit to arrive) — matches the AIS/VDL2 HDLC layers and the FCS in `xng_dsp::checksum` |
| Bit destuffing | a `0` following five consecutive `1`s is dropped |
| Closing flag | six ones + the flag's leading zero are stripped (7 bits) from `buf`; the same flag may open the next frame (shared-flag back-to-back frames recovered) |
| Abort | seven+ consecutive ones aborts collection, resumes flag hunting |
| FCS | 16-bit, **CRC-16/X-25** (poly 0x1021, reflected, init 0xFFFF, xorout 0xFFFF), via `xng_dsp::checksum::hdlc_frame_ok` |
| Length gate | destuffed frame must be a whole number of octets, `MIN_BITS`=24 floor, `MAX_BITS`=4096 cap (lets the FCS reject garbage) |

A completed frame yields `AtcsFrame { bytes (FCS stripped), fcs }`; only
CRC-valid frames are emitted. `hdlc_bits` is a transmit-order framing
helper (opening flag, stuffed payload + FCS, closing flag) used by the
tests and any future modulator — it is **not** a self-consistency oracle
(see Validation).

## Layer 3 — Spec-200 packet header (`spec200.rs`)

The Layer-3 packet header decoded from the HDLC frame bytes:

```text
Octet 1  control:  Q D 1 0 P P P A   (MSB -> LSB)
    Q   service-signal indicator      (bit 7; 0 on originate traffic)
    D   network-service-signal confirmation request (bit 6)
    10  fixed bits 5..4               (sanity flag: control_well_formed)
    PPP 3-bit priority level          (bits 3..1)
    A   ARQ-disable bit               (bit 0; 1 disables auto repeat)
Octets 2..4  reserved, zero on origination (not interpreted)
Octet 5  address length:
    upper nibble = source addr length      (count of BCD digits)
    lower nibble = destination addr length (count of BCD digits)
Octet 6.. source addr then destination addr, BCD-packed (two digits per
          octet, high nibble first), then user data.
```

`decode_packet` requires ≥5 octets (control + 3 reserved + addr-len),
rejects a zero source/destination length, reads each address as BCD
(`read_bcd`: two digits/octet high-nibble-first, odd counts take only the
high nibble of the final octet, any nibble >9 fails the whole decode),
and returns the trailing bytes as raw `user_data`. The decoded
`Spec200Packet` carries `control` (verbatim), `service_signal`,
`confirm_request`, `priority`, `arq_disabled`, `control_well_formed`,
`source`, `destination`, a derived `direction` summary, and `user_data`
(serialized as a lowercase-hex string). The whole struct is `Serialize`
for downstream output.

**Direction** is derived from the two address types: `ground-to-field`
(ground source → field destination), `field-to-ground`, else `other`.

## ATCS address decode (`address.rs`)

An ATCS address is a BCD digit string. First digit = user-group /
direction; next three = AAR-assigned railroad number; the rest is
railroad-internal routing.

| First digit | `AddressType` | Label |
|---|---|---|
| 0 | `Network` | network (ground network) |
| 1 | `Locomotive` | locomotive |
| 2 | `Host` | host/office (dispatch) |
| 3 | `WaysideWireline` | wayside-wireline |
| 4 | `OtherMobile` | other-mobile |
| 5 | `WaysideRf` | wayside-rf/mcp (field MCP) |
| 6–9 | `Other(d)` | other (reserved / railroad-specific) |

`is_ground()` = Network or Host; `is_field()` = WaysideWireline or
WaysideRf. `AtcsAddress::parse` exposes the verbatim `digits`, the
`addr_type`, the `railroad` number (digits 2..4), the verbatim `routing`
remainder, and a **best-effort** `line` / `node` split for the two
documented address-type formats only:

- **Type 5 MCP** `T-RRR-XX-AAAA` (6 routing digits) → 2-digit line + 4-digit node.
- **Type 7 random-access** `T-RRR-CC-AAA` (6 routing digits) → 3-digit codeline + 3-digit node.

Any other type/length leaves `line`/`node` as `None` — the line/node
split is railroad- and type-specific, so it is **not fabricated** for
undocumented formats; the raw routing string is always preserved.

## Sourcing / oracles

This crate verifies against external references, never an
encode→decode self-loopback. No raw ATCS HDLC bit capture (IQ or on-air
bytes) is published, so the framer is anchored to a public CRC catalogue
value and the packet/address decode is anchored to a published
worked example.

| Fact | Oracle | How verified |
|---|---|---|
| HDLC FCS = CRC-16/X-25 | Public CRC catalogue (X-25 / IBM-SDLC) | `frame.rs::fcs_matches_x25_catalogue_value`: `hdlc_fcs("123456789") == 0x906E`, the catalogue check value — anchors the FCS to an external constant, not our encoder |
| Flag / stuffing / abort / shared-flag deframing | ISO/IEC 3309 / 13239 HDLC | Catalogue-string frame with its **externally fixed** FCS (0x906E) carried through flag-hunt + destuffing; long-1-run stuffing, bad-FCS rejection, shared-flag, and 7-ones abort all asserted |
| 4800 bps FSK, 900 MHz, 12.5 kHz channels, HDLC-LAPB | AAR Standard Manual of ATCS (Spec-200); Signal ID Wiki ATCS page (independent confirmation) | Documented; not exercised at runtime (no demod) |
| Spec-200 control / addr-length / BCD header layout | AAR Standard Manual of ATCS | Spec-derived packet decoded against the documented field semantics |
| Address user-group digit table | AAR Standard Manual of ATCS; ATCS Monitor (atcsmon.com) | `address.rs::address_type_digit_table` asserts every digit→type |
| Worked sample addresses + railroad number | sigidwiki.com decoded Spec-200 packet | Source `5125013826` → WaysideRf / railroad 125; destination `2125385538` → Host / railroad 125; both asserted in `address.rs` and the end-to-end test |
| Type-5 / type-7 line+node split | ATCS Monitor documented formats | Asserted on the sigidwiki sample (type 5: XX=01, AAAA=3826) and a type-7 example |

**Spec-derived end-to-end** (`tests/end_to_end.rs`): a Spec-200 packet
assembled byte-for-byte from the AAR header layout (control 0x24, three
reserved octets, BCD address-length 0xAA, BCD source `5125013826` + dest
`2125385538`, opaque user data), wrapped in a real HDLC frame with a
genuine CRC-16/X-25 FCS, is run through `HdlcDeframer` → `decode_frame`
and every recovered field (priority 2, ARQ enabled, both addresses,
railroad 125, `field-to-ground`, raw payload) is asserted against the
standard's definitions and the sigidwiki sample. This is documented as
**spec-derived**, not a loopback: the bytes are laid by hand from the
standard, and the decode is checked against the standard plus the
external worked example — never against a modulator in this crate.
Additional cases cover an FCS-rejected corrupted frame and a frame buried
in idle/noise with a stray flag and bit-sync-like preamble.

## Limitations / deferred

- **No IQ demodulator (deferred, marked TODO in `lib.rs`).** The
  FSK-discriminator front end (DDC to channel → 4800 bps FSK
  discriminator → NRZI decode → bit-sync on the 40-alternating-bit
  preamble + frame-sync sequence) is **not implemented**. There is no
  public ATCS IQ vector to verify a demodulator against, and project
  policy forbids shipping an unverifiable self-consistency loopback, so
  the demod is deferred rather than faked. The RWMON / rail.watch and
  ATCS Monitor projects describe the front end (GNU Radio FSK demod at
  4800 bps) for a future stage once a captured reference is available.
  The crate's input is NRZI-decoded bits.
- **Genisys / ARES payload not decoded.** The vendor codeline protocols
  inside the Spec-200 `user_data` are out of scope; the raw bytes are
  surfaced and the message-type / "Number=2.3.2" semantics from the
  sigidwiki sample are **not** parsed.
- **Not runtime-wired.** No `Mode::Atcs`, `--mode atcs`, scan plan,
  `Message` variant, or `to_message` mapping. The crate is a standalone
  decode core, not yet in the CLI's dependency graph (see Status).
- **Reserved octets 2..4** are read past but not interpreted.
- **Line/node split** is only attempted for the two documented type-5 /
  type-7 formats; all other addresses carry the raw routing string only.
- **No off-air fixture.** No raw ATCS bit/IQ capture is published, so
  every test is anchored to the external CRC catalogue value and the
  sigidwiki worked example rather than a vendored recording.

## Key references

- **AAR Standard Manual of ATCS (Specification 200)** — RF link (4800 bps
  FSK, 900 MHz, 12.5 kHz channels), HDLC-LAPB framing, Spec-200 Layer-3
  packet header, user-group digit identifiers.
- **Signal Identification Wiki — "Automated Train Control System (ATCS)"**
  (sigidwiki.com) — independent confirmation of 4800 bps / 900 MHz /
  HDLC-LAPB and the decoded worked sample packet (the address oracle).
- **ATCS Monitor** (atcsmon.com) — address decoding documentation
  (user-group table, type-5 / type-7 line/node renderings).
- **RWMON / rail.watch, ATCS Monitor** — front-end description (GNU Radio
  4800 bps FSK demod) for the deferred IQ stage.
- ISO/IEC 3309 / 13239 — HDLC framing; CRC-16/X-25 (X-25 / IBM-SDLC)
  catalogue check value `0x906E`.
- Shared DSP: `xng_dsp::checksum::{hdlc_fcs, hdlc_frame_ok}`.
- `crates/xng-mode-atcs/PROVENANCE.md` — clean-room sourcing per fact.
