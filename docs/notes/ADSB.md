# Mode S / ADS-B (1090 MHz) — implementation notes

Native wideband Mode S decode for `xng-mode-adsb`. Single magnitude-domain
signal (not channelized): PPM pulse demod → CRC-24 parity with an ICAO
cache for address-overlaid frames → DF / type-code field decode → per-aircraft
CPR tracker → `xng_types::Message::ModeS`. Clean-room — see `PROVENANCE.md`;
pyModeS, rs1090, readsb and dump1090-fa are read for protocol facts and used
as off-air / field oracles only (no code ported).

Result: 164 unique frames on the `modes1` capture @2.4 MS/s vs readsb's
167 (98%, decoding 5 readsb misses); 161 @2 MS/s vs dump1090-fa's 162
(99%, 7 dump1090 misses). CI floor on the vendored fixture; a phantom
ceiling gate fences the live false-positive rate. See
[BENCHMARKS.md](BENCHMARKS.md).

## Pipeline

`crates/xng-mode-adsb/src/`. Capture centered on 1090 MHz, any rate giving
≥ 2 samples/µs.

`demod.rs::PpmDemod` → `frame.rs::FrameValidator` (CRC + ICAO trust) →
`frame.rs::decode_extended_squitter` / `decode.rs` field decoders →
`lib.rs::AdsbDecoder` CPR tracker → `lib.rs::to_message` →
`MessageBody::ModeS`. SBS-1 and Beast serialization live in the app
(`src/outputs/{sbs,beast}.rs`), not the crate.

## PHY / demod (`demod.rs`)

Mode S PPM at 1 Mbps, ICAO Annex 10 Vol IV: each 1 µs bit cell carries a
0.5 µs pulse in the first half (bit 1) or second half (bit 0). 8 µs
preamble with pulses at 0, 1.0, 3.5, 4.5 µs. 56-bit (DF < 16) or 112-bit
(DF ≥ 16) frames.

- **Magnitude domain only.** Power `|x|²` is integrated per half-µs slot;
  preamble candidates are screened on pulse/quiet energy ratios
  (`PULSE_QUIET_RATIO = 0.5`, mean pulse energy > noise × 1.2), bits decided
  by half-cell energy comparison. Gates are deliberately near-floor — the CRC
  layer arbitrates, so strict pre-gates only cost frames.
- **Integer rates** (2, 4, 8 MS/s …) use direct slot sums. At exactly
  2 MS/s an optional interpolated grid set (configurable phases) is scanned
  *in addition* (never replacing) the on-grid stream and the extra frames
  merged by bytes + position — a pulse landing between samples splits its
  energy across two slots and decides wrong otherwise.
- **Fractional rates** (2.4 MS/s — the RTL-SDR's best) run a prefix-sum
  integral path with linearly weighted fractional slot edges; bits are
  decided at interpolated half-bit *centers* (measured 157 centers vs 152
  trimmed-integrals on the readsb file). The timing grid is swept at N
  sub-sample phases and merged.
- **Effort knob.** `new()` (file/`max`) scans the full ⅛-sample phase set
  (16 fractional passes / 7 integer extra grids); `new_live()` (SDR/`live`,
  used by `src/runtime.rs`) scans a single extra phase (`&[0.5]`, 4
  fractional passes) for a ~3× cheaper scan at a small recall cost
  (modes1@2.4M: 281 → 296 of max's 313 going 2 → 4 passes — live was raised
  from 2 to 4 after real-RF testing).
- Smoothed noise floor (`NOISE_ALPHA = 1e-4`) exposed as `level_dbfs`.

## Framing / CRC trust (`frame.rs`)

CRC-24, generator polynomial via `xng_dsp::checksum::mode_s_crc`. Syndrome
= expected parity over data bits XOR received parity field.

- **DF17 / DF18 (extended squitter):** clean PI (II = 0) → syndrome 0; the
  24-bit AA field is the address. The CRC is linear, so a nonzero syndrome
  is checked against a precomputed single-bit-error syndrome table and the
  one flipped bit repaired (re-verified clean); two-bit errors are dropped.
- **DF19 (military extended squitter):** clean-PI parity with the address
  in the AA field, identical framing to DF17/18 and the same two-sighting
  confirmation. (No single-bit repair on this path; military AF≠0
  sub-formats are non-public.)
- **DF11 (all-call):** only the low-7-bit interrogator code is overlaid
  (`syndrome & 0xFF_FF80 == 0`); carries no emitted payload but counts as a
  confirmation sighting.
- **DF0 / 4 / 5 / 16 / 20 / 21 (address-overlaid parity):** the syndrome
  *is* the ICAO; accepted only when that address is already in the cache
  (learned from squitters).
- **DF24–27 (Comm-D ELM):** address-overlaid parity, always 112-bit;
  accepted only for a cache-confirmed ICAO (the long-frame length is
  additionally required).
- **Two-sighting ICAO confirmation** (load-bearing for live RF): a
  CRC-clean DF17/18/19/11 whose address has never been seen is held, not
  emitted; a second clean frame with the same address confirms it,
  releases the held frame at its original position (`released`), and admits
  the ICAO. Random parity passes (P ≈ 2⁻²⁴) never repeat an address, so
  phantoms die in the capped (64-entry, age-evicted) pending table — a quiet
  capture dropped from ~30 phantom DF17s/min to 0. ICAO cache caps at 8192,
  stalest-half eviction; the staleness clock ticks on sightings, not
  candidate attempts (attempt-based clock thrashes the cache: −7 frames).

## Message / field types implemented

### Downlink formats (DF)

| DF | Meaning | Parity / gate | Decoded |
|---|---|---|---|
| 0, 4, 16, 20 | Surveillance / Comm-B altitude | address-overlaid (cache) | 13-bit AC altitude; DF4/20 add FS/DR/UM; DF20 adds Comm-B |
| 5, 21 | Surveillance / Comm-B identity | address-overlaid (cache) | 13-bit squawk + FS/DR/UM; DF21 adds Comm-B |
| 11 | All-call reply | interrogator-code overlay | confirmation sighting only |
| 17 | Extended squitter | clean PI (single-bit repair) | full ES type-code decode |
| 18 | Extended squitter (non-transponder) | clean PI (single-bit repair) | full ES decode + CF source tag |
| 19 | Military extended squitter | clean PI | source tag + (AF=0) embedded ME type code |
| 24–27 | Comm-D ELM | address-overlaid (cache), 112-bit | KE / ND / 80-bit MD segment |

Other DFs are rejected.

### Extended-squitter type codes (TC)

- **1–4 identification:** 8×6-bit callsign over the ICAO 64-char set.
- **5–8 surface position:** quarter-globe CPR + Movement (ground speed,
  piecewise per DO-260B / "1090 MHz Riddle" §4) + Ground-Track (when the
  track-status bit is set). NUCp + version-aware NIC plumbing as for
  airborne (see Accuracy).
- **9–18 airborne position (barometric):** 12-bit altitude — **both** Q=1
  (25 ft linear) **and** Q=0 (100 ft Gillham) decoded via the dump1090 /
  pyModeS ladder — plus CPR. Per-fix position quality (NUCp + the in-message
  NICb bit) folded into `adsb_status`.
- **19 velocity:** subtypes 1/2 ground speed (E-W / N-S components,
  supersonic ×4), 3/4 airspeed/heading; vertical rate ±; **NACv** (figure
  of merit), **vertical-rate source** (GNSS vs baro, ME bit 35), and the
  **GNSS-minus-baro** altitude difference (ME 48–55) folded into
  `adsb_status`.
- **20–22 airborne position (GNSS height):** CPR taken; **geometric (HAE)
  altitude decoded** (metres → feet) and surfaced under
  `adsb_status.geometric_altitude_ft` (not the barometric `altitude_ft`).
- **28 aircraft status:** subtype 1 emergency/priority state (none /
  general / medical / minimum-fuel / no-comms / unlawful-interference /
  downed / reserved), subtype 2 ACAS-RA-broadcast flag.
- **29 target state & status (BDS 6,2):** MCP/FCU vs FMS selected altitude
  ((raw−1)·32 ft), baro pressure setting (800+(raw−1)·0.8 mbar), selected
  heading (raw·360/512°), NACp / NICbaro / SIL, and the autopilot / VNAV /
  altitude-hold / approach / LNAV flags (gated by the mode-status bit) +
  TCAS-operational.
- **31 operational status (BDS 6,5):** ADS-B version, NIC-supplement-A
  (NICa) and NIC-supplement-C, NACp, SIL (+ supplement, v2), HRD heading
  reference; airborne adds GVA + baro-altitude integrity; v2 adds SDA (low
  2 bits of the operational-mode field).

### Accuracy / integrity layer (`decode.rs`)

The version-dependent ADS-B quality layer (NUCp / NIC / NACv / SDA),
table-sourced from pyModeS `uncertainty.py` and the rs1090 `bds65`
operational-mode layout:

| Quantity | Source | Where decoded | Where emitted |
|---|---|---|---|
| **NUCp** (+ 95% containment radius) | type code (v0) | `nuc_p` / `nuc_p_rcu_m` | every position frame, `adsb_status.nuc_p` / `nuc_p_radius_m` |
| **NICb** | ME bit 7 (TC9-18) | inline | `adsb_status.nic_b` |
| **NIC v1** | TC + NIC-supp-A | `nic_v1` | via `position_quality` (computed-only, see deferrals) |
| **NIC v2** | TC + NICa + NICb/NICc | `nic_v2` | via `position_quality` (computed-only) |
| **NACv** (+ HFOM m/s) | TC19 ME 10–12 | `velocity` / `nac_v_hfom_mps` | `adsb_status.nac_v` / `nac_v_hfom_mps` |
| **SDA** | TC31 op-mode 38–39 (v2) | `operational_status` | `adsb_status.sda` |

### DF18 CF-field source classification (`df18_cf_class`)

CF 0–7 → ADS-B non-transponder (ICAO) / ADS-B non-ICAO / fine TIS-B /
coarse TIS-B / fine TIS-B non-ICAO / ADS-R / reserved (DO-260B §2.2.3.2.1.2,
the readsb & dump1090-fa `mode_s.c` mapping). Folded into `adsb_status` as
`cf` / `source` / `source_addr_type` / `source_detail`, merged with any
TC28/29/31 status already present.

### Comm-B / BDS registers (DF20/21 MB field, `bds_infer`)

Decoded registers (the brief's "2,1 / 5,3" are **not** decoded — they appear
only as labels inside the BDS 1,7 GICB capability map):

- **Format-ID registers** (explicit identifier byte / strict pattern,
  mutually exclusive, first-match-wins): **1,0** Data Link Capability
  Report, **1,7** Common Usage GICB capability map (24-register list, Doc
  9871 Table A-2-25), **2,0** aircraft identification (callsign), **3,0**
  ACAS active Resolution Advisory (ARA / RAC bits, terminal flags, threat
  identity TTI 1 = ICAO / TTI 2 = altitude+range+bearing).
- **EHS heuristic set** (scored, see below): **4,0** selected vertical
  intention, **5,0** track & turn report, **6,0** heading & speed report.
- **Meteorological fallback** (only when the EHS set is empty, mirroring
  pyModeS `include_meteo`): **4,4** MRAR (wind / SAT / pressure / turbulence
  / humidity), **4,5** MHR (turbulence / wind-shear / microburst / icing /
  wake-vortex levels + SAT / pressure / radio height).

**rs1090-style density + penalty scoring.** The EHS / meteo disambiguation
is no longer "exactly one validates" but the rs1090 score
(`bds_density_score` / `bds_penalty` / `bds_score`): each candidate's score
is the *mean* of its per-field log-densities under Gaussian / Laplace
distributions (xoolive's CAT-048-calibrated parameters), plus a within-record
cross-field penalty (BDS 5,0 `−|TAS−GS|/100` and −2.0 for a roll/track-rate
turn-sign mismatch; BDS 6,0 −3.0 when the IAS/Mach ratio leaves [250,800] kt).
Candidates below `DENSITY_THRESHOLD = −3.0` are dropped; the highest survivor
wins (EHS-first on ties). Format-ID registers keep their fast-path
precedence. This preserves the old single-match outcomes and recovers
ambiguous frames the exactly-one rule discarded.

### Comm-D ELM (DF24–27, `comm_d`)

The ELM control bit KE (downlink-tx / uplink-ack), the 4-bit D-segment
number ND, and the 80-bit message segment MD (10 bytes, hex), per ICAO
Annex 10 Vol IV §3.1.2.7.3 (rs1090 `CommDExtended` field order). Emitted
under `comm_b` (the Comm-D message channel). No public single-message
oracle exists, so the test vector is spec-derived (clearly marked, not a
loopback).

### Surveillance header FS/DR/UM (DF4/5/20/21, `surveillance_status`)

Flight status (frame bits 5–7 → alert / SPI / on-ground flags + text),
downlink request DR (bits 8–12), utility message UM (bits 13–18), per ICAO
Annex 10 Vol IV §3.1.2.6.5 (pyModeS `surv` / rs1090 `FlightStatus`). DF0/16
carry no FS header and are left unchanged.

### Mode A/C reply (`mode_ac.rs`)

Decode kernel only — the 16-bit Mode A pulse word → 4-digit octal squawk
(`word & 0x7777`) + SPI/Ident pulse (0x0080), and the Mode A→Mode C Gillham
altitude ladder (dump1090 `internalModeAToModeC`). The same ladder
(`gillham_ac13_ft`) is reused by the AC13/AC12 altitude decoders.
**The RF framing-pulse demod is not implemented**; this is the decode side a
future Mode A/C demod would feed.

## CPR position tracking (`lib.rs`)

Per-aircraft even/odd tracker. Global airborne decode from a fresh even/odd
pair (within `CPR_PAIR_SECS = 10`); local decode against the aircraft's last
fix when fresher than `CPR_LOCAL_SECS = 180`; surface positions resolve
locally against the receiver location (`set_receiver_position`). NL(lat)
closed form; the newest frame's own fix is reported.

**Speed gate:** a candidate fix is rejected if it implies motion faster than
`MAX_SPEED_MPS = 700` (~1360 kt) from the last accepted fix
(+ `SPEED_GATE_SLACK_M = 500`). This kills the tens-of-km jumps a
corrupted-but-CRC-clean CPR field produces (e.g. an unlucky single-bit
"repair" of a two-bit error). After `REJECTS_TO_REANCHOR = 3` consecutive
failures the anchor itself is dropped and re-acquired from a fresh pair.
Track table caps at `TRACK_MAX = 4096`, evicting stale fixes.

### Graduated position trust (ADSB-7, `resolve_position` / `decode::PosTrust`)

`resolve_position` returns a *graded* fix `(lat, lon, PosTrust)` layered on
top of the existing global/local CPR decode + speed gate. The grade records
*how* the fix was resolved and whether it survived the plausibility gates,
mirroring the dump1090 / pyModeS CPR trust hierarchy (most → least trusted):

- **`GlobalUnambiguous`** — resolved from a fresh even/odd pair
  (`cpr_global_airborne`); no prior reference needed.
- **`LocalContained`** — referenced off the aircraft's last good fix (when
  fresher than `CPR_LOCAL_SECS`) and confirmed inside the integrity-derived
  containment of that fix (`within_local_containment`).
- **`LocalReceiver`** — referenced off the static receiver position
  (surface targets, or an airborne first fix with no even/odd pair yet);
  weaker, since the aircraft may be far from the receiver.

**Local-containment gate (ADSB-7b).** A `LocalContained` candidate must land
within `local_containment_radius_m = 2·rc_m + min(motion, ½-zone) + slack`
of its reference, else the CPR zone number wrapped (or the field is
corrupt) and the fix is *rejected*, not merely downgraded:

- `rc_m` is the per-fix NIC/NUCp containment radius — the `nuc_p_radius_m`
  the position decoder already folds into `adsb_status` for this very frame
  (the `2·rc_m` term covers the new fix *and* the reference; `None` → no
  integrity term, only capped motion + slack).
- `min(motion, ½-zone)` caps the elapsed-time motion budget
  (`MAX_SPEED_MPS · elapsed_s`) at half an airborne CPR latitude zone
  (`CPR_LOCAL_RANGE_M ≈ 334 km`, the dump1090 `decodeCPRrelative` "±½ zone"
  range cap). Beyond half a zone a local decode can no longer be the
  nearest solution, so this term is what makes the gate strictly additive
  over the unbounded speed gate (which alone would admit an arbitrarily
  distant zone-wrap at large `elapsed_s`).

A containment-gate rejection, like a speed-gate rejection, feeds the same
`REJECTS_TO_REANCHOR` counter — a persistently wrong anchor is dropped and
re-acquired globally.

**Surfacing.** The grade is emitted in `adsb_status` as `position_trust`
(`global` / `local_contained` / `local_receiver`), only alongside a resolved
position. `xng_types::MessageBody::ModeS` has no typed trust field, so this
rides the JSON `adsb_status` channel. Oracle: pyModeS / dump1090 CPR plus
synthetic CPR round-trips (`trust_grades_global_then_local_contained`,
`local_containment_gate_rejects_zone_wrap`).

## Altitude / squawk codecs (`decode.rs`)

13-bit AC field (`altitude13`): M-bit metric flag (rejected — unused), Q-bit
25 ft, else 100 ft Gillham routed through the dump1090/pyModeS ladder
(byte-for-byte across all 4096 codes). 12-bit ADS-B field (`altitude12`)
reinserts a zero M bit and delegates. 13-bit identity field → 4-digit octal
squawk (C/A/B/D pulse interleave). The Gillham path is shared with `mode_ac`,
which also corrected a latent off-by-100-ft bug in the earlier helpers.

## Outputs

Crate emits `AdsbFrame` and `MessageBody::ModeS` (df, icao, callsign,
altitude, squawk, lat/lon, speed/track/vertical-rate, comm_b, adsb_status,
raw bytes, level). The app serializes to **SBS-1 / BaseStation** CSV and
**Beast** binary (`0x1a` framing, type '2'/'3', 6-byte MLAT counter, signal
byte), plus the standard JSON / asf-2.0 feed.

**DF17 synthesis (XM-2.2).** Non-Mode-S aircraft sources (UAT 978, HFDL)
have no raw 1090 frame, so a shared `AircraftFix` (`src/outputs/aircraft.rs`)
is re-encoded into DF17 extended squitters by `xng_mode_adsb::synth` — the
`uat2esnt` trick — letting any raw-Beast consumer (tar1090/readsb) plot them:
an even/odd **airborne-position** pair (TC11), a **callsign** frame (TC4),
and a **ground-velocity** frame (TC19 subtype 1) when a true ground speed +
track is present. Each encoder is the inverse of a `decode.rs` function and is
proven by round-tripping through this crate's own decoder, so there is no
hand-rolled bit layout that can silently drift from the reader.

An `EsSource` selects the downlink format so **rebroadcast provenance
survives** the trip onto 1090 (NEW-P0-1.3): native ADS-B is **DF17** (CA=5),
a UAT **TIS-B** rebroadcast becomes **DF18 CF=2**, and **ADS-R** becomes
**DF18 CF=6** — the CF that `decode::df18_cf_class` reads back as TIS-B /
ADS-R. The class comes from the UAT `address_qualifier`, so a 978 MHz TIS-B
target is not mislabelled as a native transponder on tar1090.

## Validation / oracles

These crates verify against external oracles, never self-loopback for field
facts:

- **Demod / frame counts:** readsb (`--no-fix`) and dump1090-fa
  (`--no-fix`) on the `modes1` capture; CI floor on the vendored fixture +
  a phantom ceiling on a quiet live capture.
- **Field decode:** worked examples from "The 1090 Megahertz Riddle"
  (Junzi Sun, CC BY-SA) vendored as unit vectors — the 40621D CPR pair,
  ground-speed / airspeed velocity, ident, altitude.
- **Position trust (ADSB-7):** synthetic CPR round-trips against the 40621D
  pair — `trust_grades_global_then_local_contained` (fresh pair → `global`,
  lone follow-up frame inside NUCp containment → `local_contained`) and
  `local_containment_gate_rejects_zone_wrap` (a flipped longitude bit lands a
  CPR sub-zone away and is rejected by the containment gate even while it
  sits under the coarse speed-gate threshold). pyModeS / dump1090 CPR are the
  field oracle for the underlying decode.
- **Accuracy / integrity:** pyModeS `uncertainty.py` tables; the NIC
  golden-vector set (`test_adsb`, twelve `8D3C…` frames → NIC 0…11, two
  supplement-sensitive) vendored as the `nic_v1` test; NACv / VR-source /
  geo-baro pinned to `bds09.decode_bds09`; TC31 v2 field positions pinned to
  `bds65.decode_bds65`.
- **Altitude:** `altitude12` / `altitude13` Gillham asserts cross-checked
  against pyModeS `decode()` on CRC-valid DF17 frames (5000/4800/5800 ft Q=0);
  Q=1 vectors from `test_adsb` (38000/−325/1000 ft); `gnss_height_ft` pinned
  to a TC20 frame (3000 m → 9842 ft); the Gillham ladder pinned to dump1090
  `internalModeAToModeC` compiled verbatim as a C oracle.
- **BDS registers:** pyModeS v3 `decode()` and `test_bds_commb`
  golden/synthetic vectors, field-exact (BDS 1,0 / 1,7 / 2,0 / 3,0 / 4,0 /
  4,4 / 4,5 / 5,0 / 6,0 / 6,2 / 6,5), vendored as `decode.rs` tests. The
  density/penalty scoring helpers are pinned to rs1090's own unit-test values
  (`cruise_bds50_passes` ≈ −0.090, `slow_bds60_fails` below −3.0,
  `density_at_centre_is_zero`, and the penalty relations).
- **Mode A/C:** dump1090 `internalModeAToModeC` / `decodeModeAMessage`
  compiled verbatim as an independent C oracle (not an encode→decode loop).
- **TC28/29/31 + FS/DR/UM:** pyModeS `bds65` / `bds62` synthetic and golden
  vectors and `surv` flight-status text; DF18 CF against readsb /
  dump1090-fa `mode_s.c`; DF19 / Comm-D against rs1090 field order
  (Comm-D's vector spec-derived, clearly marked).
- **End-to-end:** synthetic PPM loopback (`modulate.rs::frame_iq`) at 2, 8,
  and native 2.4 MS/s with added noise (`tests/end_to_end.rs`).

## Known limitations / intentional gaps

- **Version-aware NIC still computed-only.** Graduated position trust is now
  wired (`PosTrust`, surfaced as `adsb_status.position_trust` — see CPR
  tracking), but the *containment radius* it gates on is the NUCp
  `nuc_p_radius_m`, not the version-aware NIC. `nic_v1` / `nic_v2` /
  `position_quality` can resolve the version-aware NIC, and the tests pin it,
  but at decode time position frames still call
  `position_quality(tc, nic_b, None, 0, 0)` — there is no per-aircraft state
  that remembers an aircraft's latest TC31 version + NIC supplements and
  feeds them into the next position fix. Only NUCp and the raw NICb bit are
  emitted today; the resolved version-aware NIC is deferred.
- **No Mode A/C RF demod** — only the information-word decode kernel.
- **No phase-classified demod templates.** The demod is energy-comparison
  PPM with sub-sample phase sweeping; per-bit phase classification against
  reference pulse templates is not implemented.
- **Beast MLAT counter is the RX sample clock** (ADSB-8 / VERIFY-12).
  `PpmDemod::tick` derives the 6-byte 12 MHz counter from the SDR sample
  clock — `((base_samples + pos) / input_rate) · 12e6`, with `base_samples`
  carried across reads — and stamps it on `SignalQuality.rx_ticks_12mhz`;
  the host wall clock (`timestamp_micros · 12`) is only the fallback when
  that is absent. It is a consistent-rate *passive* counter (good enough for
  an MLAT client to fit the receiver's clock drift), not GPS-disciplined.
- **13-bit metric altitude (M-bit) rejected** — unused in practice.
- **Surface-position global decode needs a receiver reference**
  (`--receiver-pos`); without it, surface targets resolve only once an
  aircraft airborne fix exists.

## Standing falsifications (do not retry without a stronger prior)

- **In-frame collision rescanning** pollutes the ICAO cache with false
  mid-frame DF11/DF17 candidates and evicts real aircraft (−7 unique).
- **Two-bit syndrome-pair repair** for DF17 (even ICAO-gated) gains zero
  frames and halves max throughput.
- **Replacing** the on-grid stream with an interpolated one loses frames
  (midpoint samples blur pulse/quiet contrast: −35 used alone) — phases
  must be scanned independently and unioned.
- **Per-candidate preamble-contrast phase refinement** overfits preamble
  noise (154 vs 157 plain-center) on the readsb benchmark.

## References

- ICAO Annex 10 Vol IV (Mode S downlink, CPR, AC/ID fields, FS/DR/UM,
  Comm-D, BDS 3,0).
- ICAO Doc 9871 (BDS register tables A-2-16 / -25 / -32 / -33).
- RTCA DO-260A/B (TC28/29/31, DF18 CF, surface Movement, NIC/NACv).
- "The 1090 Megahertz Riddle", Junzi Sun (CC BY-SA) — worked examples.
- pyModeS, rs1090, readsb, dump1090-fa — fact/field oracles (no code ported).
- PROVENANCE.md — sourcing policy and per-table oracle notes.
