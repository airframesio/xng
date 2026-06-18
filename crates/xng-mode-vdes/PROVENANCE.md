# Provenance — xng-mode-vdes

Clean-room implementation of the VDES ASM (Application-Specific Message)
decode core. Sources are standards/spec text only; no decoder code was read
or ported.

## What VDES ASM is

ITU-R M.2092-1 ("Technical characteristics for a VHF data exchange system in
the maritime mobile band between 156 MHz and 162.05 MHz") defines VDES, which
augments AIS with two new sub-systems: **ASM** (Application-Specific
Messages) on dedicated channels, and **VDE** (VHF Data Exchange, the
high-rate links). This crate decodes **ASM only**.

The ASM channels — **ASM 1 = 161.950 MHz** and **ASM 2 = 162.000 MHz** (the
former AIS channels 2027 / 2028) — carry GMSK at **9600 bit/s**, modulation
index h = 0.5 (±2400 Hz deviation), Gaussian filter **BT = 0.5**. The link
layer is HDLC (ISO/IEC 13239): NRZI line coding (a transmitted 0 is a level
change, a 1 is no change), bit stuffing after five consecutive ones, 0x7E
flags, and a 16-bit CRC-16/X-25 FCS — the same profile AIS uses (ITU-R
M.1371). The ASM burst leads with a 32-bit ramp-up / training sequence
before the opening flag (longer than AIS's 24-bit training).

## ASM message format (decoded)

ITU-R M.2092-1 carries ASMs using the **AIS binary-message transport**: the
addressed-binary (AIS **Message 6**) and broadcast-binary (AIS **Message 8**)
structures of ITU-R M.1371, with the **same DAC/FID application-identifier
catalogue** (a 10-bit Designated Area Code + a 6-bit Function Identifier).
The transport header bit layout (ITU-R M.1371-5, reused verbatim by
M.2092-1) is decoded in `asm.rs`:

- **Message 8 (broadcast ASM):** msg ID 6 / repeat 2 / source MMSI 30 /
  spare 2 / DAC 10 / FID 6 / application data (from bit 56).
- **Message 6 (addressed ASM):** msg ID 6 / repeat 2 / source MMSI 30 /
  seqno 2 / dest MMSI 30 / retransmit 1 / spare 1 / DAC 10 / FID 6 /
  application data (from bit 88).

We extract the **source MMSI**, the **DAC/FID**, and (for Message 6) the
**destination MMSI**, and carry the binary application payload.

### Application payloads decoded (DAC=1, IMO international)

The DAC/FID catalogue is shared with AIS Message 6/8 and catalogued by IMO
SN.1/Circ.289 ("Guidance on the use of AIS application-specific messages",
2 June 2010). Two well-documented DAC=1 payloads are decoded; each arm of
`asm::app_decode` cites the governing clause:

- **FID=16 — Number of persons on board** (Circ.289 Annex; ITU-R M.1371-5
  Annex 5 §3.10): 13-bit unsigned count, 0 = not available.
- **FID=31 — Meteorological and hydrological data** (Circ.289 Annex; ITU-R
  M.1371-5 Annex 8): longitude 25 / latitude 24 (1/1000 minute, raw/60000°,
  longitude FIRST) with 181°/91° not-available sentinels, position-accuracy
  flag, UTC day/hour/minute, average + gust wind speed (kt), wind direction
  (deg), air temperature (0.1 °C signed), relative humidity (%). N/A
  sentinels (day 0, hour 24, minute 60, wind 127, dir 360, temp raw -1024,
  humidity 101) are honoured — omitted, never emitted as junk.

Unrecognised DAC/FID fall through to a `data_hex` dump of the application
payload — no unverified subtypes are fabricated.

## Verification (project mandate — no self-consistency loopbacks)

**Framing / payload decode** is verified against **spec-cited ground-truth
bit vectors** in `tests/asm_decode.rs`. The fixtures are hand-built by an
*independent* MSB-first bit packer (`pack` / `pack_i`) that lays down
`(value, width)` pairs in DOCUMENT ORDER per the cited clause; the decoder
reads by absolute `(offset, width)`. The two share no code, so a wrong
offset or width in the decoder mismatches the hand-laid packer — this is not
a self-encode/self-decode loopback. Tests cover: the Message 8 broadcast and
Message 6 addressed transport headers (source/dest MMSI, DAC/FID), the FID=16
persons-on-board count, the FID=31 met/hydro physical-value fields, the
N/A-sentinel omission regression, and the unknown-DAC/FID `data_hex`
fallback.

**Demod (PHY)** is validated **only** by a genuine modulate→AWGN→demod
chain in `tests/end_to_end.rs` (`modulate_msk/gmsk_awgn_demod_decodes_asm`,
`wideband_capture_with_carrier_offset`, `synthetic_ber_at_moderate_snr`).
This is **synthetic** — there is **no published off-air VDES ASM IQ** to test
against. The BER test runs 40 independent bursts (varying MMSI, payload, and
noise seed) at a fixed SNR and requires the overwhelming majority to deframe
and decode correctly, exercising the timing/offset loops across bit patterns
rather than a single vector.

## DEFERRED (skip-don't-fake — recorded honestly)

VDES has sparse public deployment and the full spec detail (especially VDE
and the satellite component) is not freely available. The following are
**not** implemented and were skipped rather than guessed:

- **VDE links** (VDE-TER terrestrial and VDE-SAT satellite high-rate data
  exchange): different modulation (π/4-QPSK / 8-PSK / 16-APSK), FEC, and
  framing per M.2092-1 — out of scope; no public worked examples to ground a
  clean-room decoder.
- The **full IALA ASM DAC/FID catalogue** beyond DAC=1 FID 16/31 (regional
  DACs, the remaining IMO FIDs, inland-AIS DAC=200, etc.). The transport
  header (source MMSI + DAC/FID) is always decoded and the body preserved as
  `data_hex`, so nothing is lost; the per-FID body fields are not fabricated.
  (The sibling `xng-mode-ais` crate decodes a much larger DAC/FID set for the
  AIS position channels and is the place to extend if needed.)
- **ASM in-frame interleaving / FEC** (M.2092-1 specifies a 3/4-rate
  convolutional code + interleaver as an option for the long ASM format):
  the implemented PHY is the uncoded GMSK + HDLC link, matching the AIS-style
  ASM transport. The coded long-ASM format is deferred — no public reference
  vector to ground it.

The PHY demod itself is a textbook frequency-discriminator GMSK demodulator
(clean-room DSP) reusing the same approach as `xng-mode-ais`.
