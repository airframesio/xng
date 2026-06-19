# xng — Competitive Comparison & Gap Research

> **STATUS: DRAFT FOR REVIEW — manual review & editing required before any TODO.md is created. No implementation has been done.**
>
> This document is a research synthesis only. Every finding below is a *candidate* for work, not a committed task. Treat it as raw material for a human triage pass. Nothing here has been built, fixed, or scheduled.

---

## How to read this doc

- **Status-marker legend** (applied per leaf, mode-by-mode and feature-by-feature)
  - ✅ **HAVE** — xng already does this, at or near parity with the best competitor.
  - ⚠️ **PARTIAL** — xng does some of it; the specific shortfall is named on the line.
  - ❌ **MISSING** — xng does not do this at all; feasibility/verdict noted inline.
  - 💡 **IMPROVE** — an enhancement, new build, or differentiator opportunity (often beyond the competitor).
  - ❓ **VERIFY** — a claim that needs a source/code check before it is trusted (consolidated in the Appendix).
- **Methodology**
  - Audited the live xng source tree (`crates/xng-*`, `src/outputs/*`, `src/main.rs`, `src/runtime.rs`) plus each crate's `PROVENANCE.md`, `docs/notes/*` (BENCHMARKS, HFDL, IRIDIUM, STDC, VDL2), and `docs/REFERENCES.md`.
  - Compared each mode against the named gold-standard open decoder (libacars/acarsdec, dumpvdl2, dumphfdl, JAERO, inmarsatc/tekmanoid, iridium-toolkit/iridium-sniffer, AIS-catcher, readsb/pyModeS/rs1090) and against the live competitive landscape via web research.
  - New-mode research assesses **decode feasibility + Airframes mission fit** (aviation + maritime data-link), not just "is it on the air."
  - Adversarial critic pass (folded in below + Appendix "Claims to verify") sharpened or challenged specific claims and added net-new items.
- **What "best competitor" means per mode** — each Part A section names its single most-complete reference target; gaps are framed against that one tool, with secondary references noted.

---

## Table of contents

- [Executive summary](#executive-summary)
- [Part A — Per-mode comparison & gaps](#part-a--per-mode-comparison--gaps)
  - [A1. VHF ACARS (ARINC 618) + ACARS application layer](#a1-vhf-acars-arinc-618--acars-application-layer)
  - [A2. VDL Mode 2 (ICAO Annex 10) + AVLC + ATN-B1](#a2-vdl-mode-2-icao-annex-10--avlc--atn-b1)
  - [A3. HFDL (ARINC 635)](#a3-hfdl-arinc-635)
  - [A4. Inmarsat Aero L-band (classic aero, P/R/T/C channels)](#a4-inmarsat-aero-l-band-classic-aero-prtc-channels)
  - [A5. Inmarsat Aero C-band bursts (aero-c)](#a5-inmarsat-aero-c-band-bursts-aero-c)
  - [A6. Inmarsat STD-C / EGC](#a6-inmarsat-std-c--egc)
  - [A7. Iridium](#a7-iridium)
  - [A8. AIS (ITU-R M.1371)](#a8-ais-itu-r-m1371)
  - [A9. Mode S / ADS-B (1090 MHz)](#a9-mode-s--ads-b-1090-mhz)
- [Part B — Cross-cutting: ecosystem / outputs / tooling gaps](#part-b--cross-cutting-ecosystem--outputs--tooling-gaps)
- [Part C — Per-decoder Airframes feeding feature design](#part-c--per-decoder-airframes-feeding-feature-design)
- [Part D — New transportation & RF modes research](#part-d--new-transportation--rf-modes-research)
- [Appendix — Claims to verify + references](#appendix--claims-to-verify--references)

---

## Executive summary

> The headline: **xng is at or near parity (often ahead) on demodulation/sensitivity and on the protocol *framing* of every mode it implements.** The systematic gaps are (1) human-readable *content* decode (ACARS text bodies, ATN ASN.1 arguments, AIS ASM, ADS-B accuracy/intent), (2) *positions from non-1090 modes never reaching SBS/the map*, and (3) ecosystem features (history DB, alerting, range/coverage, MLAT, finer metrics). The biggest strategic opportunity is a **unified cross-mode entity + signal-quality + distress model** that pays off in every mode at once.

- **Quick wins** (high value, low/clean-room effort, mostly pure parsing)
  - ❌ **ACARS text-content decoders** — OOOI (gtout/gtin/wloff/wlin, depa/dsta/eta), free-text POS, AMDAR/met, FLIGHTPLAN, H1 `#CFB` maintenance sub-formats. xng decodes envelopes, not the human-readable bodies competitors (acarsdec, PlanePlotter, airframes' own doc repo) surface. *Biggest user-visible ACARS win.*
  - ❌ **ADS-B TC5-8 surface movement + ground track, TC29 target-state, TC31 operational-status** — the bits are already in-message; TC31 unlocks ADS-B version + NIC/NACp/SIL so every position fix carries a quality/Rc. Pure mode-s.org clean-room.
  - ❌ **AIS easy sub-fields + multi-fragment reassembly** — type-5 eta/dims/epfd, types 1-3 rot/raim/radio, type-21 AtoN name extension, type-24 A/B merge; then a DAC/FID ASM dispatch table (~40 subtypes, the #1 AIS coverage gap).
  - ❌ **Positions → SBS/Beast/map for HFDL, Iridium, Aero** — all three *already decode* lat/lon (perf-data/freq-data/ADS-C, SBD ADS-C, AES) but `sbs.rs` is `ModeS`-only, so none reach tar1090/VRS/the dashboard. One mode-agnostic adapter fixes all of them.
  - ❌ **AIS-SART / MOB / EPIRB-AIS tagging** — near-free distress layer over data xng already decodes (tag MMSI 970/972/974 + nav_status 14 + "MOB ACTIVE" text).
  - 💡 **Shared per-burst signal-quality line** (signal/noise dBFS, SNR, CFO-ppm, FEC-corrected) — its absence is flagged in *seven* modes; one `SignalQuality` struct populated by every demod closes them all.
  - ❌ **STD-C SafetyNET area-address decode** (rect/circle/NAVAREA/coastal) + LES/NCS operator-name tables + frame-number→UTC — no open decoder does the geographic decode; high leverage.
  - ❌ **HFDL performance-data (0xD1) + frequency-data (0xD5) full records** — rich link-health/propagation telemetry xng currently discards via a shared handler; pure parsing, no DSP risk.

- **Big bets** (large but high-leverage; some unlock many leaves at once)
  - 💡 **Table/codegen-driven unaligned-PER ASN.1 core** for the ATN stack — unlocks ~44 CPDLC argument types, CHOICE extensions/fragmentation, ACSE, Session, full CM addresses, **and native ATN-B2 ADS-C** in one pass (the flagship dumpvdl2 payload xng misses entirely). Requires CLNP+COTP reassembly too.
  - 💡 **Unified cross-mode entity model** `Entity{kind, id, positions[], identities[], source_modes[]}` — ties ADS-B + HFDL/Aero/Iridium ADS-C + AIS into one track store feeding one dashboard/SBS/heatmap; BelugaProject already fuses air+sea. Pairs with one shared ICAO/registration resolver and a single TDOA/MLAT primitive over the asf-2.0 fan-in.
  - 💡 **Ecosystem catch-up**: on-disk message/track history DB + replay, alerting/watchlists + emergency/distress overlay, range/coverage analytics, readsb-schema `aircraft.json`/`receiver.json`, richer Prometheus + Grafana, continuous autogain, MLAT-usable Beast timestamps.
  - 💡 **Soft-decision / Chase decoding** where it's the standing sensitivity lever (Iridium IDA `--chase`/`--harder`, ACARS syndrome-table FEC, populate `fec_corrected` everywhere).

- **Most promising NEW modes** (ranked across all domains, mission-fit first)
  - 💡 **UAT 978 MHz** (ADS-B + FIS-B weather + TIS-B/ADS-R) — open DO-282B spec, same RF/Soapy chain, fuses with the existing `adsb` mode; fills the glaring US ADS-B/weather hole. *Aviation #1.*
  - 💡 **COSPAS-SARSAT 406 MHz** (ELT/EPIRB/PLB distress) — open T.001/T.018, BCH/CRC-gated, demod within xng-dsp, dead-center on the aviation+maritime distress mission. Rare on-air but compelling.
  - 💡 **DSC (VHF Ch70 + HF/MF) + NAVTEX** — the GMDSS maritime-distress pair; open BCH(10,7)/SITOR-B coding, reuses xng's VHF AIS + HF front-ends; multimon-ng still lacks DSC.
  - 💡 **Radiosondes (RS41/M10/DFM…)** — open cores (rs1729/RS), worldwide, huge SondeHub community, auto-scan maps onto xng tooling; culturally identical to Airframes feeding.
  - 💡 **EOT/HOT/DPU rail telemetry (457/452 MHz)** — clear FSK/Manchester, RTL-SDR-only, train-tail position telemetry that drops into the live-map model; competitors are stale GPL scripts. *Best rail fit.*
  - Honorable mentions: **ADS-L + FANET/OGNTP** (open sub-GHz GA/glider, one 868/915 capture), **APRS/AX.25 incl. HAB balloons**, **POCSAG/FLEX ops paging**, **ATCS 900 MHz rail signaling**.
  - **Explicitly decline:** PTC/GSM-R/FRMCS/TETRA-TEA2-3/LoRaWAN (encrypted), Eurobalise/AEI/E-ZPass (near-field/toll-plaza-only), DSRC/C-V2X (wrong band+PHY, sunsetting), SatDump weather imagery + gr-satellites/SatNOGS cubesat (entrenched incumbents, imaging not data), Globalstar/Starlink/rocket telemetry (proprietary/encrypted), SwiftBroadband-Safety (no open PHY decoder — roadmap watch only).

---

## Part A — Per-mode comparison & gaps

### A1. VHF ACARS (ARINC 618) + ACARS application layer

> **Reference targets:** libacars 2.2.1 (app layer) + f00b4r0/acarsdec fork (demod/FEC/outputs/text-fields); PlanePlotter/COAA (commercial, plots embedded text positions). **Closest Rust peer:** xoolive `datalink`/`acars` crate.

- **Message / PDU / field coverage gaps**
  - ARINC 618 frame structure
    - ✅ HAVE — `SYN SYN SOH` hunt (1-bit tolerant, both polarities), `Mode/Address(7)/TechAck/Label(2)/BlockId` header, STX/ETX/ETB/bare-ETX, BCS CRC-16/KERMIT, DEL, odd-parity LSB-first — full ARINC 618-6 §2.x framing.
    - ⚠️ PARTIAL — Mode-1 **1.8 kbps** demod absent (xng is 2400-baud-only, `BAUD=2400`); acceptable since ARINC Mode 1 was deleted from ICAO standards in the 1990s and live VHF ACARS is 2.4 kbps POA only — "no real gap" but note for completeness vs the nominal dual-rate spec.
    - ❌ MISSING — raw **MIN (Message Identifier Number)** not surfaced as a field the way acarshub/acars_router expect (xng keys reassembly on `(tail,label,msg_num)` but does not expose the raw MIN). (verify the 4th-char downlink rule edge cases)
  - Standard message labels (ARINC 618 / airframes label catalogue)
    - ⚠️ PARTIAL — xng decodes the **label string** but has no label→meaning table; acarsdec has hardcoded per-label field extractors (`10,11,12,15,17,1G,20,21,26,2N,2Z,33,39,44,45,80,83,8D,8E,8S,Q1,Q2,QA–QT,RB`); canonical reference is `airframesio/acars-message-documentation`.
    - ✅ HAVE — `_d` (`0x7F`→`'d'`) display convention; `SQ` squitter / all-call (NUL tail → `tail: None`).
    - ❌ MISSING — `Q0` and the full **`Q`-series link-test/squitter family** (Q0–Q7, QA–QX) not labelled/decoded.
  - Embedded text-content message formats — **the big gap (xng decodes envelopes, not human-readable bodies)**
    - ❌ MISSING — **OOOI** (OUT/OFF/ON/IN): acarsdec emits `gtout/gtin/wloff/wlin/depa/dsta/eta` from text; xng emits none.
    - ❌ MISSING — **position reports in text** (labels `20/POS`, `4J`, `H1 POS`): PlanePlotter plots embedded position/AMDAR/ADS reports; xng extracts lat/lon only from ADS-C envelopes, never from free-text POS.
    - ❌ MISSING — **AMDAR / winds-aloft / PIREP** weather in ACARS text (labels `40`, `4J`). *Critic sharpening:* AMDAR is a structured, internationally-standardized WMO-BUFR-class schema (phase/roll-flag/turbulence/humidity, b004/xx003-009) decoded operationally by NOAA `dcacar` — treat as a parseable schema, not "weather text."
    - ❌ MISSING — **FLIGHTPLAN / route (FPN)**, Boeing/Airbus telex / free-text structured formats (H1 FPN, label `40` clearance) — documented in airframes repo, undecoded.
    - ⚠️ PARTIAL — H1 **`#CFB`/CF** Boeing variants: xng routes H1 to ARINC 622/OHMA only. *Critic sharpening:* the `#CFB` family is far richer than `#CFB.01` — documented set includes `#CFBAPM_REPORT, #CFBATA, #CFBAL, #CFBFDE, #CFBECT, #CFBFLR, #CFBLIGHTS, #CFBMIL, #CFBMPF, #CFBPAGE, #CFBWRN` (engine vibration / oil-temp / FADEC-bleed / nav-light-fail / MDC-trend), each a distinct maintenance-telemetry schema in airframes' own repo.

- **Application / decode-layer (vs libacars 2.2.1)**
  - Label routing — ✅ HAVE `H1`→sublabel/MFI→ARINC 622/OHMA; `A6/AA/B6/BA`→ARINC 622; `MA`→MIAM; `SA`→media advisory. ⚠️ PARTIAL — sublabel/MFI extraction only on `H1` (libacars applies it generically; acarsdec emits `sublabel`+`mfi`).
  - ARINC 622 ATS envelope — ✅ HAVE IMI table, GS addr, CRC-16/IBM-3740 residue 0x1D0F. ❌ MISSING **CR1/CC1/DR1 context-management ASN.1** bodies (payload_hex only; libacars also punts, but flag it).
  - ADS-C / FANS-1/A — ✅ HAVE full libacars tag set, exact scaling, field-validated vs 4 real messages — **at parity with libacars adsc.c**.
  - CPDLC / FANS-1/A — ✅ HAVE UPER decode, 129 downlink + 183 uplink element tables, ~23 argument shapes, RouteClearance. ⚠️ PARTIAL — argument shapes not decoded fall to bracketed template: `DistanceOffsetDirection, Frequency, BeaconCode, ProcedureName, ICAOunitnameFrequency, ICAOfacilitydesignation, HoldClearance, Predepartureclearance, PositionReport, RemainingFuelRemainingSouls, ErrorInformation, Altimeter, ATISCode, VersionNumber, Tp4table, DistanceToFromPosition`; `FANSPosition` placeBearingDistance + RouteClearance trackDetail/routeInformationAdditional undecoded; additional elements stop at first undecodable arg.
  - MIAM / ARINC 841 — ✅ HAVE CF frame map, base85, v1/v2 DATA, DEFLATE, file reassembly. ⚠️ PARTIAL CRC parsed-not-verified; validated only by synthetic roundtrip (no off-air MA samples vendored).
  - OHMA (737 MAX) — ✅ HAVE routing prefix, RYKO marker, dup-first-block quirk, base64→zlib→JSON (libacars parity; synthetic-only caveat).
  - Media advisory (SA) — ✅ HAVE v0, 8 link codes. ❌ MISSING only v0 (libacars handles version field generically; verify any v1+ exists).
  - Reassembly — ✅ HAVE `(tail,label,msg_num)` key, downlink 4th-MSN-char + uplink block-id A..W wrap, dedup, 120 s, re-run app decode. 💡 IMPROVE surface a **reassembly-status enum** (acarsdec `assstat`: complete/in-progress/skipped/duplicate; xng only sets `reassembled=true`).
  - **Net parity:** xng is at/near libacars parity on every app libacars decodes; the only app-layer gaps are the CPDLC argument readers + trackDetail above. xng lacks no top-level protocol class libacars has (✅ CONFIRMED — see Appendix — libacars 2.2.1 adds no new top-level protocol since OHMA).

- **Demod / sensitivity / robustness**
  - ✅ HAVE — differential MSK-on-AM via AM envelope (carrier-offset immune), DC block, −1800 Hz mix, 1300 Hz LPF, discriminator, integrate-and-dump w/ zero-crossing timing, both-polarity sync.
  - ✅ HAVE — **error correction at parity with acarsdec** (`MAXPERR=3` both, bad-parity localization + 16-flip BCS search).
  - 💡 IMPROVE — acarsdec uses a **precomputed syndrome table** (`syndrom.h`) for O(1) error-position lookup; xng brute-forces → faster/deeper correction possible.
  - ❌ MISSING — **no `noise`/noise-floor field, no SNR** (only envelope RSSI `level_dbfs`); acarsdec emits `level`+`noise` per message.
  - ⚠️ PARTIAL — **no off-air ACARS sensitivity benchmark**: BENCHMARKS.md has HFDL/VDL2/Iridium/AIS/Mode-S but **none for ACARS POA** (loopback only) → can't claim acarsdec parity; add a head-to-head decode-count CI gate.
  - ✅ HAVE — multi-channel from one wideband capture (proven 2ch @ 2.4 MS/s).

- **Outputs & feed**
  - ✅ HAVE — acarsdec-compatible flat JSON over UDP (`feed.airframes.io:5550`), CRC-OK only.
  - ❌ MISSING — acarsdec JSON fields xng does NOT emit: **`noise`, `sublabel`, `mfi`, `assstat`, nested `libacars`/`app` object, `depa/dsta/eta/gtout/gtin/wloff/wlin`** (`sublabel`/`mfi`/app exist internally but are dropped from the acarsdec_json datagram).
  - ❌ MISSING — **MQTT output** sink (acarsdec f00b4r0 has `mqttout.c` — ❓ VERIFY against current 4.x tree, see Appendix); **StatsD/Prometheus per-label & per-channel** counters (verify xng has per-label ACARS counters).

- **What the best competitor does that xng doesn't** — text-layer field extraction (OOOI/per-label), `noise`, MQTT, StatsD, syndrome-table FEC, `assstat` (acarsdec); CPDLC argument readers + trackDetail/placeBearingDistance (libacars); plotting embedded text positions/AMDAR (PlanePlotter).

- **Improvement ideas** — build label→type table + text-content decoders (OOOI/POS/free-text/FLIGHTPLAN/AMDAR/#CFB) from airframes' own doc repo; add noise/SNR + emit the missing acarsdec_json fields; establish an off-air acarsdec head-to-head benchmark; finish CPDLC argument readers; add MQTT + per-label counters; verify MIAM CRC and vendor real off-air captures.

- **File refs:** `crates/xng-mode-acars/src/{demod.rs,frame.rs}`, `crates/xng-acars/src/{lib.rs,cpdlc/mod.rs,cpdlc/tables.rs,miam.rs,media_adv.rs,reasm.rs}`, `src/outputs/acarsdec_json.rs`, `docs/notes/BENCHMARKS.md` (no POA row).

---

### A2. VDL Mode 2 (ICAO Annex 10) + AVLC + ATN-B1

> **Gold-standard:** dumpvdl2 (szpajder) — the only open decoder with the full ATN stack. vdlm2dec decodes only ACARS-over-AVLC + ARINC-622 via libacars. MultiPSK is closed/weak. Every gap below is xng vs dumpvdl2.

- **AVLC link layer** — ✅ HAVE I/S/U frame parse (RR/RNR/REJ/SREJ, UI/DM/DISC/UA/FRMR/XID/TEST), N(S)/N(R)/poll. ❌ MISSING **SABME** (0x6F) U-command (verify if folded into `U?`). ⚠️ PARTIAL FRMR info-field decode (named not expanded); AVLC FCS octet order accepted either way (pin one canonical to drop a false-accept path).
- **XID parameter set (Doc 9776 / ISO 8885)** — xng decodes ~17 VDL params + GS lists; dumpvdl2's table is larger.
  - ✅ HAVE — SQP (0x02), T4 (0x42), MAC persistence (0x43), M1 (0x44), TM2 (0x45).
  - ❌ MISSING — **TG5 (0x46), T3min (0x47), GS-address-filter (0x48), broadcast-connection (0x49), frequency-support-list (0xC0), airport-coverage (0xC1), nearest-airport (0xC3), ATN-router-NETs (0xC4), system-mask (0xC5), TG3 (0xC6), TG4 (0xC7)**, and the **entire public ISO-8885 HDLC param set** (procedure classes, HDLC options, N1/k/timers/counters) — xng parses only the VDL private group 0xF0.
  - ⚠️ PARTIAL — autotune-frequency/binary layouts kept as hex (deliberate); 💡 decode the well-specified ones (autotune freq→MHz, timers→int).
- **ATN transport — X.25 (ISO 8208)** — ✅ HAVE DATA/CALL-REQ/ACC/CLR/RESET/DIAG/RR/RNR/REJ + M-bit reassembly per-LCN (60s). ❌ MISSING **RESTART-REQ/CONF**, facility **naming**, clear/reset/restart **cause + 100+ diagnostic-code dictionaries** (xng carries raw numbers), **SNDCF compression facility**, Call-Request maintenance/init status bit (verify).
- **ATN transport — CLNP (ISO 8473)** — ✅ HAVE full header. ❌ MISSING **multipart CLNP reassembly** (gate for ADS-C v2 — ✅ CONFIRMED dumpvdl2 v2.2.0), **ATN security label TLVs** (traffic-type/ATSC-class/subnetwork-type). ⚠️ PARTIAL compressed-CLNP LREF (near-parity, both label-only).
- **ATN transport — COTP (ISO 8073)** — ✅ HAVE 5 TPDUs (CR/CC/DR/DT/ER) + refs. ❌ MISSING **DC/ED/AK/EA/RJ** (dumpvdl2 has all 10), full variable part (TPDU-size, checksum, **ATN checksum 0x08**, credit, EOT, extended seq), **multipart COTP reassembly**.
- **ATN transport — IDRP (ISO 10747)** — ✅ HAVE OPEN/UPDATE/ERROR/KEEPALIVE/CEASE + 16 UPDATE path attrs (parity). ❌ MISSING **RIB-REFRESH** (6th type), OPEN body fields. ⚠️ PARTIAL ERROR code/subcode text mapping.
- **ATN transport — ES-IS (ISO 9542)** — ✅ HAVE ESH/ISH/**RD** (xng *ahead* — dumpvdl2's esis.c has no RD). ❌ MISSING option TLVs (Mobile-Subnetwork-Capabilities 0x81, ATN-Data-Link-Capabilities 0x88, Priority 0xCF, Security 0xC5) — verify xng option coverage.
- **ATN application — CPDLC** — ✅ HAVE protected-mode PDUs, msg header, DateTimeGroup, logicalAck, full UPLINK[238]/DOWNLINK[114] element names. ⚠️ PARTIAL — **~44 argument types unsupported** (element named but walk stops, hiding later elements — *the single biggest CPDLC gap*). ❌ MISSING **plain/unprotected PDUs** (verify), **forward/forward-response bodies**, CHOICE extension alternatives / integrityCheck / PER fragmentation.
- **ATN application — Context Management (CM)** — ⚠️ PARTIAL only cmLogonRequest decoded; CM TSAP/NSAP addresses + CMContactRequest/LogonResponse/ForwardRequest/Update not decoded.
- **ATN application — ADS-C over ATN (ATN-B2)** — ❌ MISSING **the headline gap**: native ATN-B2 ADS-C (ADSReport/RequestContract/Accept/Reject/PositiveAck/NonCompliance over CLNP/COTP). xng decodes ADS-C only over ACARS/ARINC-622 (FANS). This is exactly why dumpvdl2 added CLNP/COTP reassembly.
- **ATN application — ACSE / Session** — ❌ MISSING **ACSE (ISO 8650 AARQ/AARE/RLRQ/RLRE/ABRT)** and **Session (ISO 8327/X.225 SPDU)** layers entirely.
- **ACARS-over-AVLC (0xFF AOA)** — ✅ HAVE 0xFF strip → full shared xng-acars stack + multiblock reassembly.

- **Application strategy** — xng hand-walks unaligned PER; dumpvdl2 uses asn1c-generated decoders. 💡 IMPROVE: a generated/table-driven PER core would close ~44 CPDLC args, CHOICE extensions, fragmentation, ACSE, Session, CM addresses, and ADS-C-v2 **at once**. ❌ MISSING CLNP+COTP fragment reassembly; ❌ MISSING `--dump-asn1` raw tree dump.
- **Demod** — ✅ HAVE coherent preamble phase-fit + per-sample CFO + decision-directed differential tracking (beats dumpvdl2 off-air, **44 vs 41**), RS(255,249) + interleaver + soft erasures, multi-channel. ⚠️ PARTIAL acquisition on weakest bursts (own PROVENANCE names it the standing gap; no Gardner/matched timing-error detector). ❌ MISSING **PPM/CFO reject filter** (`--max-ppm` — ✅ CONFIRMED v2.5.0).
- **Outputs (what dumpvdl2 emits that xng doesn't)** — ⚠️ PARTIAL per-message signal line `[-18.9/-43.9 dBFS][25 dB][0.4 ppm]` (xng has rssi only, not noise+SNR+ppm together); ⚠️ `--extended-header` `[S][L][F][#]` (xng has F only); ❌ `--addrinfo`/`--bs-db` aircraft enrichment; ⚠️ `--msg-filter` per-PDU grammar; ⚠️ StatsD (xng has Prometheus, different protocol; no `good_loud` overload counter — verify); ⚠️ binary raw-AVLC archive; ❌ **ZMQ publisher**; ❌ `pp_acars` line format (verify). ✅ HAVE `--gs-file` GS naming, JSON for all protocols.
- **xng leads on:** off-air decode count, ES-IS RD PDU, capture-sharing architecture, one-binary multi-mode, permissive license.
- **Improvement ideas** — codegen PER core; CLNP+COTP reassemblers; remaining XID params + autotune/timer decode; X.25 facility/cause/diagnostic naming + Restart; full per-message signal line + extended-header + CFO/ppm metric & reject filter; aircraft enrichment + msg-filter grammar; ZMQ + raw-AVLC capture; ATN security label/CLNP TLVs; add SABME + FRMR body + pin FCS order.
- **Critic verify (Appendix):** dumpvdl2 **v2.5.1 (2026-01-06)** fixed a **249-octet-multiple block-length decode bug** — cross-check xng's own AVLC block-length handling.

---

### A3. HFDL (ARINC 635)

> **Gold-standard:** dumphfdl (szpajder). PC-HFDL/MultiPSK are closed and weaker. Every gap is xng vs dumphfdl.

- **SPDU (squitter)** — ✅ HAVE 66-octet parse w/ FCS, gs_id/name, utc_sync, frame_index/offset, change_note, min_priority, systable_version, freqs_in_use, neighbor2/3 (field-for-field vs spdu.c). ❌ MISSING `rls_in_use` (b1) + `iso8208_supported` (b5) flags; SPDU per-slot assignment codes `[4..51]` (47-octet TDMA reservation map — neither tool parses, but largest unparsed region, exposes live scheduling).
- **MPDU / LPDU** — ✅ HAVE uplink/downlink + per-aircraft grouping, logon-request/resume/confirm/logoff w/ bit-reversed ICAO. ⚠️ PARTIAL **logon-denied (0x2F)** falls to generic `"lpdu"` (dumphfdl has reason-code table); logoff/denied reason emitted raw (no text map). ❌ MISSING **aircraft-ID→ICAO cache** (dumphfdl ac_cache.c resolves channel-local ac_id to real ICAO from logon-confirm; xng emits raw byte, no `--aircraft-cache-ttl`).
- **HFNPDU** — ⚠️ PARTIAL **performance-data (0xD1)** uses the *same handler as 0xD5*, lifting only flight/lat/lon/utc; dumphfdl parses the full 47-byte record (version, flight_leg, gs/freq_id, freq_search_cnt, hf_data_disabled_duration, per-bitrate mpdu/spdu rx/tx/err counters, freq_change_code) — rich telemetry xng discards. ⚠️ PARTIAL **frequency-data (0xD5)** drops the per-GS `{gs_id, prop_freqs, tuned_freqs}` arrays. ❌ MISSING **system-table-request (0xD2)** field parse; ⚠️ delayed-echo (0xDE) parity-but-unnamed; ✅ HAVE enveloped ACARS (0xFF)→shared stack + unnumbered-data envelope event (no silent drop).
- **Application** — ✅ HAVE full shared ACARS stack over HFDL (≥ dumphfdl's libacars carriage). ⚠️ PARTIAL CPDLC ASN.1 arguments pending. ❌ MISSING **position from logon request/resume** (dumphfdl position.c back-dates UTC); ❌ MISSING **consolidated HFDL position → Basestation feed** (xng's SBS server is Mode-S-only — HFDL lat/lon from 0xD1/0xD5/ADS-C never reaches it). *Cross-cutting — see Part B unified-entity bet.*
- **Demod** — ✅ HAVE 36/37 vs dumphfdl (97%); coherent A1 joint timing/CFO fit, T/2 LMS equalizer, decision-directed carrier loop, M1 8-shift, failed-burst rescue. 💡 IMPROVE soft-Viterbi corrected-bit count (`fec_corrected` always None); ⚠️ LMS taps 7 vs documented 15 (verify); 💡 FFT polyphase channelizer for many-channel CPU; ❌ MISSING per-frame SNR/signal/freq-offset in headers (RSSI only).
- **Outputs** — ✅ HAVE JSON/proto (asf2), Airframes feed, dumphfdl-JSON external ingest, Prometheus, scan harvesting of `systable-complete`. ⚠️ PARTIAL Prometheus far coarser than dumphfdl StatsD (no preamble A2/M1, bad_fcs/too_short per layer, dir counters, reasm states, cache gauges). ❌ MISSING per-channel **noise-floor trending gauge**, `--freq-as-squawk`, zmq/kafka/file-rotation outputs, `--raw-frames`/`--output-mpdus`/`--output-corrupted-pdus` hex modes.
- **System table & GS data** — ✅ HAVE 0xD0 partial reassembly + `systable-complete`. ❌ MISSING **persistence** (`--system-table`/`--system-table-save`; cold start can't enrich until a fresh full table is heard). ⚠️ PARTIAL GS names hardcoded IDs 1–17 with 12 holes (dumphfdl reads a config file up to ID 127); 💡 config-driven station file future-proofs new GS (Muan/RKJB).
- **Improvement ideas** — split 0xD1/0xD5 handlers + decode full perf/freq records (biggest content gap, pure parsing); system-table file load/save + config GS names; aircraft-ID→ICAO cache + lift `{lat,lon,utc}` into `MessageBody::Hfdl` + wire HFDL positions to SBS/Beast (incl. freq-as-squawk); name 0x2F/0xD2/0xDE with reason tables; expand Prometheus to dumphfdl granularity + noise-floor gauge; surface per-burst SNR/CFO; set `fec_corrected`.
- **Files:** `crates/xng-mode-hfdl/src/{pdu.rs,systable.rs,demod.rs,lib.rs}`, `src/outputs/{beast.rs,metrics.rs}`, `src/main.rs`, `docs/notes/{HFDL.md,BENCHMARKS.md}`.

---

### A4. Inmarsat Aero L-band (classic aero, P/R/T/C channels)

> **References:** JAERO (names ~30 P-channel SU types, decodes ~6) + libaeroambe (C-channel audio); **strongest competitor:** inmarsat-sniffer (alphafox02) — continuous/parallel channels, 568k aircraft DB, satellite flags, SBS/MQTT/web.

- **P-channel SU coverage (JAERO names a full 0x00–0x76 table; xng decodes ~6)**
  - ❌ MISSING — log-on/log-off control SUs (0x10–0x17: request/confirm/reject/interrogation/ack/prompt + data-channel-reassignment) — the AES↔GES session lifecycle.
  - ❌ MISSING — `Call_announcement` 0x21, `T_channel_assignment` 0x51.
  - ❌ MISSING — **AES system-table broadcast** (satellite_identification 0x0C, GES_beam_support 0x07, Psmc/Rsmc channels 0x05, broadcast_index 0x0A) — JAERO *decodes* satellite ID + longitude + frequency; xng has no satellite/beam/channel parse at all.
  - ❌ MISSING — EIRP-table 0x28, P/R-control-ISU 0x40, T-control-ISU 0x41, RQA 0x61, RACK/TACK 0x62, short-LSDU user-data 0x74/0x76.
  - ✅ HAVE — ISU 0x71, SSU 0xC0|seq, Fill 0x01, C-channel assignment 0x31–0x34 (rx/tx MHz + spotbeam) — JAERO's only fully-decoded P types.
- **R-channel** — ⚠️ PARTIAL decodes user-data ISU/SSU but not JAERO's named control set (access-request/call-progress/telephony-ack/RQA/ACK); R-channel `SEQINDICATOR→(k,n)` mapping flagged-for-verification, unvalidated off-air.
- **T-channel** — ✅ HAVE user-data ISU RLS 0x71 + Fill; ❌ MISSING the ISU/SSU multi-SU variant.
- **C-channel sub-band** — ✅ HAVE 0x01 fill / 0x30 call-progress / 0x60 telephony-ack (JAERO's AEROTypeC set).
- **Header/framing** — ⚠️ PARTIAL skips the 16-bit P-channel frame header (formatid/superframe/framecounters) JAERO uses for superframe sync/diagnostics.
- **Application** — ❌ MISSING satellite identity output (which sat: I-4 F1/F3, Alphasat, I-3 F5 — competitor only takes it as config); ❌ MISSING structured log-on/log-off events (would *beat* JAERO's name-only handling); ⚠️ PARTIAL C-channel AMBE extracted (12B/20ms) but no audio (JAERO+libaeroambe decode to WAV); ❌ MISSING older AERO-H **LPC** voice path; ✅ HAVE full shared ACARS/ADS-C/CPDLC/MIAM/OHMA layer; 💡 IMPROVE `fec_corrected` always None.
- **Demod** — ⚠️ PARTIAL 600/1200 freq-discriminator MSK ~2 dB less sensitive than JAERO's coherent OQPSK; ❌ MISSING channel-center AFC/DCD state machine (JAERO `FreqOffsetEstimateSlot`); ✅ HAVE 10.5k A-QPSK coherent OQPSK lock (BER 0, 144 CRC-valid off-air — stale PROVENANCE note is wrong); 💡 no BER-vs-SNR curve vs JAERO; ⚠️ R/T/C loopback-only (R/T mostly unreceivable on L-band by physics); 💡 C-channel scrambler `dl2` alignment unimplemented.
- **Outputs** — ❌ MISSING C-channel `to_message()`/feed path + not wired to any runtime `Mode` (library-only, users can't run it); ⚠️ `MessageBody::Aero` only carries `c-channel-assignment` (no log-on/satellite-id/system-table/call bodies); ✅ HAVE normalized `Mode::AeroL` output + shared sinks; 💡 no aircraft-DB enrichment / Aero position plotting (inmarsat-sniffer ships 568k DB + tar1090 basestation + Leaflet map).
- **Best competitor (inmarsat-sniffer) edges** — operator-selectable satellite + positions; aircraft DB + 3-tier position extraction; JAERO-text-format-3 output; per-channel raw-audio ZMQ. **xng matches/beats on:** continuous multi-channel + STD-C from one capture, SoapySDR/Airspy, MQTT/SBS/JSON/web; AMBE frame extraction + readable ADS-C/CPDLC + asf-2.0.
- **SwiftBroadband-Safety (SB-S) — scope check** — ❌ MISSING / roadmap watch (not a hand-wave). *Critic addition:* SB-S carries **CPDLC + ADS-C + ACARS-over-IP** identical to Classic Aero, IP-encapsulated then stripped to the ACARS network on the ground — it is the *designated FANS successor* xng will eventually need. No open SDR decoder exists; the barrier is the ARINC 781 Att.8 / Cobham AVIATOR-S PHY. Name it explicitly as the future-FANS watch-item.
- **Improvement ideas** — full P-channel SU classifier (0x00–0x76) emitting log-on/call/T-assign/RACK events; decode AES system-table → satellite-id/longitude/channel-lists + tag every message with the resolved satellite (self-configuring scanner, L-band analogue of HFDL systable); wire `CChannelDecoder` into a runtime mode (opt-in AMBE behind a feature flag); interpret the frame header for superframe lock + DCD/AFC; capture real off-air R/T to validate SEQINDICATOR + C-channel `dl2`.

---

### A5. Inmarsat Aero C-band bursts (`aero-c`)

> **Reference:** JAERO burst demod. Decodes the *same* SU/ISU/SSU/ACARS layer as L-band `aero`; only the PHY differs.

- **Scope reality check — keep distinct?**
  - ⚠️ PARTIAL — `aero-c` decodes Classic-Aero R/T inbound bursts (aircraft→GES, up-converted to ~3685–3687 MHz C-band feeder); same protocol layer as L-band P-channel, only the burst MSK PHY (UW `0xE15AE893`, 64×5+64×3 interleave) differs.
  - ❌ MISSING — runtime never reaches it: `Mode::AeroC` is in the enum/`native_modes()` but has **no scan-plan/dispatch, no feed output**, and `AeroEvent::to_message` hard-tags `Mode::AeroL` so bursts would **mislabel as `aero-l`**.
  - 💡 IMPROVE — the C-band feeder return link is **effectively unreceivable** (JAERO's author knows of no one who has received it); `aero-c` is loopback-only *by physics*. Consider folding into `aero` as a burst sub-decoder (shares 100% of the SU layer; `burst.rs` already reuses `su::Reassembler`).
  - 💡 IMPROVE — JAERO ships a 10.5k A-QPSK burst path; xng tries 600/1200 only, has **no 10.5k burst** here.
- **SU type layer** — ❌ MISSING all named R/T/P SU message types (xng only does ISU/SSU reassembly + 0x31–0x34 + ACARS; non-data signalling SUs dropped); ⚠️ PARTIAL C-assignment SUs extract AES/GES + MHz + spotbeam but not `INIT_EIRP`/`REFNO`; ⚠️ R-channel SEQINDICATOR + T-burst control-ISU framing unverified.
- **Application** — ✅ HAVE ACARS over reassembled T-burst → ADS-C/CPDLC; ⚠️ multi-burst defrag unverified (JAERO flags it TODO too); ❌ MISSING telephony/call-progress decode.
- **Demod** — ⚠️ freq-discriminator MSK ~2 dB below JAERO coherent; ✅ HAVE per-burst CFO + energy gate, both 600/1200; ❌ MISSING 10.5k A-QPSK burst; ❌ MISSING off-air/synthetic C-band sensitivity numbers (loopback BER-0 only); 💡 no AFC/DCD state machine.
- **Outputs** — ❌ MISSING feed/output integration entirely; ❌ MISSING mode mislabeling fix (always `AeroL`); 💡 no `bit_rate`/`R-vs-T`/`channel` discriminator.
- **Improvement ideas** — merge `aero-c` into `aero` as a PHY-selected burst sub-decoder (kills the unwired mode + the `AeroL` mislabel); typed SU classifier shared by P/R/T; C-channel descrambler `dl2`; emit `bit_rate`+channel tag + write `docs/notes/AERO.md` documenting the unreceivable-return-link reality; verify SEQINDICATOR/control-ISU against JAERO source.
- **Verify note:** the two `aerol.cpp`/`aerol.h` WebFetch passes returned slightly inconsistent hex for individual `AEROTypeP/R` enumerators — treat the *set of named types* as authoritative, confirm exact hex against JAERO source before encoding a SU-type table.
- **Files:** `crates/xng-mode-aero/src/{burst.rs,su.rs,cchannel.rs}`, `crates/xng-mode-aero/PROVENANCE.md`, `crates/xng-types/src/mode.rs`.

---

### A6. Inmarsat STD-C / EGC

> **References:** inmarsatc (cropinghigh, GPL Scytale-C port — closest OSS) + tekmanoid (closed, de-facto commercial). Both decode more C-channel signalling + EGC than xng today.

- **C-channel descriptor parsing** — ✅ HAVE identical descriptor type set. ⚠️ PARTIAL `0x7D` bulletin-board (xng: network_version/frame_number/channel_type; inmarsatc adds signallingChannel, count, channelTypeName, 5 status booleans, **16-bit services bitfield**, randomInterval, UTC-of-day). ❌ MISSING `0x6C` signalling-channel (uplinkMHz + services + 28 TDM slots), `0x83` logical-channel-assignment (status/frame-length/duration/offset/down+up MHz/packetDescriptor1 — needed to actually tune the message channel), `0x92` login-ack (LES station list), `0xAB` les-list, `0xA3`/`0xA8` short-message text, `0x08` ack-request routing.
- **Channel-frequency decode** — ❌ MISSING uplink/downlink MHz from channel-number word (formulas are in xng's own STDC.md but unimplemented).
- **EGC header** — ✅ HAVE service/continuation/priority/msg_seq/pkt_seq/presentation/address. ⚠️ PARTIAL verify single-header `0xB0` vs double `0xB1`+`0xB2` distinction in surfaced details. 💡 frame_number → `seconds_of_day = frame_number × 8.64` UTC-of-day (cheap, high-value).
- **EGC service taxonomy** — ✅ HAVE 15 service codes (superset of inmarsatc's 13). 💡 add canonical long names (operator-friendly).
- **Geographic area-address decode — the single biggest gap; no open decoder does it, tekmanoid does**
  - ❌ MISSING — **rectangular** (C2=04/34, SW-corner lat/lon + N/E extension), **circular** (C2=24/44/14, center + radius nm), **NAVAREA/METAREA number** (C2=31, 01–21), **coastal/NAVTEX** (C2=13/73). xng carries raw hex only.
  - ❌ MISSING — geographic **map plotting** of areas/coordinates.
- **Text/presentation** — ✅ HAVE IA5 + printable heuristic + hex fallback (more than inmarsatc). ❌ MISSING **ITA2/Baudot (presentation 6)** (inmarsatc also lacks — opportunity to lead); ⚠️ presentation 7 binary as hex only (tekmanoid saves typed binary artifacts).
- **Identity resolution** — ⚠️ PARTIAL ocean region + numeric LES id but **no LES operator name table** (inmarsatc ships ~40–60-entry `getLesName()`); 💡 full ocean-region long names.
- **Reassembly** — ✅ HAVE EGC part assembly + per-LCN concat + recursive 0xBD/0xBE multiframe (parity). 💡 align ~69 s retention to documented ~30 s intent.
- **Distress** — ⚠️ PARTIAL priority decoded + 0x91/0xA0 named, but no distress-specific position/alerting (tekmanoid plots distress search-areas).
- **Demod** — ✅ HAVE coherent BPSK 1200 sym/s, square-law FFT AFC, Costas, gated Gardner (off-air UW 128/128). ⚠️ 180° ambiguity resolved at UW; verify mid-frame polarity-flip recovery (inmarsatc flags `isMidStreamReversePolarity`). 💡 no blind/adaptive equalizer (inmarsatc CMA, stepSize 0.001); 💡 generic 121-tap LPF vs proper **RRC matched filter** (likely weak-signal win); ⚠️ cold-start Gardner weakness on unfiltered injection; 💡 no stage-by-stage golden validation (vs SatDump `.frm`).
- **Outputs** — ⚠️ no per-frame BER (inmarsatc surfaces `BER = min(nUW,rUW)` UW Hamming distance); ❌ `fec_corrected`/`errors` always None; ⚠️ SNR/noise unset; 💡 CRC-failing packets silently dropped (expose a counter); 💡 emit decoded area geometry as structured fields once area decode lands.
- **Best competitor (tekmanoid) edges** — LES message-channel decode (paid tier: ship email/telex traffic, not just NCS), EGC area visualization, active-LES-list, typed binary capture. **vs inmarsatc:** xng matches the descriptor set + beats on text heuristics/service-codes but trails on per-descriptor field depth + LES names + BER.
- **Improvement ideas** — implement C2/C3 area-address decoder + structured geometry (highest leverage, nothing in OSS has it); bundled LES/NCS operator name table + ocean-region long names; flesh out bulletin-board/signalling/logical-channel fields + channel-frequency formula; populate `fec_corrected` + per-frame UW BER; frame_number→UTC; ITA2/Baudot; RRC matched filter + optional CMA + SatDump `.frm` goldens; follow `0x83` to demod the LES message channel (closes biggest functional gap vs tekmanoid).

---

### A7. Iridium

> **References:** iridium-toolkit (muccc, the reference) + iridium-sniffer (alphafox02, strongest non-xng single-process). xng leads on weak-frame *production*/sensitivity and far exceeds sniffer's frame-type *parsing*.

- **Frame-type typing** — ❌ MISSING **AQ acquisition uplink** (`IridiumAQMessage`), **NXT** (`IridiumNXTMessage`); ⚠️ PARTIAL ISY sync typed but verify the `"10"` vs `"11"` UL pattern distinction.
- **IBC/broadcast** — ✅ HAVE type-0 sat/beam/slot/acq + assignments + info sub-block. ⚠️ verify non-zero `bc_type` sub-blocks.
- **IRA / IMS / IP / Voice** — ✅ HAVE IRA (sat/beam/ECEF/lat/lon/alt/interval/EPI/pages + END), IMS paging (ASCII/BCD/Raw), IIP/IIQ/IIR/IIU IP frames, VDA/VO6/VOD/VOZ/VOC voice classify + RS6/RS8. AMBE intentionally bytes-only.
- **GSM layer-3** — ✅ HAVE CC/MM/SMS + Mobile Identity (IMSI/IMEI/TMSI) + LAI + cause. ⚠️ PARTIAL **RR messages** (Immediate Assignment/Paging/System-Info) — verify xng labels these vs raw_l2. 💡 emit a **PCAP** file (`-m lap`) for offline Wireshark.
- **SBD/ACARS / ITL / mt-position** — ✅ HAVE Layer-A+B reassembly, MO HELLO, 0x76xx transfer, ACARS-over-SBD → shared app layer; ITL via vendored PRS tables (exceeds toolkit); mt-position ECEF from paging.
- **Critic additions — Iridium upper layers / current events**
  - ❌ MISSING — **PPP-PAP credential frames + HTTP Basic-Auth headers in plaintext IP sessions** (88.5% of Iridium frames unencrypted; 2026 security analysis recovers usernames/passwords/admin logins in clear from IIP/IIR IP payloads). xng decodes IIP/IIQ/IIR framing but not the PPP/IP upper-layer content — a meaningful new decode target.
  - ⚠️ PARTIAL — **LCW (Link Control Word) layer + COMP128-1 / GSM-management auth messages**: verify whether `frame.rs` exposes LCW beyond voice-frame classification.
  - ❓ VERIFY (correctness, not feature) — **Iridium time re-epoch** (ERA3 = 2025-02-14; next re-epoch **2026-01-14 18:08 UTC**): IRA/IBC `iri_time` and SGP4/TLE correlation must handle the LBFN epoch roll — check xng's satellite-naming/time code for re-epoch correctness (live bug risk).
- **Demod** — ✅ HAVE faithful gr-iridium port + beyond-gr refinements (**758 vs 573 CRC-OK** lead over gr-iridium; two-filter union, per-frame CFO, peak-relative EOF). ❌ MISSING **soft-decision / Chase decoding** (sniffer `--chase`, toolkit `--harder`/`--uw-ec`; xng BCH is hard ≤2-flip — the next weak-frame lever); ❌ MISSING **GPU/OpenCL/Vulkan** burst detection; ⚠️ explicit SIMD kernel select (`--simd`); 💡 UW error-correction pre-classify; 💡 SigMF annotation.
- **Outputs** — ✅ HAVE Iridium JSON, ACARS-over-SBD on all sinks, GSMTAP UDP, satellite naming (TLE/SGP4), web dashboard 48-beam reconstruction, Prometheus. ❌ MISSING **BaseStation/SBS for Iridium-derived positions** (sbs.rs ModeS-only — ADS-C/ACARS positions from SBD never reach SBS/tar1090/VRS; sniffer `--basestation` sources ADS-C→ACARS→waypoint→beam-estimate); ❌ MISSING KML export, web-map MT/aircraft layers (xng *decodes* mt-position but doesn't render it — verify), burst-IQ capture; 💡 IBC-driven PPM clock self-calibration (`-m ppm`).
- **Best competitor edges** — sniffer: Doppler self-geolocation (`--position`), `--basestation` 4-tier, `--chase`, GPU, `--simd`, `--save-bursts`, tar1090 aircraft DB. toolkit: `mkkml`, `-m ppm`, `-m tdoa` multilateration, `-m lap` PCAP, `--harder`/`--uw-ec`, voice-cluster tooling, beam-reception plotter.
- **Improvement ideas** — soft-decision/Chase BCH on IDA→LCW/RA/IBC blocks; wire Iridium ADS-C/mt-position into SBS + map (extend sbs.rs beyond ModeS); KML/SigMF/burst-IQ export; IBC PPM clock cal; AQ/NXT/richer ISY typing; multi-receiver TDOA (xng already has IRA ECEF + IBC iri_time primitives). *AMBE framing note (Appendix verify): the gap is codec legality, not capture — voice is unencrypted by default.*
- **Files:** `crates/xng-mode-iridium/src/{frame,ira,ms,demod,wideband}.rs`, `src/outputs/sbs.rs` (ModeS-only — confirms the position gap), `docs/notes/IRIDIUM.md`.

---

### A8. AIS (ITU-R M.1371)

> **Gold-standard:** AIS-catcher (jvde-github). gnuais/rtl-ais/aisdeco2/gr-ais are FM/position-only — xng already exceeds them.

- **Top-level dispatch (1–27)** — ✅ HAVE full range; every type exposes msg_type+mmsi even when body undecoded.
- **Parsed but not field-decoded** — ⚠️ PARTIAL type 10 (dest_mmsi), 15 (interrogation slots), 16 (assignment-mode), 25/26 (binary — surfaced with no dac/fid/payload, not even raw hex).
- **Position/identity body** — ✅ HAVE Class A 1/2/3 (nav_status, sog/lat/lon/cog/heading w/ sentinels), Class B 18/19, long-range 27, SAR-aircraft 9. ⚠️ PARTIAL type 5 omits eta/dte/epfd/dimensions/position_accuracy; types 1-3 omit **rot/timestamp/maneuver/raim/radio comm-state**; type 21 omits **name extension**/off_position/virtual/dims/epfd/raim; type 4/11 omit epfd/raim/radio; type 24B omits vendor_id/model/serial/dims/mothership_mmsi. ❌ MISSING **ROT decode helper** (`4.733·√rot`).
- **ASM (DAC/FID binary on 6/8/25/26)** — ❌ MISSING **zero ASM decoding** (the #1 gap; AIS-catcher decodes **~40 subtypes**). DAC=1 IMO SN.1/Circ.289 (FID 31/11 meteo-hydro, 21 weather-from-ship, 16 POB, 22/23 area-notice, 17 VTS-targets, 24/25/26 ship-static/cargo/sensor, 27-30 route/text, 32 tidal); DAC=200 Inland (10/21/22/23/24/40/55); regional DACs (235/250/366 AtoN-monitoring, 316/366 Seaway-meteo, 367 US-environmental, 265 STM-route).
- **Reassembly/tracking** — ❌ MISSING **multi-fragment AIVDM reassembly across sentences** (each frame decoded independently — long type 5/6/8/26 never merged before field-decode; note: outgoing >60-char *encoder* fragmentation is ✅ HAVE — inverse); ❌ MISSING **type 24 Part A+B merge** by MMSI; ❌ MISSING type-5 fragment merge into one voyage record; ❌ MISSING `AISTracker`-style per-MMSI state aggregation.
- **Demod** — ✅ HAVE genuinely strong coherent path: 24-bit complex template, ±1200 Hz CFO grid + fractional timing, **16-state GMSK-exact MLSE Viterbi**, CFO×gain fan-out, **successive interference cancellation** (SIC — most open decoders lack it). ⚠️ PARTIAL **48 vs AIS-catcher 53 = 91%** on the weak capture (AIS-catcher's `-m 2`/`-m 4` still lead at the noise floor). 💡 FM-discriminator path needs ~14 dB SNR. ✅ two-sighting MMSI-confirmation gate + ≥25% SIC energy-fit → **zero false decodes** (real edge); ✅ soft-bit Chase repair **falsified** (forged foreign-MMSI under weak FCS-16) — well-founded "do-not-retry", not a gap. 💡 low-CPU Pi mode (1.4× headroom is thin); ❓ verify CIC5 droop compensation in xng-dsp.
- **Outputs** — ✅ HAVE bit-exact NMEA AIVDM + **TCP server**. ❌ MISSING **NMEA-over-UDP** (README "UDP + TCP" overstates — UDP is ACARS/GSMTAP only; UDP is the dominant hobby transport), AIVDO own-ship, NMEA tag-blocks, GPSd output/fusion, NMEA2000/N2K, HTTP aggregator push; ⚠️ JSON `details` is thin (no ASM) vs AIS-catcher JSON_FULL. ❌ MISSING per-type/MMSI filtering + rate downsampling (`position_interval`/`own_interval`) + output dedup (`unique on`). ✅ HAVE A=161.975/B=162.025 from one capture; ⚠️ `channel_letter` **silently defaults unknown freqs to 'A'** (should flag, not guess).
- **Best competitor (AIS-catcher) edges** — full ASM (~40 DAC/FID), built-in web viewer/map server (`-N`), community feed (`-X`), PostgreSQL/PostGIS, MQTT/SBS/N2K sinks, deeper noise-floor decode (91% of it on the shared capture). **xng edge to keep:** SIC + GMSK-exact MLSE + two-sighting acceptance (AIS-catcher documents none of these).
- **Critic addition (folded into A8 + Part D):** **AIS-SART / MOB / EPIRB-AIS distress tagging** — these are ordinary Msg 1 (nav_status 14) + Msg 14 text on AIS1/AIS2; xng already demods the band and parses nav_status (`NAV_STATUS[14]="AIS-SART"`) and Msg 14 text — so the *bits are captured*. What's MISSING is **classification/alerting**: detect SART/MOB/EPIRB-AIS by MMSI prefix (**970=SART, 972=MOB, 974=EPIRB-AIS**) and surface "MOB ACTIVE"/"MOB TEST" as a distress event. ✅ TRIVIAL — presentation/tagging over data xng already decodes; no new DSP. (verify xng maps 970/972/974 — appears NOT today.)
- **Improvement ideas** — DAC/FID ASM dispatch table (start DAC=1 FID 31/11/21/16/22-23/27-30, then DAC=200; validate vs pyais oracle); multi-fragment reassembly; NMEA-over-UDP + fix README; fill easy type-5/1-3/21/24B sub-fields + ROT; NMEA tag-blocks + AIVDO; per-MMSI downsampling + filtering; low-CPU Pi mode; VDES/AIS 2.0 (ITU-R M.2092) readiness (ASM channels AIS 3/4 = ch 75/76, SOLAS ~2028); rot/comm-state decode.
- **Files:** `crates/xng-mode-ais/src/{fields.rs,coherent.rs,demod.rs,frame.rs,nmea.rs}`, `crates/xng-mode-ais/PROVENANCE.md`, `docs/notes/BENCHMARKS.md`.

---

### A9. Mode S / ADS-B (1090 MHz)

> **References:** readsb (wiedehopf) for outputs/state-machine; pyModeS + rs1090/jet1090 for BDS/TC coverage (rs1090 covers BDS 10/17/18/19/20/21/30/40/44/45/50/60/61/62/65 + ASTERIX CAT48 — the richest matrix to benchmark against).

- **Downlink Format (DF) coverage** — ✅ HAVE DF17/18 ES (clean PI parity, AA-address, single-bit repair), DF11 all-call, DF0/4/16/20 (AC altitude), DF5/21 (squawk) address-overlaid gated on cached ICAO. ⚠️ PARTIAL DF20/21 Comm-B (only 4 BDS inferred). ❌ MISSING **DF19 (military ES)** (rs1090 parses it), **DF24-27 Comm-D ELM** (xng rejects DF≥24). 💡 IMPROVE DF4/5/20/21 carry FS/DR/UM (xng drops FS alert/SPI/ground that readsb surfaces).
- **ADS-B Type Code (TC) coverage** — ✅ HAVE TC1-4 callsign, TC19 velocity subtypes 1-4. ⚠️ PARTIAL **TC5-8 surface: movement (ground speed) + ground track NOT decoded but present in-message** (readsb emits gs+track); TC9-18 Q=0 Gillham ES altitude left None; TC20-22 geometric altitude undecoded (readsb `alt_geom`). ❌ MISSING **TC28 aircraft status** (emergency/priority + 1090ES ACAS RA broadcast), **TC29 target state** (MCP/FCU selected alt, QNH, selected heading, autopilot/VNAV/approach/LNAV flags), **TC31 operational status** (ADS-B version, NIC-sup, NACp, NACv, SIL, GVA), TC0/TC23-27.
- **Accuracy/integrity suite (NUC/NIC/NAC/SIL) — entirely absent** — ❌ MISSING NUCp (v0), NIC+supplement, NACp, NACv, SIL, SDA, GVA, Rc; ❌ MISSING **ADS-B version inference** (needed because NIC/NACp/SIL meanings are version-dependent). *This is the modern data layer xng lacks entirely.*
- **Velocity refinements** — ⚠️ PARTIAL VR source bit (GNSS vs baro) not distinguished; geom-minus-baro altitude diff not surfaced; supersonic ×4 only; NACv not emitted.
- **Comm-B BDS registers**
  - ELS — ❌ MISSING BDS 1,0 data-link-capability, BDS 1,7/1,8/1,9 GICB capability; ✅ HAVE BDS 2,0 ident; ❌ MISSING **BDS 3,0 ACAS/TCAS RA** (high safety/intel value), BDS 2,1 registration markings.
  - EHS — ✅ HAVE BDS 4,0 (selected vertical intention), 5,0 (track & turn), 6,0 (heading & speed). 💡 IMPROVE BDS 5,0/6,0 disambiguation: xng's "exactly-one-validates" vs pyModeS `is50or60` cross-check + rs1090 density-scoring/penalty arbitration (recovers ambiguous frames xng drops).
  - Met (MRAR/MHR) — ❌ MISSING BDS 4,4 (wind/temp/pressure/turbulence/humidity → readsb wd/ws/oat/tat), BDS 4,5 (hazard), BDS 5,3 (air-referenced state).
  - 💡 IMPROVE — extend inference beyond 4 registers; adopt rs1090 `infer(mrar=True)` log-density + cross-field-penalty scoring vs binary validate.
- **Critic addition — DF18 CF-field subtypes** — ⚠️ PARTIAL: xng treats all DF18 as plain ES, missing the **CF=0 ADS-B non-transponder / CF=2/3/5 TIS-B fine/coarse/management / CF=6 ADS-R rebroadcast** distinction that readsb/dump1090 already classify — loses the TIS-B/ADS-R `source` tag on the 1090 feed xng *does* decode.
- **Demod** — ✅ HAVE magnitude PPM, integer + fractional (2.4 MS/s) paths, multi-phase sub-sample sweep (**164 vs readsb 167 = 98%**, beats dump1090-fa), single-bit CRC repair, two-sighting ICAO confirmation (70→0 phantoms). 💡 IMPROVE readsb's residual ~3-frame edge = **phase-classified per-phase bit templates** (BENCHMARKS already names this). ❌ MISSING **Mode A/C** decode (`--modeac`); 💡 graduated position trust (`--json-reliable`, `--position-persistence`, NIC-aware acceptance) vs xng's binary cache+speed-gate.
- **Outputs** — ✅ HAVE SBS-1 (MSG 1/3/4/5/6), Beast binary (port 30005, 12 MHz MLAT counter, RSSI byte), JSONL, asf-2.0, MQTT, web dashboard (tar1090/Mictronics). ⚠️ PARTIAL Beast 12 MHz counter synthesized from message timestamp (not a stable RX clock — see MLAT). ❌ MISSING readsb-schema **`aircraft.json`** (version/nic/nac_p/nac_v/sil/gva/emergency/nav_*/acas_ra/wind/oat — because the TCs/BDS aren't decoded); ❌ MISSING `docs/notes/ADSB.md`.
- **UAT 978 MHz — entirely missing** — ❌ UAT ADS-B downlink (978-equipped GA), FIS-B weather (NEXRAD/METAR/TAF/PIREP/AIRMET-SIGMET/NOTAM/winds-temps/TFR/SUA), TIS-B + ADS-R (dump978 `uat2esnt` → DF18 CF=6 is a clean integration path). *See Part D — UAT is the #1 new-aviation candidate.*
- **MLAT — missing/unverified** — ❌ MISSING MLAT client/server role; ⚠️ VERIFY whether an mlat-client could sync against xng's synthesized-counter Beast feed (true MLAT needs forwarding all Mode-S incl. position-less DFs with a monotonic RX clock). *Critic sharpening (Appendix): ultrafeeder's mlat-client tolerates non-GPS clocks via server clock-sync — the blocker may be jitter/monotonicity from system-clock-derived timestamps, not the absence of GPS. Sharpen before citing as a hard blocker.* ❌ MISSING `mlat`/`tisb` provenance flag (xng always sets crc_ok=true, no source distinction).
- **Improvement ideas (highest leverage first)** — TC31 operational-status (unlocks version + NIC/NACp/SIL → real per-fix quality); TC5-8 surface movement/track + TC29 target-state (bits already present); BDS 3,0 (ACAS RA) + 4,4/4,5 (met) into the inference set; rs1090-style density/penalty BDS scoring; Mode A/C; emit readsb-schema `aircraft.json`; per-phase bit templates; true RX-clock Beast timestamps → MLAT-feedable; classify DF18 CF subtypes for TIS-B/ADS-R source tags.

---

## Part B — Cross-cutting: ecosystem / outputs / tooling gaps

> xng's per-mode decode is strong; its operability/ecosystem surface trails the readsb/acarshub/AIS-catcher/tar1090 world. References inline.

- **1. Persistence, history & replay**
  - ❌ MISSING — on-disk message DB (xng keeps RAM `VecDeque` cap 200 + append-only JSONL, no query). acarshub: SQLite + retention (`DB_SAVE_DAYS` 7d, alerts 120d) + search UI.
  - ❌ MISSING — per-aircraft trace history (readsb `traces/HEX/` + `globe_history`); BaseStation-schema flight/aircraft DB (VRS DatabaseWriter).
  - ⚠️ PARTIAL — xng replays IQ files but the **dashboard has no time-scrubber/replay of decoded history** (tar1090 `?replay`/`?pTracks`).
  - ❌ MISSING — position-density heatmap (tar1090 `?heatmap`; AIS-catcher shipping-lane density).
- **2. Coverage / range analytics** — ❌ MISSING measured/theoretical range outline (upintheair.json / HeyWhatsThat / graphs1090 polar), range rings (tar1090 `rangeRings`), distance/RSSI columns + per-entity reception; ❓ verify xng has a station marker/receiver-position pin at all.
- **3. Alerting & notifications** — ❌ MISSING watchlists (tail/flight/ICAO/MMSI/label/text; acarshub Alert terms + 120d DB), special-category highlighting (readsb `dbFlags` mil/interesting/PIA/LADD + emergency-squawk 7500/7600/7700), push (Discord/Telegram/webhook/sound), MQTT **Home Assistant autodiscovery** (xng MQTT is raw JSON to `xng/<mode>` only).
- **4. REST / data API** — ❌ MISSING `/data/aircraft.json` + `/data/receiver.json` (readsb schema — would make xng drop-in for tar1090/graphs1090/HA); ⚠️ only `/api/state` full-snapshot poll (no filtered/incremental `?since=` like AIS-catcher, no historical search-by-field); 💡 no WebSocket/SSE/delta (re-polls full snapshot every cycle).
- **5. Export formats** — ❌ MISSING GeoJSON/KML/GPX export, `?screenshot` kiosk + shareable deep-link state.
- **6. Map/dashboard UX** — ⚠️ single hardcoded CDN basemap (no switcher/overlays/offline MBTiles/weather); ⚠️ RAM-only trails cap 60 pts decimated, lost on restart; ⚠️ map/table have no alt/speed/type/callsign/source filters (tar1090 has all); ❌ no measure/ruler, gridlines, on-map labels, box-select, `tableInView`, column sort, dim/contrast/icon-scale controls.
- **7. Statistics & graphs** — ❌ MISSING built-in graphs page (msgs/min, per-freq, signal-level histogram, CRC/error %, CPU/temp/uptime — graphs1090/acurashub Statistics); ⚠️ Prometheus depth thin (only `xng_frames_total`, `_crc_ok_total`, `_channel_level_dbfs`, `_samples_total` — missing per-mode/type counts, aircraft/vessel gauges, decode-latency, FEC-corrected, reassembly, dropped/lagged-bus); 💡 no first-party Grafana dashboard JSON; ⚠️ per-message-type/label breakdown absent from stats view.
- **8. Health / watchdog / operability** — ⚠️ per-session last-message-age shown but no health state machine / no-data alarm / auto-restart; ❓ verify continuous autogain during `listen` (only one-shot `survey --tune-gain`); ❌ no web config/management UI (TOML-only).
- **9. Multi-receiver aggregation, dedup & MLAT** — ⚠️ `xng ingest` (asf-2.0) terminates many stations but is a feed terminator, not a fan-in dashboard/dedup hub; ❌ no cross-receiver dedup (acarshub dedups; AIS-catcher `unique on`); ❌ no MLAT (Beast counter is system-clock-derived, not RX-clock — 💡 emit GPS/sample-clock-disciplined timestamps to be MLAT-usable; see A9 critic sharpening on jitter-vs-GPS).
- **10. Marine / other integrations** — ❌ MISSING SignalK delta output, NMEA2000/N2K, NMEA tag-blocks + community-map push, GPSd input (mobile-station positioning).
- **11. Enrichment data** — ⚠️ user CSV aircraft DB (Mictronics) + country tables but no bundled/auto-updating DB and no `dbFlags`; ❌ no route/airline/operator/airport enrichment on dashboard; ⚠️ vessels get MID-country + ship-type but no persistent registry across restarts.
- **12. Smaller dashboard/quality** — 💡 flat 300 s entity expiry (slow ADS-C/HFDL aircraft vanish too fast → make per-mode); 💡 message stream cap 200/100 (add scrollback backed by ring file); 💡 no dark/light/scale controls; ❓ verify non-Iridium trail antimeridian/date-line wrapping.

- **Critic additions — competitor projects + aggregators not in corpus**
  - 💡 IMPROVE — **BelugaProject** decodes/maps **ADS-B *and* AIS together** in one web app from local feeders — direct prior-art for xng's cross-mode unified dashboard ambition (corpus named tar1090/acarshub/AIS-catcher but not the one tool already fusing air+sea).
  - 💡 IMPROVE — aggregator-network targets to feed/dedup against absent from the feeding design: **airplanes.live, adsb.lol (ODbL), adsb.fi, OpenSky** — relevant to the cross-mode multi-receiver story.

- **Cross-mode opportunities (critic — the highest-leverage structural bets)**
  - 💡 **Unified entity model spanning air + sea (+ rail/space)** — one `Entity{kind: aircraft|vessel|sat|beacon, id: ICAO|MMSI|IMEI|HexID, positions[], identities[], source_modes[]}` ties ADS-B + HFDL/Aero/Iridium ADS-C + AIS into one track store. Then **one mode-agnostic position→SBS/Beast adapter** keyed on the entity replaces the four separate per-mode wirings the corpus flags individually (HFDL/Iridium/Aero positions never reaching SBS).
  - 💡 **Shared ICAO/registration resolver + dbFlags** — aircraft-ID→ICAO is solved independently in 5 modes (HFDL ac_cache, Iridium ADS-C, ADS-B AA, ACARS tail↔ICAO). One shared resolver (tail↔ICAO↔reg↔operator↔dbFlags mil/PIA/LADD) serves ACARS/VDL2/HFDL/Aero/Iridium/ADS-B at once.
  - 💡 **Multi-receiver geolocation as one primitive** — Iridium TDOA (`-m tdoa`), Iridium Doppler self-position, and ADS-B MLAT are *one* multi-receiver-timestamp + solver primitive over the asf-2.0 fan-in. A shared TDOA/MLAT engine keyed on the unified entity is one network-scale feature, not three.
  - 💡 **Cross-mode dedup + shared distress overlay** — one dedup keyed on `(entity_id, content_hash, time-window)` covers ADS-B multi-RX, AIS `unique on`, and the ecosystem fan-in. Pairs with a **cross-mode distress/emergency overlay**: ADS-B 7500/7600/7700 + TC28 emergency + AIS-SART/MOB (MMSI 970/972/974) + STD-C EGC distress + (future) DSC/COSPAS-SARSAT 406 → one alerting surface.
  - 💡 **Shared per-burst signal-quality schema** — `SignalQuality{signal dBFS, noise dBFS, SNR, CFO-ppm, FEC-corrected}` populated by *every* demod closes the gap flagged separately in ACARS, VDL2, HFDL, STD-C, Iridium, Aero, and AIS — the single highest-leverage cross-mode *output* fix (mirrors dumpvdl2's `[-18.9/-43.9 dBFS][25 dB][0.4 ppm]` line).

- **Files:** `src/outputs/http.rs` (dashboard + `/api/state`), `src/outputs/assets/dashboard.html`, `src/outputs/metrics.rs`, `src/outputs/{mqtt.rs,sbs.rs,beast.rs,nmea_tcp.rs,dbinfo.rs}`.

---

## Part C — Per-decoder Airframes feeding feature design

> Feature design for per-mode ingest routing + station-id override + per-decoder disable. Today xng feeds **only ACARS** to `feed.airframes.io:5550`; every other mode decodes locally and reaches Airframes only via asf-2.0. **Each Airframes ingest expects that mode's native wire format** (the canonical decoder's output) — *not* acarsdec JSON for everything. **asf-2.0 is the sole exception:** its `AirframesFeed/Stream` schema (`proto/asf2.proto`) self-describes every mode in one connection, so a mode rides asf-2.0 regardless of whether a legacy per-port ingest exists. **Ports/formats below are verified against primary Airframes sources (2026-06)** — see confidence column; unverified rows must NOT be hard-coded as public endpoints.

### C(a) Researched Airframes per-mode ingest / port / format / station-id table

| Mode | Public Airframes ingest? | Transport · endpoint | Wire format | Confidence (source) |
|---|---|---|---|---|
| **VHF ACARS** | ✅ Yes (dedicated) | UDP `feed.airframes.io:5550` | acarsdec flat JSON (`-j`/`-o 4`) | **confirmed** (docs `feeding/how`; matches xng `acarsdec_json.rs`) |
| **VDL2** | ✅ Yes (dedicated) | **UDP `feed.airframes.io:5552`** (dumpvdl2 direct) · **TCP `feed.airframes.io:5553`** (dumpvdl2 acars_router formatted) | dumpvdl2 `decoded:json` (vdlm2dec flat JSON also accepted) | **confirmed** (docs; acars_router README; protocol-examples) |
| **HFDL** | ✅ Yes (dedicated) | **TCP `feed.airframes.io:5556`** (dumphfdl direct; acars_router also TCP `:5556`) | dumphfdl nested `decoded:json` (`{hfdl:{…lpdu…hfnpdu…}}`) | **confirmed** (docs; acars_router README) |
| **AIS** | ✅ Yes (dedicated) — **HTTP, not 555x** | **HTTP `http://feed.airframes.io:5599`** | AIS-Catcher **`PROTOCOL AIRFRAMES`** HTTP POST (NMEA AIVDM + metadata; `-H … ID <station> INTERVAL 15`) | **confirmed** (Airframes Community "AIS-Catcher HTTP ingest is LIVE"; AIS-catcher HTTP docs) |
| **Inmarsat Aero L (IMSL)** | ✅ Yes (dedicated) | **UDP `feed.airframes.io:5571`** also ASF2 asf-2.0 to Airframes | JAERO / SatDump Inmarsat JSON | **confirmed** (Airframes About page) |
| **Inmarsat Aero C** | ✅ Yes (dedicated) | **UDP `feed.airframes.io:5571`** also ASF2 asf-2.0 to Airframes | JAERO / SatDump Inmarsat JSON | **confirmed** (Airframes About page) |
| **Inmarsat STD-C / EGC** | ❓ No public ingest found | — | — (no canonical Airframes decoder format published) | **unverified** (absent from all sources) |
| **Iridium (IRDM)** | ❓ **No stable public port; feeding explicitly "unsettled"** | `:5558` is a **LOCAL** acars_router/acarshub listener only | iridium-toolkit `reassembler.py` ACARS-shaped JSON (`source.transport:"iridium"`) | **confirmed** — thebaldgeek (cited by official docs): standard UDP "WON'T work for airframes.io" (June 2024) - look at the airframes feeder script for details in the iridium-toolkit project |
| **Mode S / ADS-B** | ❌ **Not fed to Airframes** | Beast `:30005` / SBS `:30003` are **LOCAL servers** for *other* aggregators (FR24/ADSB.lol/…) | Beast binary / SBS BaseStation CSV | **confirmed-absent** (no Airframes ADS-B feeder port in any source) |

- **Canonical port scheme** — acars_router's own *ingest listeners* are ACARS 5550 · VDLM2 **5555** · HFDL 5556 · IMSL 5557 · IRDM 5558 (each UDP+TCP). Note the trap: acars_router *listens* on 5555 for VDLM2 but **SENDs to Airframes on 5552/5553** — so 5557/5558 being listener ports does **not** imply matching public Airframes endpoints (they aren't).
- **Station-id convention** — `XX-YYYY-MODE` (operator initials, nearest ICAO, mode suffix), e.g. `KE-KSEA-ACARS`, `AU-YMML1-VDL2`, `XX-KMHR3-HFDL1`; name or UUID, ≥36 chars allowed. ❌ MISSING per-mode suffix: Airframes expects a **distinct id per mode** from one site; xng stamps ONE global id, so VDL2 frames would carry the `-ACARS` id. 💡 IMPROVE auto-derive the suffix from a base id (no competitor does this; acars_router forces each `FEED_ID` by hand).
- **Why per-mode native format matters** — acars_router fans out by class to `AR_SEND_{UDP,TCP}_{ACARS,VDLM2,HFDL,IMSL,IRDM}`; acarshub composes acarsdec+dumpvdl2+dumphfdl each emitting its own decoder's JSON with its own `FEED_ID`. Each ingest validates the format it expects — feeding acarsdec JSON to the VDL2 port would be rejected/mis-parsed.

### C(b) Gap analysis — xng today

- ⚠️ PARTIAL — `--feed-airframes` appends ONLY `feed.airframes.io:5550` to a shared `udp` list (`main.rs:21,185,753`); no 5552/5553/5556, no AIS HTTP 5599.
- ❌ MISSING — the UDP output runs one formatter `format_acarsdec` that returns `None` for every non-ACARS body (`acarsdec_json.rs:12-14`) — even if 5552 were a target, VDL2 would drop. **No per-mode native serializer and no per-mode→per-ingest routing.**
- ⚠️ PARTIAL — single global `StationFile.station_id` copied verbatim into every `SessionConfig.station_ident` → every message's `station_id`; no per-session/per-mode override.
- ❌ MISSING — per-decoder enable/disable (feeding is all-or-nothing per process).
- **Native-serializer readiness** (what xng can reuse vs must write):
  - ✅ `acarsdec_json.rs` — acarsdec flat JSON → already feeds **ACARS :5550/UDP**.
  - ⚠️ `nmea_tcp.rs` emits NMEA AIVDM but as a **pull-style TCP server** (OpenCPN/pollers) — the Airframes AIS path is an **HTTP POST in AIS-Catcher `AIRFRAMES` format**, which xng does NOT emit; a small HTTP poster is needed (or just use asf-2.0).
  - ⚠️ `beast.rs` (:30005) / `sbs.rs` (:30003) target **other** aggregators, not Airframes (no Airframes ADS-B ingest exists).
  - ❌ Must write to use legacy per-port ingests: **dumpvdl2 `decoded:json`** (:5552/5553), **dumphfdl `decoded:json`** (:5556). IMSL/IRDM/STD-C/Aero-C have **no public port** → asf-2.0 is the path.
- ✅ HAVE in our favor — asf2 (gRPC+QUIC) already carries every mode + ident (per-mode requirement already satisfied on asf2); MQTT already publishes per-mode topics (`<prefix>/<mode>`) — proves the runtime can branch by mode; `OutputsToml` is already cloned per-session.

### C(c) Proposed TOML schema + threading

```toml
station-id = "KE-KSEA"            # base; per-mode suffix auto-appended

[outputs.airframes]
enabled = true
# station-id = "KE-KSEA"          # optional; defaults to top-level
auto-suffix = true                # 💡 no competitor has this

[outputs.airframes.acars]         # -> feed.airframes.io:5550 UDP, id KE-KSEA-ACARS
enabled = true

[outputs.airframes.vdl2]          # -> :5552 UDP (or :5553 TCP), id KE-KSEA-VDL2
enabled = true
station-id = "KE-KSEA-VDL2-EXP"   # explicit override beats auto-suffix

[outputs.airframes.hfdl]
enabled = false                   # decode locally, do NOT feed (per-decoder disable)

[outputs.airframes.ais]           # -> HTTP feed.airframes.io:5599 (AIS-Catcher AIRFRAMES body)
enabled = true

[[session]]
mode = "acars"
sdr = "driver=rtlsdr,serial=00000001"
channels = ["130.025", "131.550"]

[[session]]
mode = "vdl2"
sdr = "driver=airspy"
feed = false                              # session-level kill-switch (overrides mode block)
airframes-station-id = "KE-KSEA-VDL2-ANT2"  # session-level id override
```

- **Threading (implementation-ready, no code written)**
  - `station.rs` — add `OutputsToml.airframes: Option<AirframesToml>`; new `AirframesToml { enabled, station_id: Option<String>, auto_suffix: bool=true, acars/vdl2/hfdl/ais: Option<AirframesModeToml> }`; `AirframesModeToml { enabled: Option<bool>, station_id, host, port, proto }` where `proto ∈ {udp,tcp,http}`. Add `SessionToml.feed: Option<bool>`, `airframes_station_id: Option<String>`. Keep `deny_unknown_fields`; keep `feed_airframes: bool` back-compat (true → `airframes.enabled`; **only ACARS defaults on for back-compat** — adding VDL2/HFDL/AIS feeds is an opt-in once serializers exist).
  - Resolver — per session compute `Option<AirframesTarget{host,port,proto,station_id}>` by precedence: **session `feed=false` kills it → `[outputs.airframes.<mode>].enabled` (default = block `enabled`) → station-id = session `airframes-station-id` ⟶ mode-block `station-id` ⟶ `auto-suffix(base, mode)` ⟶ base**. Mode→default endpoint from the C(a) table; only `acars/vdl2/hfdl/ais` have a default (others = asf2-only).
  - `runtime.rs OutputConfig` — replace shared `udp: Vec<String>` Airframes coupling with typed `airframes: Option<AirframesTarget>` per-session (already cloned at `main.rs:854`); keep generic `udp` for non-Airframes sinks. The Airframes output task stamps the **mode-specific** id at format time (don't mutate provenance — keeps asf2/jsonl/dashboard on the canonical ident).
  - New emitters beside `acarsdec_json.rs`: **`dumpvdl2_json.rs`** (→ UDP :5552 / TCP :5553), **`dumphfdl_json.rs`** (→ UDP :5556) — each returns `None` for non-matching bodies; and an **AIS `AIRFRAMES`-format HTTP poster** (→ HTTP :5599, batched on an interval like AIS-Catcher's `INTERVAL 15`). UDP/TCP modes spawn one socket per enabled mode→port; AIS uses an HTTP client task.
  - `main.rs run_station_cmd (745-862)` — drop unconditional `udp.push(AIRFRAMES_ACARS_UDP)` (753); build per-session `airframes` targets via the resolver. `OutputOpts::build (166-221)` — keep `--feed-airframes` = "ACARS as today + (when serializers land) auto-suffixed per-mode feeds"; optional later `--feed-airframes-modes acars,vdl2,hfdl,ais`.

### C(d) Edge cases + validation

- **asf-2.0 is exempt and INDEPENDENT** — it carries ALL modes (incl. IMSL/IRDM/STD-C/Aero-C/ADS-B) in one self-describing schema under the canonical `station_ident`, so it is NOT subject to per-mode wire-format conformance and must NOT be routed through the per-mode resolver. A station may run asf-2.0 (all modes, one id) AND per-port legacy feeds (per-mode ids) at once. 💡 **asf-2.0 is the recommended forward path; the legacy per-port feeds exist for the ingests Airframes still runs publicly (ACARS/VDL2/HFDL/AIS).**
- **AIS is HTTP, not UDP/TCP** — the schema/resolver must carry a `proto=http` target and use a batched HTTP-POST task (AIS-Catcher `AIRFRAMES` body), distinct from the datagram path used by ACARS/VDL2/HFDL.
- **No public ingest for IMSL / IRDM / STD-C / Aero-C** — reject (or warn-and-skip) a `[outputs.airframes.<mode>]` block for these with a clear message: "no public Airframes ingest; feed via asf-2.0." Iridium especially is in flux (no settled mechanism).
- **ADS-B / Mode S not fed to Airframes** — Beast/SBS are local server outputs; reject `[outputs.airframes.adsb]` at parse unless/until Airframes confirms an ADS-B ingest.
- **Validation (fail fast at `station.rs::load`)** — error if a feed is enabled but resolved id empty; `feed=false` wins over mode-block `enabled=true` (most-specific); warn if two same-mode sessions resolve to the same id+endpoint (Airframes dedup inflates counts); reject unknown mode sub-tables (**whitelist `acars/vdl2/hfdl/ais`**); `auto-suffix` strips-then-reappends a known suffix (`KE-KSEA-ACARS`+vdl2 → `KE-KSEA-VDL2`).
- **Remaining open items** (sourced as unverified) — whether Airframes will expose public IMSL/IRDM/STD-C ingest ports (today asf-2.0 only); the settled Iridium feeding mechanism; whether the VDL2 port prefers dumpvdl2 `decoded:json` vs vdlm2dec flat JSON (both reportedly accepted — emit dumpvdl2 `decoded:json`).
- **Files:** `src/commands/station.rs`, `src/main.rs` (21, 166-221, 745-862), `src/runtime.rs` (23-70), `src/outputs/{acarsdec_json,nmea_tcp,beast,sbs}.rs`, `proto/asf2.proto`, `docs/ASF2.md`.

---

## Part D — New transportation & RF modes research

> Each verdict = decode feasibility + Airframes mission fit (aviation+maritime data-link). xng today = aviation+maritime only (`xng-mode-{acars,vdl2,hfdl,aero,ais,stdc,iridium,adsb}`), Rust, RTL-SDR/Airspy, MIT-Apache, airframes.io feeding — every new item is at minimum ❌ MISSING in the codebase; the tag reflects whether xng *should* add it.

### D1. Rail / trains

- 💡 **EOT / HOT / DPU telemetry (FRED) — the standout** — EOT uplink 457.9375 MHz, 1200-baud Manchester FSK (NFM ~8 kHz), BCH, **fully clear**; near-clone of xng's existing FSK/Manchester paths. Field decode (unit addr, **brake-pipe pressure**, motion bit, marker light, battery, valve). HOT downlink 452.9375 MHz (incl. emergency-brake arm; AAR S-9152; CVE-2025-1727 confirms unauthenticated). DPU 457.9250 MHz. Competitors stale GPL (PyEOT, EOTDecode, closed SoftEOT). **VIABILITY: HIGH — build first.** RTL-SDR-only, clear-text, position-adjacent telemetry → live train-tail map.
- ⚠️ **ATCS 900 MHz** (wayside↔dispatch) — FSK direct-FM 4800 bps 12.5 kHz, HDLC-LAPB, **clear-text**; carries train location/signal/switch state/track warrants. Competitors Windows-only ATCSMon + GNU-Radio RWMON. **VIABILITY: HIGH** — multi-channel-from-one-capture is xng's wheelhouse; payload protocol-zoo (Spec-200 + ARES + Genisys + SCS-128) is the cost.
- ❌ **AEI railcar RFID** (902–921 MHz, AAR S-918/ISO-10374, passive backscatter) — **NO-GO (passive):** tags only reflect under a trackside interrogator's 2 W flood; nothing on-air to receive from afar.
- ❌ **PTC / I-ETMS** (220 MHz Meteorcomm) — **LOW/NO-GO:** payload **AES-encrypted** end-to-end; even a perfect demod yields ciphertext.
- ❌ **GSM-R / FRMCS / Eurobalise / TETRA-rail / defect detectors** — **NO-GO/LOW:** encrypted (A5/1 GSM-R, 5G FRMCS, TEA TETRA), near-field-only (Eurobalise 27.095 MHz inductive loop), or not-a-data-mode (defect-detector analog voice needs ASR).
- ⚠️ **APRS / open-data (GTFS-RT, OpenTrainTimes)** — APRS = LOW (no real rail traffic); open-data = N/A (ingest/map-enrichment, not an SDR decode).
- **Ranking:** #1 EOT/HOT/DPU, #2 ATCS; rest NO-GO/LOW.

### D2. Road / buses / transit / vehicles

- ⚠️ **TETRA** (public-transit/emergency, 380–430 MHz π/4-DQPSK) — clear-net **control-plane + SDS-LIP position** decode is feasible on xng's DSP (vehicle-ID + GPS without voice); but a large new layered stack (MAC→CMCE→SDS→LIP), EU-centric, most safety traffic encrypted (TEA1 backdoored/recoverable but off-mission/legally fraught; TEA2/3 unbroken). **MEDIUM fit** — best wedge = LIP SDS on clear commercial/transit nets.
- ⚠️ **DMR / P25 / NXDN fleet radio** — well-trodden on RTL-SDR; the airframes-aligned slice is **metadata + embedded GPS** (DMR LRRP, P25 Motorola Unit-GPS, talkgroup/unit-ID), **skip vocoder audio**. GopherTrunk (single ~10 MB Go binary, P25/DMR/TETRA/NXDN/MPT1327/dPMR) proves single-binary feasibility. **MEDIUM-HIGH fit** for position PDUs; trunking control-channel state machine is the lift; encryption is a hard stop where present.
- ❌ **Bus AVL** — not its own RF mode; modern fleets are cellular (off-SDR). Reachable only via the TETRA/DMR/P25 GPS PDUs above. **LOW** standalone.
- ❌ **DSRC / C-V2X (5.9 GHz)** — **NOT VIABLE:** wrong band + OFDM/LTE-sidelink PHY for RTL-SDR; DSRC sunsetting **Dec 14 2026**, C-V2X harder PHY.
- ⚠️ **TPMS (rtl_433, 315/433 OOK/FSK)** — easiest decode class; persistent per-sensor ID = vehicle presence/re-ID at a fixed site. **MEDIUM fit, LOW effort** but tangential to airframes + pulls xng into generic-ISM scope it lacks (verify desire).
- ❌ **Toll transponders (E-ZPass 915 MHz) / taxi MDT** — **LOW:** toll-plaza-only / fragmented proprietary, no airframes value.
- 💡 **GTFS-realtime ingest** — **BEST transit-tracking ROI but non-RF** (Protocol Buffers over HTTP, authoritative bus/train lat/lon from agency AVL). Frame as complement to (not part of) the SDR core.
- ❌ **Keyless entry** — out of scope per task.
- **Ranking:** #1 GTFS-RT (non-RF), #2 DMR-LRRP/P25-Unit-GPS/TETRA-SDS-LIP position PDUs, #3 TPMS; avoid DSRC/C-V2X/toll/MDT/encrypted voice.

### D3. Maritime beyond AIS

- ✅ **DSC — Digital Selective Calling — best first maritime add.** VHF Ch70 (156.525 MHz, FFSK 1200 Bd, BCH(10,7)) + HF/MF (2187.5/4207.5/6312/8414.5/12577/16804.5 kHz, 100 Bd/170 Hz FSK, M.493). The live GMDSS distress layer; reuses xng's VHF AIS + HF stacks. Open refs: GopherTrunk (Go, Apache-2.0), SDRangel (GPL-3); HF refs TAOSW.DSC_Decoder (MIT), YaDD oracle. **multimon-ng has NO DSC** (open since 2015) — xng could leapfrog. **VIABILITY: HIGH.**
- ✅ **NAVTEX (518/490/4209.5 kHz, SITOR-B / CCIR 476, 100 Bd/170 Hz)** — MSI broadcast; simple FSK + time-diversity FEC (shared with HF-DSC). Refs: pd0wm/navtex, fldigi, SDRangel, YaND oracle. **VIABILITY: HIGH.** DSC+NAVTEX = the natural distress pair.
- ✅ **AIS-SART / MOB / EPIRB-AIS tagging** — TRIVIAL; tag MMSI 970/972/974 + nav_status 14 + "MOB ACTIVE" Msg 14 text as distress events over data xng already decodes. (See A8.)
- 💡 **SafetyNET header parsing in STD-C** — incremental polish on existing mode (C1/C2/C3 service-code + area-address; xng carries area address raw today).
- ⚠️ **VDES / AIS 2.0 (ITU-R M.2092)** — AIS component ✅ HAVE; ASM channels (AIS 27/28→ASM1/2) + VDE-TER/VDE-SAT ❌ MISSING. **SPECULATIVE/watch-list:** no open refs, low traffic, SOLAS pre-2028. ASM sub-channel is the only near-term slice.
- ⚠️ **Iridium maritime (Certus GMDSS / SBD)** — signaling-only; xng already demods the frames, but maritime distress payloads are encrypted. No new mode warranted.
- ❌ **WEFAX** (HF radiofax, IOC-576) — LOW fit: image output, not parseable PDUs (mismatch with xng's decode-to-JSON model). ❌ **Pactor/Winlink/SailMail** — NOT VIABLE (proprietary P2-4 PHY, vendor-blob monitor only). ❌ **Coastal radar** — N/A (no decodable message).
- **Ranking:** 1 DSC, 2 NAVTEX, 3 AIS-SART/MOB tagging, 4 SafetyNET header parsing; defer VDES; skip WEFAX/Pactor/Iridium-maritime-payload/radar.

### D4. Aviation-adjacent (UAT, FLARM, sondes, ELT, Remote ID)

- 💡 **UAT 978 MHz — PRIORITY #1.** CPFSK ±312.5 kHz, 1.0416 Mbps; short/long ADS-B + 432-byte ground uplink (FIS-B/TIS-B); open spec **DO-282B**. UAT ADS-B (US GA positions, fuses with existing `adsb`), **FIS-B weather** (NEXRAD/METAR/TAF/PIREP/AIRMET-SIGMET/Winds-Temps/NOTAM/TFR/SUA — FAA AC 00-63B), TIS-B/ADS-R (`uat2esnt`→DF18 CF=6). Ref: flightaware/dump978, same RTL-SDR/Soapy chain. **VIABILITY: clearly viable; first open decoder to feed FIS-B into Airframes is a unique data product.**
- 💡 **Radiosondes — PRIORITY #2.** 400.05–406 MHz GFSK, RS41/RS92/DFM/M10/M20/iMet/LMS6/iMS-100/MRZ. Open cores rs1729/RS wrapped by projecthorus/radiosonde_auto_rx → SondeHub/APRS-IS/MQTT; auto-scan maps onto xng's scanner. Worldwide, huge active community = instant feeder base. **VIABILITY: clearly viable; culturally identical to Airframes feeding.**
- ✅ **ADS-L (EASA SRD860, 868.2/868.4 + 869.525 MHz, 2-GFSK; Issue 2 Dec 2025 adds RemoteID rebroadcast)** — open EASA e-conspicuity standard, bundles with the OGN-family 868/915 capture; do **before** FLARM.
- ⚠️ **OGN family (FLARM / OGNTP / PilotAware / FANET)** — PRIORITY #4. One 868/915 capture yields the whole family (rtlsdr-ogn model = xng's multi-channel strength). FANET (LoRa CSS, open spec) + OGNTP + PilotAware + ADS-L are open → do these; **FLARM is decodable but proprietary + protocol-rotating** (broke OGN March 2024, ToS gray area) → only after the open ones land. Note: OGN's `ogn-decode` is a closed binary; use SoftRF/GXAirCom open cores.
- ✅ **COSPAS-SARSAT 406 MHz (ELT/EPIRB/PLB)** — PRIORITY #5. FGB (biphase-L 400 bps, 15-hex ID + position, C/S T.001) + SGB (OQPSK+DSSS, 23-hex, T.018). Open spec, thin open tooling (xng could be the clean-room reference). RTL-SDR feasible (SarsatJRX proves it). Rare on-air but the aviation-ELT/maritime-EPIRB angle is dead-center mission.
- ⚠️ **Drone Remote ID (ASTM F3411)** — in-mission (drone tracking) but **architecturally off-axis** (BT4/BT5/Wi-Fi, not SDR — needs nRF52/Sniffle). Only RF path that touches xng = ADS-L Issue-2 RemoteID rebroadcast. **LOW PRIORITY** unless via ADS-L.
- 💡 **LoRa-APRS / Horus Binary HAB** — cheap rider on the radiosonde build (same Project Horus stack, SondeHub-Amateur), open 4FSK+LDPC; amateur-payload relevance. **MEDIUM.**
- 💡/❌ **Mode A/C** — small add to existing 1090 core but low-value/fading (also listed in A9). ❌ **ACARS-over-SwiftBroadband** — not feasible today (wideband IP, no open ACARS-extraction tooling); see A4/critic roadmap-watch.
- **Ranking:** 1 UAT 978, 2 radiosondes, 3 ADS-L+FANET(+OGNTP/PilotAware), 4 FLARM (after open OGN), 5 COSPAS-SARSAT 406, 6 LoRa-APRS/HAB; skip Mode A/C (low), Remote ID (off-axis), SwiftBroadband (no open path).

### D5. Satellites & spacecraft

- 💡 **COSPAS-SARSAT 406 (FGB + SGB) — recommended #1** (also in D4) — the one addition both tractable (open T.001/T.018, BCH/CRC-gated, biphase-L/OQPSK within xng-dsp, same VHF/UHF hardware) and dead-center on the aviation+maritime distress mission. Clean-room port from amsa-code/{fgb,sgb}-decoder.
- 💡 **Orbcomm STX (137–138 MHz, SDPSK 4800 bps) — optional/low value** — cheapest band-mate to existing VHF, MIT ref (fbieberly), demod in xng-dsp's toolbox; but payload is proprietary IoT messaging + ephemeris, and the hoped-for "sat-AIS" is **not** exposed on STX (OG2 relays AIS to Orbcomm's own gateways). Breadth flex, not mission.
- ❌ **Weather sats (NOAA APT / Meteor LRPT / GOES/GK2A HRIT / Metop / FengYun / Elektro / MSG)** — **skip:** SatDump (GPL, 100+ downlinks, imagery pipelines) is an entrenched incumbent in a different problem domain (DSP→pixels), zero comms-payload mission fit.
- ❌ **Globalstar simplex (2.4 GHz DSSS)** — proprietary IoT framing, security finding not a product, wrong front-end. **skip.**
- ❌ **Amateur/cubesat telemetry** — gr-satellites + SatNOGS (500+ stations, 11M observations) own it via per-bird SatYAML + Doppler rotators; multi-year tarpit, no aviation/maritime relevance. **skip.**
- ❌/⚠️ **ISS** — SSTV is image-domain (SatDump); ISS APRS digipeater is generic AX.25 relayed once per pass (novelty). **skip** unless AX.25 is added for another reason.
- ❌ **Starlink** — encrypted/proprietary OFDM, only RF-presence/PNT achievable, needs steerable Ku dish. **not decodable.**
- ❌ **GNSS / GPS / Galileo / GLONASS / BeiDou** — PNT solver, nav message has no aircraft/vessel info; GNSS-SDR owns it. **skip.** *Critic addition:* **SBAS (WAAS/EGNOS/MSAS/GAGAN/SDCM/BDSBAS, L1 1575.42 MHz, 250 bit/s)** broadcasts aviation-grade integrity/correction data, decodable by GNSS-SDR — but it is GNSS-augmentation telemetry with no aircraft/vessel entities → **❌ considered and declined** (same rationale as GNSS; named here so reviewers don't flag the omission).
- ❌ **Rocket/launch telemetry** — commercial encrypted (SpaceX since 2021); open exceptions are amateur beacons in gr-satellites' domain. **skip.**
- **Ranking:** 1 COSPAS-SARSAT 406, 2 (optional) Orbcomm STX; decline everything else.

### D6. Other RF data worlds

- ❌ **Pager networks (POCSAG 512/1200/2400, FLEX/FLEX-NEXT)** — clear FSK, reuses xng's FSK chain (POCSAG = sync-word + BCH(31,21)); refs multimon-ng/PDW. Airport/airline ground-ops + FBO dispatch ride some POCSAG/FLEX = the one mission-touching angle. **VIABILITY: HIGH feasibility, MEDIUM fit** (verify which airfields still page).
- ❌ **ERMES (169 MHz 4-FSK)** — clear but near-dead EU network, PDW-only. **skip.**
- ❌ **APRS / packet radio (AX.25, 144.39/432 MHz AFSK1200)** — clear; vehicles/balloons/weather/telemetry; ref direwolf, aprs.fi. **HAB balloon tracking** (airborne moving objects) = closest-to-mission APRS use; igate output mirrors xng's feed pattern; AFSK1200 demod reuses across POCSAG. **MEDIUM-HIGH fit (HAB).**
- ❌ **rtl_433 ISM universe (320 protocols)** — only **TPMS** has a vehicle-ID angle; weather/AMR/security sensors near-zero mission fit. Reimplementing the catalog is a huge breadth commitment → better as `xng extern` wrapper than native core.
- ❌ **wM-Bus (868.95 MHz utility meters)** — utility telemetry, often AES, no aviation/maritime fit. **skip.**
- ❌ **P25/DMR/NXDN/TETRA voice-trunking** — vocoded + frequently encrypted + scanner-community focus; **out of mission** (metadata/GPS slice covered in D2). **skip.**
- ❌ **LoRa / Meshtastic / LoRaWAN** — LoRaWAN encrypted; Meshtastic decodable-but-flaky (<5% CRC); novel but low maturity + low fit. **P4/skip.**
- ❌ **WSPR/FT8/FT4/JS8 (HF MFSK + LDPC)** — strong existing tooling (WSJT-X), hard FEC, ham-centric; only relevance = HF propagation health (could inform HFDL site planning). **P3 niche.**
- ✅ **ACARS-adjacent ground systems** — VHF ACARS/VDL2/HFDL/Aero/STD-C/Iridium all ✅ HAVE. ❌ MISSING **CSC/Gatelink / AeroMACS** (WiMAX 5 GHz airport-surface datalink — no mature open decoder, verify demand); 💡 VDL Mode 4 + ACARS-over-Iridium-Certus extend the family. *Critic addition:* **VDL Mode 4** (STDMA 25 kHz VHF, ADS-B/TIS-B/FIS-B/GNS-B — the "AIS of the sky," operational only in Sweden's LFV net + niches, WAVECOM W-CODE only) is the one VDL variant xng's VDL2 work doesn't touch → ⚠️ **LOW (regional, near-dead) — considered, declined**, named explicitly so a VDL mode isn't silently omitted.
- **Maritime/SAR-adjacent** — covered in D3 (DSC, NAVTEX, AIS-SART, COSPAS-SARSAT, VDES).
- **Space** — covered in D5 (skip weather imagery — SatDump incumbent).

### Consolidated viability & mission-fit matrix (all new candidates)

| # | Mode | Band / Mod | Open? | Reference decoder | Feasibility | Mission fit | Priority |
|---|------|-----------|:-----:|-------------------|:-----------:|:-----------:|----------|
| 1 | **UAT 978 (ADS-B + FIS-B + TIS-B)** | 978 MHz CPFSK 1.04 Mbps | ✅ DO-282B | dump978-fa, readsb | H | **H** (US aviation, fuses `adsb`) | **P0** |
| 2 | **COSPAS-SARSAT 406 ELT/EPIRB/PLB** | 406 MHz BPSK/OQPSK | ✅ T.001/T.018 | amsa-code, dump406 | H | **H** (aviation+maritime distress) | **P0** |
| 3 | **DSC (VHF Ch70 + HF/MF)** | FFSK 1200 / 100 Bd FSK | ✅ M.493 BCH(10,7) | GopherTrunk, SDRangel | H | **H** (GMDSS distress) | **P0** |
| 4 | **Radiosondes (RS41/M10/DFM…)** | 400–406 MHz GFSK | ✅ | rs1729/RS, auto_rx | H | **H** (worldwide, SondeHub feed) | **P1** |
| 5 | **AIS-SART / MOB / EPIRB-AIS** | 161.975/162.025 GMSK | ✅ | AIS-catcher/gnuais | H (xng AIS core) | **H** (in-band maritime) | **P1** |
| 6 | **EOT / HOT / DPU rail telemetry** | 457/452 MHz Manchester FSK | ✅ AAR S-9152 | PyEOT (stale) | H | M-H (train-tail map) | **P1** |
| 7 | **NAVTEX** | 518/490/4209.5 kHz SITOR-B | ✅ | pd0wm, fldigi | M (LF hw) | M-H (maritime MSI) | **P1** |
| 8 | **ADS-L + FANET / OGNTP** | 868/915 MHz 2-GFSK / LoRa | ✅ EASA/open | SoftRF, GXAirCom | H | M-H (GA/glider) | **P2** |
| 9 | **APRS / AX.25 (incl. HAB)** | 144.39/432 AFSK1200 | ✅ | direwolf, aprs.fi | H | M-H (airborne objects) | **P2** |
| 10 | **POCSAG / FLEX / FLEX-NEXT** | VHF/UHF 2/4-FSK | ✅ | multimon-ng, PDW | H (reuses FSK) | M (airport/EMS ops paging) | **P2** |
| 11 | **ATCS 900 MHz rail signaling** | FSK 4800 bps HDLC-LAPB | ✅ | ATCSMon, RWMON | H | M (rail, multi-channel) | **P2** |
| 12 | **VDES / long-range AIS (msg 27)** | VHF Data Exchange | ✅ | AIS-catcher (partial) | H (extends AIS) | M-H (maritime) | **P2** |
| 13 | **FLARM** | 868 MHz GFSK XXTEA | ⚠️ proprietary/rotating | SoftRF | M (chase protocol) | M (glider) | P3 (after open OGN) |
| 14 | **LoRa-APRS / Horus HAB** | 433/868/915 4FSK+LDPC | ✅ | horusdemodlib | M | M (amateur HAB) | P3 (radiosonde rider) |
| 15 | **DMR-LRRP / P25-Unit-GPS / TETRA-SDS-LIP** | VHF/UHF C4FM/TDMA/DQPSK | ⚠️ clear nets only | SDRTrunk, GopherTrunk | M | M (transit GPS PDUs) | P3 (metadata only) |
| 16 | **TPMS (rtl_433 subset)** | 315/433 OOK/FSK | ✅ | rtl_433 | H (or wrap) | L-M (vehicle ID/presence) | P3 |
| 17 | **Orbcomm STX** | 137–138 MHz SDPSK | ✅ (PHY) | fbieberly | M | L (proprietary IoT payload) | P3 (breadth flex) |
| 18 | **GTFS-realtime (non-RF)** | HTTP/protobuf | ✅ | agency APIs | H (ingest, not decode) | M-H (transit, complement) | P3 (ingest layer) |
| 19 | WSPR / FT8 / FT4 / JS8 | HF MFSK + LDPC | ✅ | WSJT-X, CWSL_DIGI | M (hard FEC) | L-M (HF prop health) | P4 |
| 20 | rtl_433 full breadth / wM-Bus | 433/868/915 ISM | ✅/⚠️ AES | rtl_433, rtl-wmbus | M | L | wrap via `xng extern` |
| 21 | Meshtastic / LoRaWAN | LoRa CSS | ⚠️/❌ enc | gr-lora_sdr | L | L | skip/P4 |
| 22 | P25/DMR/NXDN/TETRA voice | vocoded/encrypted | ⚠️ | OP25, telive | M | L (out-of-mission voice) | skip |
| 23 | ERMES | 169 MHz 4-FSK | ✅ | PDW only | M | L (dead network) | skip |
| 24 | VDL Mode 4 | STDMA 25 kHz VHF | ✅ | WAVECOM only | M | L (regional/near-dead) | skip (declined) |
| 25 | DSRC / C-V2X | 5.9 GHz OFDM/LTE-sidelink | ⚠️ | (USRP-class) | L (wrong band/PHY) | M (sunsetting) | skip |
| 26 | Toll (E-ZPass) / Taxi MDT | 915 MHz / UHF FSK | ✅/proprietary | ez-sniff / taxidecoder | M (plaza-only) | L | skip |
| 27 | AEI / Eurobalise | 902–921 MHz / 4.234 MHz near-field | ✅ | — | NO-GO (passive) | L | skip |
| 28 | PTC / GSM-R / FRMCS | 220 MHz / GSM / 5G | ❌ encrypted | — | NO-GO | L | skip |
| 29 | NOAA APT / Meteor / GOES (weather imagery) | L/VHF imagery | ✅ | SatDump | M | L (imaging, incumbent) | skip |
| 30 | Cubesat telemetry / SatNOGS | 145/435 MHz mixed | ✅ | gr-satellites | M (per-bird configs) | L | skip |
| 31 | Globalstar / Starlink / rocket | 2.4 / Ku / S-band | ❌ proprietary/enc | — | L/NO-GO | L | skip |
| 32 | GNSS / SBAS | L1 1575.42 MHz | ✅ | GNSS-SDR | M | L (PNT, no entities) | declined |
| 33 | Drone Remote ID (F3411) | BT4/BT5/Wi-Fi | ✅ | opendroneid | L (not SDR) | M (drones) | P-low (via ADS-L only) |
| 34 | SwiftBroadband-Safety | L-band IP (ARINC 781 Att.8) | ❌ no open PHY | — | NO-GO today | H (FANS successor) | roadmap watch |

### Ranked top candidates (cross-domain)

1. **UAT 978** — fills the US ADS-B/FIS-B/TIS-B hole, same RF chain, fuses with `adsb`.
2. **COSPAS-SARSAT 406** — open spec, clean-room, dead-center distress mission.
3. **DSC (VHF + HF/MF)** — GMDSS distress, reuses VHF+HF front-ends, multimon-ng still lacks it.
4. **Radiosondes** — worldwide, huge SondeHub community, auto-scan fits xng tooling.
5. **AIS-SART / MOB / EPIRB-AIS** — near-free distress tagging over decoded AIS.
6. **EOT/HOT/DPU rail** — clear FSK telemetry → live train-map; best rail fit.
7. **NAVTEX** — extends the maritime-safety franchise (pairs with DSC).
8. **ADS-L + FANET / APRS-HAB** — open sub-GHz GA/airborne-object family from one capture.

- **Verify before committing:** (a) which airports/airlines still run POCSAG/FLEX ground-ops paging; (b) AeroMACS/Gatelink demand; (c) whether xng's L-band front-ends already pass 406 MHz for SARSAT reuse; (d) whether xng wants generic-ISM scope (TPMS).
- **Files:** `crates/` (existing mode crates), `crates/xng-dsp/src/` (reusable OQPSK/BPSK/FSK/Viterbi/BCH DSP for SARSAT/DSC/EOT/Orbcomm), `crates/xng-mode-ais/src/fields.rs` (reuse for AIS-SART), `README.md`.

---

## Appendix — Claims to verify + references

### Claims to verify (from the critic + inline ❓/verify flags)

- **Confirmed by critic (claim stands):**
  - ✅ **VDL2 `--max-ppm`** — dumpvdl2 v2.5.0 (2025-11-02). Also note **v2.6.0 (2026-02-07)** added SDRPlay RSP1B/RSPdx-R2, and **v2.5.1 (2026-01-06)** fixed a **249-octet-multiple block-length decode bug** — *cross-check xng's own AVLC block-length handling against this.*
  - ✅ **libacars 2.2.1 = 2025-11-02, "no new top-level protocol since OHMA"** — ACARS parity claim stands.
  - ✅ **ATN-B2 ADS-C requires CLNP+COTP reassembly (dumpvdl2 v2.2.0, 2022)** — accurate.
- **Needs sharpening / re-check:**
  - ⚠️ **Iridium AMBE "intentionally bytes-only / parity with ir77_ambe"** — framing: the gap is *codec legality*, not capture (voice is unencrypted by default per the 2026 analysis). Don't imply voice is otherwise protected.
  - ⚠️ **Beast 12 MHz MLAT counter usability** — ultrafeeder's mlat-client tolerates non-GPS clocks via server clock-sync, so the blocker may be *jitter/monotonicity of system-clock-derived timestamps*, not the absence of GPS. Sharpen before citing as a hard MLAT blocker.
  - ⚠️ **acarsdec MQTT output (`mqttout.c`)** — verify against the current f00b4r0/acarsdec 4.x tree (TLeconte upstream lacked it; confirm the fork still ships it post-4.0 SoapySDR refactor).
- **Iridium correctness/feature verifies:** RR-message labeling in `gsm::decode`; non-zero IBC `bc_type` sub-blocks; ISY `"10"` vs `"11"` UL pattern; LCW layer exposure in `frame.rs`; **Iridium time re-epoch** (ERA3=2025-02-14; next 2026-01-14 18:08 UTC) correctness in satellite-naming/time code; whether mt-position renders on the web map; SIMD build flags; UW pre-classify EC.
- **ACARS verifies:** raw MIN / 4th-char downlink rule edge cases; per-label ACARS Prometheus counters exist; any media-advisory v1+ exists.
- **VDL2 verifies:** SABME folded into `U?`; Call-Request maintenance/init status bit; ES-IS option-TLV coverage; plain/unprotected CPDLC PDUs; StatsD `good_loud` equivalent; `pp_acars` support.
- **AIS verifies:** CIC5 droop compensation in xng-dsp; mid-frame polarity-flip recovery; whether xng maps MMSI prefixes 970/972/974 (appears NOT today).
- **HFDL verifies:** LMS tap count 7 vs documented 15.
- **STD-C verifies:** `0xB0` vs `0xB1`+`0xB2` distinction in surfaced details; mid-frame polarity flip recovery.
- **Ecosystem verifies:** dashboard station/receiver-position pin exists; continuous autogain during `listen`; non-Iridium trail antimeridian wrapping; DF18 CF-subtype classification.
- **Aero-C verify:** exact `AEROTypeP/R` enumerator hex (WebFetch returned inconsistent values — confirm against JAERO source before encoding a SU table).
- **Part C — RESOLVED (verified 2026-06 against primary sources):** ACARS 5550/UDP ✅; VDL2 5552/UDP + 5553/TCP ✅; HFDL 5556/UDP ✅; **AIS = HTTP `feed.airframes.io:5599` (AIS-Catcher `PROTOCOL AIRFRAMES`)** ✅ — was missing from the draft; **IMSL `:5557` and IRDM `:5558` are NOT public Airframes ports** (local acars_router/acarshub listeners only — Iridium feeding explicitly "unsettled"); **STD-C/Aero-C have no public ingest**; **ADS-B is NOT fed to Airframes** (Beast/SBS are local servers for other aggregators). Remaining unknowns: whether Airframes later exposes public IMSL/IRDM/STD-C ports, and the settled Iridium mechanism — asf-2.0 is the path for all of these today.
- **New-mode verifies:** which airports/airlines still run POCSAG/FLEX; AeroMACS/Gatelink demand; whether xng L-band front-ends pass 406 MHz; whether generic-ISM (TPMS) scope is desired; whether any open Orbcomm AIS payload exists; whether `crates/xng-mode-adsb` already emits Mode A/C (README implies not).

### References / URLs gathered across sections

- **ACARS / app layer:** libacars (https://github.com/szpajder/libacars) + CHANGELOG; f00b4r0/acarsdec (https://github.com/f00b4r0/acarsdec) — label.c, output.c, acars.c, syndrom.h, mqttout.c; airframesio/acars-message-documentation (https://github.com/airframesio/acars-message-documentation) incl. research/H1/CFB.md; NOAA dcacar (https://www.nco.ncep.noaa.gov/sdb/decoders/dcacar/); AMDAR (https://en.wikipedia.org/wiki/Aircraft_Meteorological_Data_Relay); PlanePlotter/COAA (https://www.coaa.co.uk/planeplotter.htm); airframes blog (https://blog.airframes.io/decoding-acars-messages-understanding-components-and-values/); xoolive datalink (https://lib.rs/crates/datalink); Wiley datalink (https://catalogimages.wiley.com/images/db/pdf/9781848217416.excerpt.pdf); sdr-enthusiasts/acars_router (https://github.com/sdr-enthusiasts/acars_router).
- **VDL2 / ATN:** dumpvdl2 (https://github.com/szpajder/dumpvdl2) — src/{xid,x25,cotp,idrp,esis,atn,asn1-format-icao-text}.c, doc/NEWS.md; vdlm2dec (https://github.com/TLeconte/vdlm2dec).
- **HFDL:** dumphfdl (https://github.com/szpajder/dumphfdl) — src/{spdu,lpdu,hfnpdu,ac_cache,position,systable}.c, doc/STATSD_METRICS.md, CHANGELOG, README; rtl-sdr.com dumphfdl (https://www.rtl-sdr.com/dumphfdl-a-multichannel-hfdl-decoder-for-sdr/); kloth.net (https://www.kloth.net/radio/hfdl-monitoring.php); iw5edi PC-HFDL (https://www.iw5edi.com/ham-radio/4403/pc-hfdl); groups.io/g/multipsk.
- **Aero (L + C):** JAERO (https://github.com/jontio/JAERO) + aerol.h/aerol.cpp/mskdemodulator.cpp; libaeroambe (https://github.com/jontio/libaeroambe); inmarsat-sniffer (https://github.com/alphafox02/inmarsat-sniffer); rtl-sdr.com AERO C-channel (https://www.rtl-sdr.com/aero-c-channel-voice-audio-now-decodable-with-jaero/); WAVECOM SAT-AERO (https://cartoonman.github.io/WAVECOM/wavecomhtm/sataeropsataerorsata.htm); usradioguy (https://usradioguy.com/inmarsat-decoding/); sigidwiki Inmarsat Aero (https://www.sigidwiki.com/wiki/Inmarsat_Aero); SwiftBroadband (https://en.wikipedia.org/wiki/SwiftBroadband), Viasat/Inmarsat SB-S (https://www.inmarsat.com/aviation/complete-aviation-connectivity/swiftbroadband-safety/), Aviation Today (https://interactive.aviationtoday.com/integrating-ip-into-acars-transmissions/).
- **STD-C / EGC:** inmarsatc (https://github.com/cropinghigh/inmarsatc) — parser/decoder/demodulator.cpp; tekmanoid (https://tekmanoid.com/egc) + rtl-sdr.com (https://www.rtl-sdr.com/tekmanoid-std-c-decoder-updated-new-paid-les-decoder-egc-visualization/), Scytale-C (https://www.rtl-sdr.com/youtube-series-on-inmarsat-decoding-with-scytale-c/); SafetyNET (https://sea-man.org/inmarsat-safetynet.html), navcen (https://www.navcen.uscg.gov/marcomms-inmarsat-c-safetynet), SafetyNET Handbook Ed.6.
- **Iridium:** iridium-toolkit (https://github.com/muccc/iridium-toolkit) — bitsparser.py, iridium-parser.py, reassembler/{ida,ppm,tdoa}.py; iridium-sniffer (https://github.com/alphafox02/iridium-sniffer); 2026 security analysis (https://arxiv.org/html/2603.12062v1); MetOcean re-epoch bulletin (https://metocean.com/important-technical-bulletin-from-metocean-iridium-re-epoch/).
- **AIS:** AIS-catcher (https://github.com/jvde-github/AIS-catcher) + docs (https://jvde-github.github.io/AIS-catcher-docs/); pyais (https://github.com/M0r13n/pyais); gpsd AIVDM (https://gpsd.gitlab.io/gpsd/AIVDM.html); gnuais (https://gnuais.sourceforge.net/), rtl-ais (https://github.com/dgiardini/rtl-ais); CESNI inland ASM (https://ris.cesni.eu/docs/File/618/...); e-Navigation ASM registry (https://www.e-navigation.nl/asm); ITU-R M.2092 (https://www.itu.int/dms_pubrec/itu-r/rec/m/R-REC-M.2092-0-201510-I!!TOC-HTM-E.htm); IALA G1117 VDES; AIS-MOB (https://www.seasofsolutions.com/resources/ais-mob/); IMO AIS-SART (https://www.imorules.com/GUID-2567AAB7-...).
- **Mode S / ADS-B:** readsb (https://github.com/wiedehopf/readsb) + README-json.md; pyModeS (https://github.com/junzis/pyModeS) + BDS API (https://mode-s.org/pymodes/api/pyModeS.decoder.bds.html); rs1090/jet1090 (https://github.com/xoolive/rs1090); dump978-fa (https://github.com/flightaware/dump978) + uat2esnt.c; mlat-client (https://github.com/ADSBexchange/mlat-client); mode-s.org (operational-status, uncertainty/version, surface-position, basics); AOPA FIS-B; ADS-B (https://en.wikipedia.org/wiki/Automatic_Dependent_Surveillance%E2%80%93Broadcast); awesome-adsb / BelugaProject (https://github.com/rickstaa/awesome-adsb).
- **Ecosystem:** docker-acarshub (https://github.com/sdr-enthusiasts/docker-acarshub); tar1090 (https://github.com/wiedehopf/tar1090) + README-query.md; graphs1090 (https://github.com/wiedehopf/graphs1090); tar1090-db (https://github.com/wiedehopf/tar1090-db); docker-adsb-ultrafeeder (https://github.com/sdr-enthusiasts/docker-adsb-ultrafeeder); VRS DatabaseWriter (http://www.virtualradarserver.co.uk/documentation/DatabaseWriter/); flightmonitor_MQTTtoHA (https://github.com/EricG78/flightmonitor_MQTTtoHA); Grafana SignalK (https://grafana.com/grafana/plugins/tkurki-signalk-datasource/).
- **Airframes feeding (verified):** docs.airframes.io feeding/how (https://raw.githubusercontent.com/airframesio/docs/main/docs/feeding/how.md) + decoders/developers.md; protocol-examples (vdlm2dec/dumphfdl/iridium-toolkit/airframes JSON, https://github.com/airframesio/docs/tree/main/docs/clients/protocol-examples); acars_router port table (https://github.com/sdr-enthusiasts/acars_router); docker-acarshub (https://github.com/sdr-enthusiasts/docker-acarshub); AIS HTTP ingest LIVE :5599 (https://community.airframes.io/t/airframes-ais-catcher-http-ingest-is-live/83) + AIS-catcher HTTP `PROTOCOL AIRFRAMES` (https://jvde-github.github.io/AIS-catcher-docs/configuration/output/HTTP/); Iridium feeding "unsettled" (https://thebaldgeek.github.io/Iridium.html); docker-satdump IMSL :5557 local (https://github.com/rpatel3001/docker-satdump); local docs/ASF2.md + proto/asf2.proto.
- **Rail:** sigidwiki EOTD (https://www.sigidwiki.com/wiki/End_of_Train_Device_(EOTD)) + rtl-sdr.com EOT/HOT/DPU; CVE-2025-1727 (https://industrialcyber.co/...); PyEOT (https://github.com/ereuter/PyEOT), EOTDecode (https://github.com/russinnes/EOTDecode); sigidwiki ATCS + atcsmon.com + rail.watch/rwmon.html; AEI (https://en.wikipedia.org/wiki/Automatic_equipment_identification); sigidwiki I-ETMS + Meteorcomm + GNU Radio Conf 2022; GSM-R/FRMCS/Eurobalise (Wikipedia + ERA SUBSET-036).
- **Road/transit:** tetra-kit (https://gitlab.com/larryth/tetra-kit), osmo-tetra-sq5bpf (https://github.com/sq5bpf/osmo-tetra-sq5bpf), teatime + Midnight Blue TETRA:BURST (https://www.midnightblue.nl/research/tetraburst); SDRTrunk (https://github.com/DSheirer/sdrtrunk), OP25, GopherTrunk (https://github.com/MattCheramie/GopherTrunk); NXDN (https://en.wikipedia.org/wiki/NXDN); rtl_433 (https://github.com/merbanan/rtl_433); DSRC/C-V2X (FCC Federal Register 2024-28980, Qualcomm, Wikipedia Cellular V2X); ez-sniff (https://github.com/kwesthaus/ez-sniff); GTFS-rt (https://gtfs.org/documentation/realtime/reference/, MBTA, OneBusAway).
- **Maritime:** GopherTrunk DSC, SDRangel Rx plugins (https://github.com/f4exb/sdrangel/wiki/Channel-Rx-plugins), TAOSW.DSC_Decoder (https://github.com/alemassimo/TAOSW.DSC_Decoder), YaDD/YaND (ndblist.info); pd0wm/navtex (https://github.com/pd0wm/navtex), fldigi NAVTEX; sigidwiki SITOR-B/GMDSS-DSC; VDES (IALA, Sternula, ITU-R M.2092); WEFAX (wojlin/WEFAX); Winlink PMON; Certus GMDSS (iridium.com).
- **Aviation-adjacent:** dump978-fa, FAA AC 00-63B; radiosonde_auto_rx (https://github.com/projecthorus/radiosonde_auto_rx) + rs1729/RS (https://github.com/rs1729/RS); SoftRF (https://github.com/lyusupov/SoftRF), 3s1d/fanet, GXAirCom; EASA ADS-L SRD860 (https://www.easa.europa.eu/.../ads-l_4_srd860_issue_1.pdf), flarm.com ADS-L v2; opendroneid-core-c (https://github.com/opendroneid/opendroneid-core-c), droneid-go; horusdemodlib (https://github.com/projecthorus/horusdemodlib); COSPAS-SARSAT T.001/T.018, amsa-code/{fgb,sgb}-decoder (https://github.com/amsa-code/fgb-decoder), SarsatJRX.
- **Satellites:** SatDump (https://github.com/SatDump/SatDump); fbieberly/ORBCOMM-receiver (https://github.com/fbieberly/ORBCOMM-receiver), sam210723/orbcomm-rx; timmattison/globalstar; gr-satellites (https://github.com/daniestevez/gr-satellites) + SatNOGS; GNSS-SDR (https://github.com/gnss-sdr/gnss-sdr) + ESA Navipedia SBAS; Starlink PNT (arxiv 2210.11578).
- **Other:** multimon-ng (https://github.com/EliasOenal/multimon-ng) + DSC issue #29; direwolf (https://github.com/wb2osz/direwolf) + aprs.fi; rtl-wmbus (https://github.com/wmbusmeters/rtl-wmbus); gr-lora_sdr (https://github.com/tapparelj/gr-lora_sdr); WSJT-X + CWSL_DIGI (https://github.com/alexranaldi/CWSL_DIGI); ERMES (sigidwiki).
