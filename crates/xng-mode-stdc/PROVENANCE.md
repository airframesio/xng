# Provenance — xng-mode-stdc

Implemented from protocol facts collected in `docs/notes/STDC.md`,
cross-verified across inmarsatc (GPL-3), SatDump (GPL-3), and Scytale-C
(GPL-3) — **facts only; all code here is re-derived** (the sourcing
policy in docs/REFERENCES.md). Key constants were numerically re-verified
during research: unique word `07 EA CD DA 4E 2F 28 C2`, descrambler LFSR
G = 1 + x^3 + x^4 + x^5 + x^7 with init 0x80 (circulating docs that say
0x40 are wrong), row permutation i·23 mod 64, 64×162 interleaver,
K=7 r=1/2 code 171/133 (shared xng-dsp Viterbi).

Bit-order convention: the 5120 decoded bits pack into bytes LSB-first
(equivalent to the KA9Q chainback + per-byte bit reversal described by
the reference implementations); flagged for confirmation against the
public sigidwiki capture (`Inmarsat-C_TDM_EGC_IQ.zip`).

EGC service-code address lengths and the packet checksum (Fletcher /
ISO 8473 style) follow the cross-verified tables in docs/notes/STDC.md.

## Field-decode tables and oracles (2026-06)

- **frame_number → UTC-of-day** and **channel-frequency formula**:
  oracle is docs/notes/STDC.md (frame = 8.64 s exactly, 10000 frames/day;
  uplink/downlink MHz formulas), cross-checked against inmarsatc
  `decode_7D` timestamp and `uplinkChannelMhz`/`downlinkChannelMhz`.
  Both are deterministic mappings with deterministic tests; the off-air
  capture validates them on real bytes (frame 5987 → 14:22:07; the real
  0x6C uplink word 0x2748 → 1636.64 MHz, inside the L-band uplink band).
- **EGC service long names** and **LES/NCS operator-name + ocean-region
  long-name tables**: verbatim from inmarsatc `getServiceCodeAndAddress
  Name` / `getLesName` / `getSatName` (facts only; re-typed, not ported).
  The LES table keys on the full region×100+id code because inmarsatc
  maps the same id to different operators by ocean region.
- **ITA2 / Baudot (presentation 6)**: oracle is the ITU-T ITA2 standard
  alphabet (universal, deterministic); one 5-bit code per on-air byte
  with LTRS/FIGS shift. No open decoder (inmarsatc/SatDump) does this.
- **C-channel descriptor field depth (STDC-2)**: the per-descriptor byte
  maps are typed verbatim from inmarsatc's `decode_*` functions (facts
  only; re-derived, not ported) —
  `decode_6C` (0x6C: 8-bit services byte + uplink word + 28 two-bit
  TDM-slot codes), `decode_83` (0x83: sat/LES, status_bits, frame_length,
  duration, down/up-link words, frame_offset, packetDescriptor1),
  `decode_92` (0x92 login-ack: LES id, downlink word, station list),
  `decode_AB` (0xAB les-list: station list), `getStations` (6-byte
  station record: sat/LES, servicesStart, 16-bit services, downlink
  word), `decode_A3` / `decode_A8` (0xA3/0xA8 IA5 short-message text),
  `decode_08` (0x08 ack-request: sat/LES, LCN, uplink word), and the
  deepened `decode_7D` fields (signalling-channel, count, channel-type
  name, local, NCS sat/LES, status flags, 16-bit services, random
  interval). The services bit→name tables are verbatim from inmarsatc
  `getServices_short` / `getServices`. Two documented deviations from the
  C++ source, both transcription bugs in inmarsatc fixed here: its
  `getStations` downlink formula reads the same byte twice (the field is
  the two-byte word), and its `decode_7D` channelType `switch` omits the
  `break`s (so its name always falls through to "Reserved"; the intended
  per-value names are used). Channel frequencies reuse the already
  off-air-validated uplink/downlink formulas. Validation: the real
  off-air sigidwiki frame decodes the deepened 0x6C (services 0xB4 +
  28-slot array) and the full 0x7D (channel type 1 = NCS, sat/LES = AOR-E
  NCS station les 144, status operational/in-service, services incl.
  SafetyNet/InmarsatC) self-consistently; the descriptor field maps that
  have no public real-byte sample are pinned by spec-derived packets
  built to the exact inmarsatc byte layout (clearly spec-derived, not
  encode→decode loopbacks).
- **Geographic area-address classification (STDC-1)**: C2 → shape +
  documented C3 field layout per the IMO International SafetyNET Manual
  (2019), Annex 4 part A §5.2–5.3 / part B §3.3, cross-checked against
  inmarsat-sniffer's C2 service-name table.
- **Geographic area-address geometry decode (STDC-1.1 / STDC-1.2)**: the
  on-air *binary packing* of the C3 address code is decoded into
  machine-readable geometry (numeric degrees / nautical miles) and
  surfaced in the existing JSON `details["area"]["geometry"]`. Oracle for
  the binary packing is **Scytale-C** `PacketDecoderGeoUtils.cs`
  (`ReturnRectangularArea` / `ReturnCircularArea` / `ReturnNavArea`),
  whose own cited bibliography is the IMO/USCG International SafetyNET
  Manual; Scytale-C is the upstream origin of the inmarsatc reference this
  crate already cross-verifies against (facts only; re-derived in Rust).
  On-air layout (C2-repeat byte stripped):
    · Rectangular (04/34): `[0]` bit7 N(0)/S(1) + bits6-0 SW-corner lat°,
      `[1]` SW-corner lon°, `[2]` bit7 E(0)/W(1) + bits6-0 north extent
      (NM), `[3]` east extent (NM).
    · Circular (14/24/44): `[0]` bit7 N/S + bits6-0 centre lat°, `[1]`
      centre lon°, `[2]` bit7 E/W + bits6-0 radius hi, `[3]` radius lo
      (15-bit NM).
    · NAVAREA/METAREA (31) and Coastal (13/73): `[0]` area number (1–21),
      and for Coastal `[1]` coastal-area letter A–Z, `[2]` subject
      indicator (A/L nav, B/E met) per Manual Annex 4 §5.3/§3.3.
  This is the only known open decode of the C3 binary — inmarsatc,
  SatDump, sdrangel and inmarsat-sniffer all carry the EGC address as raw
  bytes (each marks the area decode "TODO" / `lat = NaN`). Verified
  against the SafetyNET Manual's published worked examples, which give the
  MSI-provider digit string the binary re-encodes to: rectangular
  `60N010W30025` (SW 60°N 010°W, 30 N, 25 E), circular `56N034W035`
  (centre 56°N 034°W, r 35 nm), and the manual body example `14N 66W 300`
  (centre 14°N 66°W, r 300 nm) — each round-trips bit-exact through the
  Scytale-C layout, pinned as inline test vectors. The NAVAREA/METAREA
  coordinator table is verbatim from Scytale-C
  `ReturnNavMetAreaCoordinator`. Unit note: the manual's MSI-provider
  rectangular C3 *string* states extent in degrees, but the LES re-encodes
  the on-air binary field as nautical miles (Scytale-C); the raw on-air
  integer (`*_extent_nm` / `radius_nm`) and the corner/centre degrees are
  both surfaced so a map layer plots without re-deriving the packing.

Demodulator: textbook coherent BPSK — square-law FFT coarse frequency
estimation, decision-directed Costas loop, Gardner timing — written
independently of the GPL references.

Known demod limitation (documented during loopback bring-up): timing
acquisition from a cold start on an unfiltered direct-injection signal is
weak — the Gardner loop needs the receive-path (DDC) filtering and a few
seconds of the continuous carrier to converge, which deployment always
provides. Definitive demod validation target: the public sigidwiki
capture (Inmarsat-C_TDM_EGC_IQ.zip), stage-by-stage against SatDump's
.frm output.

## Off-air validation (2026-06)

Validated against the sigidwiki Inmarsat-C TDM/EGC IQ recording
(CC BY-SA, 49 s, AOR-E, TDM carrier at +216 Hz in the capture). The
demod chain (coarse AFC, Costas, Gardner, UW frame sync) worked on the
real signal as-is — the UW scored 128/128 on the first frame. The one
convention fix: **coded pair order is 133-output first** (the same
finding as Aero and HFDL); with 171-first the deinterleaved frame
decodes to pseudorandom bytes and no packet checksum passes, with
133-first every packet in the frame validates.

Result: 51 packets from the capture — bulletin boards with consecutive
TDM frame numbers (5987, 5988, ...), logical-channel announcements with
MES IDs and LES routing (AOR-E), confirmations, and signalling-channel
descriptors. A 14 s slice (one full frame) is vendored as a CI fixture
(tests/data/, attributed) guarded by tests/offair.rs.
