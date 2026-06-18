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

### Application payloads decoded

The DAC/FID catalogue is shared with AIS Message 6/8. The **DAC=1** (IMO
international) FIDs are catalogued by IMO SN.1/Circ.289 ("Guidance on the use
of AIS application-specific messages", 2 June 2010); the **DAC=200** FIDs are
the UNECE Inland-AIS / RIS catalogue (ES-TRIN). Each arm of
`asm::app_decode` cites the governing clause. Decoded payloads:

- **DAC=1 FID=11 — Meteorological and hydrological data (IMO236)** (Circ.289
  Annex 1, Table 1): the OLDER met/hydro layout, structurally DISTINCT from
  FID 31 — **latitude 24 FIRST, then longitude 25** (1/1000 min), a packed
  16-bit ddhhmm date/time (no position-accuracy bit), and **unsigned** air
  temperature `(raw-600)/10 °C` and dew point `(raw-200)/10 °C`, pressure
  `raw+800 hPa`. Sentinels lat 0x7FFFFF / lon 0xFFFFFF / wind 127 / dir 511 /
  temp 2047 / dewpoint 1023 / pressure 511 / humidity 127 honoured.
- **DAC=1 FID=16 — Number of persons on board** (Circ.289 Annex; ITU-R
  M.1371-5 Annex 5 §3.10): 13-bit unsigned count, 0 = not available.
- **DAC=1 FID=17 — VTS-generated / synthetic targets** (Circ.289 Annex): the
  FIRST 122-bit target report — identifier type (MMSI / IMO / call sign /
  other), target id, latitude 24 / longitude 25 (1/1000 min), COG (deg, 360 =
  N/A), UTC-second timestamp, SOG (0.1 kt, 1023 = N/A). Second-and-later
  repeated targets are deferred.
- **DAC=1 FID=18 — Clearance time to enter port** (Circ.289 Annex; addressed,
  Message 6): linkage id, UTC month/day/hour/minute, port-and-berth name
  (20×6-bit), UN/LOCODE destination (5×6-bit), longitude 25 / latitude 24.
- **DAC=1 FID=31 — Meteorological and hydrological data (IMO289)** (Circ.289
  Annex; ITU-R M.1371-5 Annex 8): **longitude 25 / latitude 24 FIRST**
  (1/1000 min, longitude FIRST — note the OPPOSITE order to FID 11) with
  181°/91° sentinels, position-accuracy flag, UTC day/hour/minute, average +
  gust wind speed (kt) and direction (deg), **signed** air temperature
  (0.1 °C), relative humidity (%), and the deeper block: dew point (signed
  0.1 °C), pressure (`raw+799 hPa`), pressure tendency, horizontal visibility
  (0.1 NM + ">" flag), water level (`(raw-1000)/100 m`) + trend, surface
  current speed/direction, significant wave height/period/direction, sea
  state, water temperature (signed 0.1 °C), salinity (0.1 ‰), ice. All N/A
  sentinels honoured — omitted, never emitted as junk.
- **DAC=200 FID=10 — Inland ship static and voyage related data**
  (UNECE Inland-AIS): ENI/European Vessel ID (8×6-bit), length (0.1 m), beam
  (0.1 m), ERI ship type, hazard blue-cone count (5 = unknown), draught
  (0.01 m), loaded status, and the speed/course/heading data-quality flags.
- **DAC=200 FID=55 — Inland number of persons on board** (UNECE Inland-AIS;
  addressed, Message 6): crew (8), passengers (13), shipboard personnel (8);
  0xFF / 0x1FFF unknown sentinels omitted.

Unrecognised DAC/FID fall through to a `data_hex` dump of the application
payload — no unverified subtypes are fabricated. Field names follow the
`xng-mode-ais` decode convention (`lat`/`lon`/`cog_deg`/`sog_kt`/...).

## Verification (project mandate — no self-consistency loopbacks)

**Framing / payload decode** is verified against **independent oracles** in
`tests/asm_decode.rs`:

1. **Third-party decode oracle (pyais).** `inland_static_voyage_matches_pyais`
   feeds two REAL AIVDM-armored Inland-AIS Message 8 DAC=200 FID=10 sentences
   (`83m;Fa0j2d<<<<<<<0@pUg`50000`, `85M67F@j2U=7EW=RAkQkBDITMV=e`) and asserts
   the exact values pyais 2.x's own test suite decodes (length 180.6 m, beam
   42 m / 7.5 m, source MMSI, DAC/FID, loaded N/A). The armor de-mapper in
   the test is independent of our decoder. This is a genuine cross-decoder
   check, not a self-encode/self-decode loopback.

2. **Spec-cited ground-truth bit vectors.** The remaining fixtures are
   hand-built by an *independent* MSB-first bit packer (`pack` / `pack_i` /
   `pack_str`) that lays down `(value, width)` pairs in DOCUMENT ORDER per the
   field tables; the decoder reads by absolute `(offset, width)`. The two
   share no code, so a wrong offset / width / scaling mismatches. Every
   offset, width, scaling divisor and N/A sentinel was taken from the
   BSD-licensed gpsd `driver_ais.c` + `gps.h` and the GPSd AIVDM/AIVDO field
   tables (used as a FACT reference for the spec — no code ported). Tests
   cover: Message 8 / Message 6 transport headers; FID=11 (incl. the FID
   11-vs-31 position-order distinction and FID-11 N/A sentinels); FID=16;
   FID=17 (MMSI and call-sign target id forms + COG/timestamp/SOG sentinels);
   FID=18 addressed clearance time (port name / LOCODE / position); FID=31
   leading scalars and the deeper dew-point/pressure/visibility/water-level/
   current/wave/sea-state/water-temp/salinity/ice block; DAC=200 FID=10 spec
   vector; DAC=200 FID=55 with its unknown-count omission; and the
   unknown-DAC/FID `data_hex` fallback.

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
- The **remaining IALA ASM DAC/FID catalogue** beyond the DAC=1 FIDs
  (11, 16, 17, 18, 31) and DAC=200 FIDs (10, 55) decoded above. Specifically
  deferred because they are **variable-length / repeated-block** payloads with
  no single ground-truth vector to hand-verify, or lack a freely available
  bit-exact layout: FID=14 (tidal window, up to 3 repeated 93-bit blocks),
  FID=22/23 (area notice, variable sub-area shapes), FID=17 second-and-later
  targets, FID=12/25 (dangerous cargo), FID=20 (berthing data), and the
  regional DACs (235/250 UK/IE AtoN, 366 US, etc.). The transport header
  (source MMSI + DAC/FID) is always decoded and the body preserved as
  `data_hex`, so nothing is lost; per-FID body fields are not fabricated.
  (The sibling `xng-mode-ais` crate carries the AIS position-channel decode;
  the ASM bodies here were derived crate-locally from the shared spec, not
  imported.)
- **ASM in-frame interleaving / FEC** (M.2092-1 specifies a 3/4-rate
  convolutional code + interleaver as an option for the long ASM format):
  the implemented PHY is the uncoded GMSK + HDLC link, matching the AIS-style
  ASM transport. The coded long-ASM format is deferred — no public reference
  vector to ground it.

The PHY demod itself is a textbook frequency-discriminator GMSK demodulator
(clean-room DSP) reusing the same approach as `xng-mode-ais`.
