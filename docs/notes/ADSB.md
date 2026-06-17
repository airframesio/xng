# Mode S / ADS-B (1090 MHz) — implementation notes

Native wideband Mode S decode for `xng-mode-adsb`. Single magnitude-domain
signal (not channelized): PPM pulse demod → CRC-24 parity with an ICAO
cache for address-overlaid frames → DF/TC field decode → per-aircraft CPR
tracker → `xng_types::Message`. Clean-room — see `PROVENANCE.md`; readsb,
dump1090-fa, and pyModeS are read for facts and used as off-air / field
oracles only.

Result: 164 unique frames on the `modes1` capture @2.4 MS/s vs readsb's
167 (98%, decoding 5 readsb misses); 161 @2 MS/s vs dump1090-fa's 162
(99%, 7 dump1090 misses). CI floor on the vendored fixture; a phantom
ceiling gate (`adsb_quiet_max = 4`) fences the live false-positive rate.
See [BENCHMARKS.md](BENCHMARKS.md).

## Pipeline

`crates/xng-mode-adsb/src/`. Capture centered on 1090 MHz, any rate giving
≥ 2 samples/µs.

`demod.rs::PpmDemod` → `frame.rs::FrameValidator` (CRC + ICAO trust) →
`frame.rs::decode_extended_squitter` / `decode.rs` field decoders →
`lib.rs::AdsbDecoder` CPR tracker → `lib.rs::to_message` →
`MessageBody::ModeS`. SBS-1 and Beast serialization live in the app
(`src/outputs/{sbs,beast}.rs`), not the crate.

## PHY / demod

Mode S PPM at 1 Mbps, ICAO Annex 10 Vol IV: each 1 µs bit cell carries a
0.5 µs pulse in the first half (bit 1) or second half (bit 0). 8 µs
preamble with pulses at 0, 1.0, 3.5, 4.5 µs. 56-bit (DF < 16) or 112-bit
(DF ≥ 16) frames.

- **Magnitude domain only.** Power `|x|²` is integrated per half-µs slot;
  preamble candidates are screened on pulse/quiet energy ratios
  (`PULSE_QUIET_RATIO = 0.5`, pulse mean > noise × 1.2), bits decided by
  half-cell energy comparison. Gates are deliberately near-floor — the CRC
  layer arbitrates, so strict pre-gates only cost frames.
- **Integer rates** (2, 4, 8 MS/s …) use direct slot sums. At exactly
  2 MS/s an optional half-sample-shifted interpolated grid is scanned
  *in addition* (never replacing) the on-grid stream and the extra frames
  merged by bytes + position — a pulse landing between samples splits its
  energy across two slots and decides wrong otherwise.
- **Fractional rates** (2.4 MS/s — the RTL-SDR's best) run a prefix-sum
  integral path with linearly weighted fractional slot edges; bits are
  decided at interpolated half-bit *centers* (slot integrals split a
  boundary-straddling sample and flip bits at adverse phases — measured
  157 centers vs 152 trimmed-integrals). The timing grid is swept at N
  sub-sample phases and merged.
- **Effort knob.** `new()` (file/`max`) scans the full ⅛-sample phase set
  (16 fractional passes / 7 integer extra grids); `new_live()`
  (SDR/`live`) scans a single extra phase (`&[0.5]`, 4 fractional passes)
  for ~3× cheaper scan at a small recall cost. 16.6× realtime live /
  5.3× max on Apple M-series.
- Smoothed noise floor (`NOISE_ALPHA = 1e-4`) exposed as `level_dbfs`.

## Framing / CRC trust (`frame.rs`)

CRC-24, generator polynomial via `xng_dsp::checksum::mode_s_crc`. Syndrome
= expected parity over data bits XOR received parity field.

- **DF17 / DF18 (extended squitter):** clean PI (II = 0) → syndrome 0; the
  24-bit AA field is the address. The CRC is linear, so a nonzero syndrome
  is checked against a precomputed single-bit-error syndrome table and the
  one flipped bit repaired (re-verified clean); two-bit errors are dropped.
- **DF11 (all-call):** only the low-7-bit interrogator code is overlaid
  (`syndrome & 0xFFFF80 == 0`); carries no emitted payload but counts as a
  confirmation sighting.
- **DF0 / 4 / 5 / 16 / 20 / 21 (address-overlaid parity):** the syndrome
  *is* the ICAO; accepted only when that address is already in the cache
  (learned from squitters).
- **Two-sighting ICAO confirmation** (load-bearing for live RF): a
  CRC-clean DF17/18/11 whose address has never been seen is held, not
  emitted; a second clean frame with the same address confirms it,
  releases the held frame at its original position (`released`), and
  admits the ICAO. Random parity passes (P ≈ 2⁻²⁴) never repeat an
  address, so phantoms die in the capped (64-entry, age-evicted) pending
  table — a quiet 60 s capture dropped from ~70 phantom DF17s/min to 0.
  ICAO cache caps at 8192, stalest-half eviction; the staleness clock
  ticks on sightings, not candidate attempts (attempt-based clock thrashes
  the cache: −7 frames).

## Message / field types implemented

### Downlink formats (DF)
17, 18 (extended squitter); 11 (all-call, confirmation only); 0, 4, 16, 20
(surveillance/Comm-B altitude → 13-bit AC field); 5, 21 (identity/Comm-B →
squawk). Other DFs are rejected.

### Extended-squitter type codes (TC)
- **1–4 identification:** 8×6-bit callsign over the ICAO 64-char set.
- **5–8 surface position:** quarter-globe CPR + Movement (ground speed,
  piecewise per DO-260B) + Ground-Track (when the track-status bit is set).
- **9–18 airborne position (barometric):** 12-bit altitude with Q-bit
  (N·25 − 1000 ft) + CPR.
- **19 velocity:** subtypes 1/2 ground speed (E-W / N-S components,
  supersonic ×4), 3/4 airspeed/heading; vertical rate ±.
- **20–22 airborne position (GNSS height):** CPR taken; the HAE altitude
  encoding is intentionally left undecoded.
- **28 aircraft status:** subtype 1 emergency/priority state (none /
  general / medical / minimum-fuel / no-comms / unlawful-interference /
  downed), subtype 2 ACAS-RA-broadcast flag.
- **29 target state & status (BDS 6,2):** MCP/FCU vs FMS selected altitude,
  baro pressure setting, selected heading, NACp / NICbaro / SIL, and
  autopilot / VNAV / altitude-hold / approach / LNAV flags (gated by the
  mode-status bit) + TCAS-operational.
- **31 operational status (BDS 6,5):** ADS-B version, NIC-supplement-A,
  NACp, SIL (+ supplement, v2), and airborne GVA / baro-altitude integrity.

### DF18 CF-field source classification
CF 0–7 → ADS-B non-transponder / ADS-B non-ICAO / fine TIS-B / coarse
TIS-B / fine TIS-B non-ICAO / ADS-R / unknown (DO-260B §2.2.3.2.1.2, the
readsb & dump1090-fa mapping). Folded into `adsb_status` as
`cf`/`source`/`source_addr_type`/`source_detail`, merged with any TC28/29/31
status already present.

### Comm-B / BDS registers (DF20/21 MB field, `bds_infer`)
- **Format-ID registers** (explicit identifier byte / strict pattern,
  mutually exclusive, first-match-wins): **1,0** Data Link Capability
  Report, **1,7** Common Usage GICB capability map (24-register list,
  Doc 9871 Table A-2-25), **2,0** aircraft identification (callsign),
  **3,0** ACAS active Resolution Advisory (ARA / RAC bits, terminal flags,
  threat identity TTI 1 = ICAO / TTI 2 = altitude+range+bearing).
- **EHS heuristic set** (accepted only if exactly one validates): **4,0**
  selected vertical intention, **5,0** track & turn report, **6,0**
  heading & speed report.
- **Meteorological fallback** (only when the EHS set is empty, mirroring
  pyModeS `include_meteo`): **4,4** MRAR (wind / SAT / pressure /
  turbulence / humidity), **4,5** MHR (turbulence / wind-shear / microburst
  / icing / wake-vortex levels + SAT / pressure / radio height).

The phased precedence (format-ID → EHS exactly-one → meteo) resolves the
real BDS 1,7-vs-4,0 collision the old flat exactly-one rule could not.

### Mode A/C reply (`mode_ac.rs`)
Decode kernel only — the 16-bit Mode A pulse word → 4-digit octal squawk
(`word & 0x7777`) + SPI/Ident pulse (0x0080), and the Mode A→Mode C Gillham
altitude ladder (dump1090 `internalModeAToModeC`). **The RF framing-pulse
demod is not implemented**; this is the decode side a future Mode A/C demod
would feed.

## CPR position tracking (`lib.rs`)

Per-aircraft even/odd tracker. Global airborne decode from a fresh
even/odd pair (within `CPR_PAIR_SECS = 10`); local decode against the
aircraft's last fix when fresher than `CPR_LOCAL_SECS = 180`; surface
positions resolve locally against the receiver location
(`set_receiver_position`). NL(lat) closed form; the newest frame's own fix
is reported.

**Speed gate:** a candidate fix is rejected if it implies motion faster
than `MAX_SPEED_MPS = 700` (~1360 kt) from the last accepted fix
(+ `SPEED_GATE_SLACK_M = 500`). This kills the tens-of-km jumps a
corrupted-but-CRC-clean CPR field produces (e.g. an unlucky single-bit
"repair" of a two-bit error). After `REJECTS_TO_REANCHOR = 3` consecutive
failures the anchor itself is dropped and re-acquired from a fresh pair.
Track table caps at `TRACK_MAX = 4096`, evicting stale fixes.

## Altitude / squawk codecs (`decode.rs`)

13-bit AC field: M-bit metric flag (rejected — unused), Q-bit 25 ft, else
100 ft Gillham (Gray reorder + reflected-binary ladder). 13-bit identity
field → 4-digit octal squawk (C/A/B/D pulse interleave).

## Outputs

Crate emits `AdsbFrame` and `MessageBody::ModeS` (df, icao, callsign,
altitude, squawk, lat/lon, speed/track/vertical-rate, comm_b, adsb_status,
raw bytes, level). The app serializes to **SBS-1 / BaseStation** CSV
(TCP 30003, TT 1/3/4/5/6) and **Beast** binary (TCP 30005, 0x1a framing,
12 MHz MLAT counter), plus the standard JSON / asf-2.0 feed.

## Validation / oracles

- **Demod / frame counts:** readsb (`--no-fix`) and dump1090-fa
  (`--no-fix`) on the `modes1` capture; CI floor on the vendored fixture +
  a phantom ceiling on a quiet live capture.
- **Field decode:** worked examples from "The 1090 Megahertz Riddle"
  (Junzi Sun, CC BY-SA) vendored as unit vectors — the 40621D CPR pair,
  ground-speed / airspeed velocity, ident, altitude.
- **BDS registers:** **pyModeS v3** `decode()` and `test_bds_commb`
  golden/synthetic vectors, field-exact (BDS 1,0 / 1,7 / 2,0 / 3,0 / 4,0 /
  4,4 / 4,5 / 5,0 / 6,0 / 6,2 / 6,5), vendored as `decode.rs` tests.
- **Mode A/C:** dump1090 `internalModeAToModeC` / `decodeModeAMessage`
  compiled verbatim as an independent C oracle for the reference pairs
  (not an encode→decode loopback).
- **DF18 CF:** readsb / dump1090-fa `mode_s.c` CF switch.
- **End-to-end:** synthetic PPM loopback (`modulate.rs::frame_iq`) at 2,
  8, and native 2.4 MS/s with added noise.

## Known limitations / intentional gaps

- **No Mode A/C RF demod** — only the information-word decode kernel.
- **TC 20–22 GNSS-height altitude undecoded** (HAE encoding); position is
  still taken.
- **13-bit metric altitude (M-bit) rejected** — unused in practice.
- Surface-position global decode needs a receiver reference
  (`--receiver-pos`); without it, surface targets resolve only once an
  aircraft fix exists.

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

- ICAO Annex 10 Vol IV (Mode S downlink, CPR, AC/ID fields, BDS 3,0).
- ICAO Doc 9871 (BDS register tables A-2-16 / -25 / -32 / -33).
- RTCA DO-260B (TC28/29/31, DF18 CF, surface Movement).
- "The 1090 Megahertz Riddle", Junzi Sun (CC BY-SA) — worked examples.
- pyModeS, readsb, dump1090-fa — fact/field oracles (no code ported).
