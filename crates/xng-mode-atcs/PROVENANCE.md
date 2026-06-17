# Provenance — xng-mode-atcs

Clean-room implementation. Sources used are protocol facts and standards
text only; no decoder code was copied or ported. Every structural fact
below is traceable to a public, externally checkable reference, and the
tests assert against those references — never against an encode→decode
self-consistency loopback.

## RF link / framing facts

- **AAR "Standard Manual of ATCS" (Specification 200)** — the published AAR
  standard. The RF data link runs at **4800 bps FSK** in the **900 MHz**
  band (12.5 kHz channels). Layer 2 is **synchronous HDLC-LAPB**
  (ISO/IEC 3309/13239 framing): `0x7E` flags, bit stuffing (a `0` after
  five consecutive `1`s), and a 16-bit Frame Check Sequence. A transmitter
  precedes each burst with bit synchronization (40 alternating 1s/0s,
  20 of each) and a frame-synchronization sequence.
- **Signal Identification Wiki — "Automated Train Control System (ATCS)"**
  (sigidwiki.com): independently confirms 4800 bps, 900 MHz, "ISO 7 Layer,
  Synchronous HDLC-LAPB", and provides decoded sample packets used as
  worked examples (see the Spec-200 header section).

The HDLC FCS used is the standard ISO HDLC/LAPB FCS = **CRC-16/X-25**
(poly 0x1021, reflected, init 0xFFFF, xorout 0xFFFF), provided by
`xng_dsp::checksum::hdlc_fcs`. The FCS implementation is anchored to the
public CRC catalogue check value for the string `"123456789"` → `0x906E`
(the catalogue value for CRC-16/X-25 / IBM-SDLC), asserted in
`src/frame.rs::tests::fcs_matches_x25_catalogue_value`. The deframing
tests carry that externally-fixed FCS through flag-hunt + destuffing, so
they verify the framer against a known external value rather than a blind
loopback.

## Spec-200 Layer-3 packet header

From the AAR Standard Manual of ATCS, the Layer-3 packet header is:

- **Octet 1 (control):** bit layout `Q D 1 0 P P P A` (MSB→LSB):
  - `Q` = service-signal indicator (0 on originate traffic)
  - `D` = network-service-signal confirmation request
  - bits 5..4 = fixed `1 0`
  - `PPP` = 3-bit priority level
  - `A` = ARQ-disable bit (1 disables automatic repeat request)
- **Octets 2..4:** reserved, zero on origination.
- **Octet 5 (address length):** upper nibble = source address length in
  4-bit BCD digits; lower nibble = destination address length in BCD
  digits.
- **Octet 6 onward:** the source and destination addresses, BCD-encoded
  (two digits per octet, the standard's "Address Lengths" octet says how
  many digits each address has).
- The remaining octets are the user data (the vendor codeline payload),
  which this crate returns raw and does not decode.

## ATCS address format

The first BCD digit of an ATCS address is the **user-group / direction**
identifier. From the AAR Standard Manual of ATCS (user-group identifiers)
and the ATCS Monitor address-decoding documentation (atcsmon.com):

| Digit | User group                                  |
|-------|---------------------------------------------|
| 0     | Network applications (ground network)       |
| 1     | Locomotive applications                      |
| 2     | Host applications (office / dispatch)        |
| 3     | Wayside equipment — wireline connected       |
| 4     | Other mobiles                                |
| 5     | Wayside equipment — RF connected (field MCP) |

After the type digit, the next three digits are the **AAR-assigned
railroad number** (e.g. `802` = Union Pacific). The remaining digits are
the railroad-internal routing: a **line / codeline / territory** field
and a **node** (serial MCP) field. Per ATCS Monitor, common renderings
are `T-RRR-CC-AAA` (type 7 random-access: 3-digit codeline + 3-digit
serial node) and `T-RRR-XX-AAAA` (type 5 MCP: 2-digit extension +
4-digit serial node). Because the line/node split is railroad- and
type-specific, this crate exposes the verbatim digit string plus the
externally-fixed fields (type and railroad) and the remaining
line+node digits, rather than fabricating an unverifiable fixed split.

### Worked Spec-200 sample (sigidwiki.com decoded packet)

```
Wayside Device - RF: 5125013826 (N Wapakoneta)
CO Datagram (1) Inbound to Ground Network (35)
Frame=34 GFI=2 Group=5 SSeq=77 Rseq=45 Beacon=0 Vital=0 UsrData=6
To Dispatch: 2125385538
Number=2.3.2 CODELINE_INDICATION_MSG
```

This is used as the spec-derived oracle for the address-type decode: the
source `5125013826` has leading digit `5` (Wayside RF / field MCP) and the
destination `2125385538` has leading digit `2` (Host / dispatch office).
The three digits after the type digit are the AAR railroad number, so both
peers carry railroad `125` (they are communicating across the same
territory). These mappings are asserted in the address tests.

## Spec-derived test vectors

No raw ATCS HDLC bit capture (IQ or on-air bytes) is published, so the
end-to-end packet-header decode is pinned against a **spec-derived**
example frame: a Spec-200 packet assembled byte-for-byte from the AAR
header layout above (control octet, reserved octets, BCD address-length
octet, BCD source + destination addresses, user data), wrapped in a real
HDLC frame with a genuine CRC-16/X-25 FCS. The asserted decode reproduces
the documented field semantics (priority, ARQ, addresses, payload). This
is documented as **spec-derived**, not a loopback: the bytes are laid out
by hand from the standard, and the decode is checked against the standard's
field definitions and the sigidwiki worked example, not against an encoder
in this crate.

## IQ demodulation

Not implemented (documented TODO in `src/lib.rs`). There is no public ATCS
IQ vector to verify a demodulator against, and project policy forbids
shipping an unverifiable self-consistency loopback, so the demod is
deferred rather than faked. The RWMON / rail.watch and ATCS Monitor
projects describe the front end (GNU Radio FSK demod at 4800 bps), which a
future IQ stage can follow once a captured reference vector is available.
