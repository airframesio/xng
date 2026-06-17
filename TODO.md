# xng — Implementation TODO

> Derived from [`COMPARISON_RESEARCH.md`](COMPARISON_RESEARCH.md) (audited 2026-06).
> This is the actionable backlog; the research doc is the rationale/sourcing for each item.
> **Nothing here is implemented yet** — every box is unchecked. Check items off (`- [x]`) as they land.

## How to use this file

- **Checkbox** — `- [ ]` open · `- [x]` done. Check a parent only when all its subtasks are done.
- **Item ID** — `CATEGORY-N` (top-level) and `CATEGORY-N.M` / `CATEGORY-N.M.K` (subtasks, tree). IDs are **stable** — never renumber; if an item is dropped, mark it `~~struck~~ (dropped: reason)` rather than deleting.
- **Tags** — `★ quick win` (high value / low, clean-room effort) · `◆ big bet` (large, high-leverage) · `⚑ correctness` (possible live bug, verify first) · `(A1)`/`(B)`/`(C)`/`(D)` = source section in the research doc.
- **Categories:** `FEED` feeding feature · `XM` cross-mode foundations · `ACARS` `VDL2` `HFDL` `AERO` `STDC` `IRID` `AIS` `ADSB` per-mode · `ECO` ecosystem/tooling · `NEW` new modes · `VERIFY` research/correctness checks.

## Suggested sequencing (recommended order, not binding)

> **Feeding follows decode.** A mode's Airframes feed (`FEED-2.x`, and enabling that mode under `[outputs.airframes]`) must land *after* the mode's decode is complete enough to populate the ingest's native wire format — feeding partial/wrong records to Airframes is worse than not feeding. The only feeding work with no decode dependency is the mode-independent scaffolding `FEED-1` (config schema, resolver, validation), which may be built early.

1. **Foundations** — `XM-1` (shared signal-quality), `XM-2` (entity model + one position→SBS adapter); optionally `FEED-1` config scaffolding (no decode dependency).
2. **Per-mode decode quick wins** — `ACARS-2`, `ADSB-1`/`ADSB-2`, `AIS-1`/`AIS-3`, `AIS-4`, `STDC-1`, `HFDL-1`, `IRID-8`/`VERIFY-1` (⚑).
3. **Big bets** — `VDL2-1`+`VDL2-2` (ASN.1/PER core + CLNP/COTP → ATN-B2 ADS-C), `ECO-1`/`ECO-4`, `XM-4` (MLAT/TDOA).
4. **Per-mode Airframes feeds (only after the mode's decode is solid)** — `FEED-2.1` after VDL2 decode, `FEED-2.2` after HFDL, `FEED-2.3` after AIS, `FEED-2.4`/`ACARS-5.1` after ACARS text decode.
5. **New modes** — by priority: `NEW-P0-*` → `NEW-P1-*` → `NEW-P2-*` (each new mode also gets its feed only after its own decode lands).

---

## FEED — Per-decoder Airframes feeding feature (C)

> The explicitly-requested feature: per-mode ingests, per-mode native wire format, per-decoder disable, per-mode/per-session station-id override. asf-2.0 is the exempt multiplexing path. Verified ingest table: ACARS UDP :5550 (acarsdec JSON), VDL2 UDP :5552 / TCP :5553 (dumpvdl2 JSON), HFDL UDP :5556 (dumphfdl JSON), AIS HTTP :5599 (AIS-Catcher `PROTOCOL AIRFRAMES`). IMSL/IRDM/STD-C/Aero-C/ADS-B → asf-2.0 only.
>
> **Ordering:** `FEED-1` (config scaffolding) has no decode dependency and may be done early. **`FEED-2.x` per-mode serializers must come AFTER the corresponding mode's decode is complete enough to fill that ingest's native format** — see the `⏳ after …` notes below.

- [x] **FEED-1** Config schema + plumbing for per-mode feeding (`station.rs`, `main.rs`, `runtime.rs`) — *done on `feat/per-decoder-airframes-feeding`*
  - [x] **FEED-1.1** Add `[outputs.airframes]` block (`enabled`, `station-id`, `auto-suffix`) + per-mode sub-tables `acars/vdl2/hfdl/ais` → `AirframesToml` / `AirframesModeToml{enabled,station_id,host,port}`
  - [x] **FEED-1.2** Add `SessionToml.feed: Option<bool>` (per-decoder kill-switch) and `airframes_station_id` (per-session id override)
  - [x] **FEED-1.3** Per-session `AirframesTarget` resolver with documented precedence (session `feed=false` → mode-block `enabled` → id: session override ⟶ mode-block id ⟶ `auto-suffix(base,mode)` ⟶ base) — `station::airframes_router`
  - [x] **FEED-1.4** `auto-suffix(base, mode)` station-id derivation (strip-then-reappend a known mode suffix) ★ quick win
  - [x] **FEED-1.5** Validation (`deny_unknown_fields` rejects `adsb`/`imsl`/`irdm`/`stdc`/`aeroc` sub-tables; `feed=false` precedence; duplicate id+endpoint warning; base id always resolvable since top-level `station-id` is mandatory)
  - [x] **FEED-1.6** `OutputConfig.airframes: Option<AirframesRouter>` replaces the shared `udp` Airframes coupling; generic `udp` kept for other sinks
  - [x] **FEED-1.7** `--feed-airframes` back-compat preserved (ACARS-only, id verbatim — live station unchanged); `--feed-airframes-modes` flag deferred
- [ ] **FEED-2** Per-mode native serializers (each returns `None` for non-matching bodies) — ⏳ **sequence each AFTER its mode's decode is complete enough to fill the native format**
  - [ ] **FEED-2.1** `dumpvdl2_json.rs` — dumpvdl2 `decoded:json` emitter → VDL2 UDP :5552 / TCP :5553 — ⏳ after VDL2 decode (`VDL2-1`/`VDL2-2`) populates the dumpvdl2 schema
  - [ ] **FEED-2.2** `dumphfdl_json.rs` — dumphfdl `decoded:json` (nested) emitter → HFDL UDP :5556 — ⏳ after `HFDL-1` (HFNPDU records) + `HFDL-4` (positions)
  - [ ] **FEED-2.3** AIS-Catcher `PROTOCOL AIRFRAMES` HTTP-POST task (batched interval) → HTTP :5599 (`proto=http` path, distinct from datagram path) — ⏳ after `AIS-1`/`AIS-3` field decode
  - [x] **FEED-2.4** Wire existing `acarsdec_json.rs` (ACARS :5550) under the new router via `format_acarsdec_with_station` (station-id stamped at format time; provenance untouched). Richer fields ⏳ after `ACARS-2`/`ACARS-4.1` → `ACARS-5.1`
- [x] **FEED-3** asf-2.0 (exempt) handling
  - [x] **FEED-3.1** asf-2.0 stays on its own path (canonical `station_ident`), never routed through the per-mode resolver; runs alongside per-port legacy feeds
  - [x] **FEED-3.2** Modes with no public per-port ingest (IMSL/IRDM/STD-C/Aero-C/ADS-B) are skipped by the router (fed via asf-2.0); documented in `airframes.rs` + example config
- [ ] **FEED-4** Open items to confirm (see VERIFY-13)
  - [ ] **FEED-4.1** Confirm VDL2 ingest accepts dumpvdl2 `decoded:json` (emit that, not vdlm2dec flat JSON)
  - [ ] **FEED-4.2** Track whether Airframes exposes public IMSL/IRDM/STD-C ports + settled Iridium feeding mechanism

---

## XM — Cross-mode structural foundations (B — cross-mode bets)

> Highest-leverage: each closes a gap flagged separately in many modes.

- [ ] **XM-1** Shared per-burst `SignalQuality{signal dBFS, noise dBFS, SNR, CFO-ppm, fec_corrected}` schema populated by **every** demod ◆ big bet (closes output gap in ACARS/VDL2/HFDL/STDC/Iridium/Aero/AIS)
- [ ] **XM-2** Unified cross-mode entity model ◆ big bet
  - [ ] **XM-2.1** `Entity{kind: aircraft|vessel|sat|beacon, id: ICAO|MMSI|IMEI|HexID, positions[], identities[], source_modes[]}` track store
  - [ ] **XM-2.2** One mode-agnostic **position → SBS/Beast/map adapter** keyed on the entity (replaces the 4 separate per-mode wirings; unlocks HFDL/Iridium/Aero positions to SBS/tar1090/VRS) ★ quick win once XM-2.1 exists
- [ ] **XM-3** Shared ICAO/registration resolver (tail↔ICAO↔reg↔operator↔dbFlags mil/PIA/LADD) serving ACARS/VDL2/HFDL/Aero/Iridium/ADS-B
- [ ] **XM-4** Multi-receiver geolocation primitive over the asf-2.0 fan-in (Iridium TDOA + Doppler self-position + ADS-B MLAT as one engine keyed on the entity)
- [ ] **XM-5** Cross-mode dedup keyed on `(entity_id, content_hash, time-window)` (covers ADS-B multi-RX, AIS `unique on`, ecosystem fan-in)
- [ ] **XM-6** Cross-mode distress/emergency overlay (ADS-B 7500/7600/7700 + TC28 + AIS-SART/MOB + STD-C EGC distress + future DSC / COSPAS-SARSAT 406) → one alerting surface

---

## ACARS — VHF ACARS + application layer (A1)

- [ ] **ACARS-1** Label catalogue + per-label field extractors (build label→meaning table)
  - [ ] **ACARS-1.1** Decode the `Q`-series link-test/squitter family (Q0–Q7, QA–QX)
  - [ ] **ACARS-1.2** Surface raw MIN; handle 4th-char downlink-rule edge cases (see VERIFY-2)
- [ ] **ACARS-2** Embedded text-content decoders — **the big user-visible gap** ★ quick win
  - [ ] **ACARS-2.1** OOOI: `gtout/gtin/wloff/wlin/depa/dsta/eta`
  - [ ] **ACARS-2.2** Free-text position reports (labels `20/POS`, `4J`, `H1 POS`)
  - [ ] **ACARS-2.3** AMDAR / winds-aloft / PIREP (WMO-BUFR-class schema; NOAA `dcacar` ref)
  - [ ] **ACARS-2.4** FLIGHTPLAN / route (FPN) + Boeing/Airbus telex / structured free-text
  - [ ] **ACARS-2.5** H1 `#CFB`/CF maintenance family (APM_REPORT, ATA, AL, FDE, ECT, FLR, LIGHTS, MIL, MPF, PAGE, WRN)
- [ ] **ACARS-3** Application-layer completion (vs libacars 2.2.1)
  - [ ] **ACARS-3.1** CPDLC argument readers for the bracketed-template shapes + `FANSPosition` placeBearingDistance + RouteClearance trackDetail/routeInformationAdditional
  - [ ] **ACARS-3.2** Generic sublabel/MFI extraction beyond `H1`
  - [ ] **ACARS-3.3** Reassembly-status enum (`assstat`: complete/in-progress/skipped/duplicate)
  - [ ] **ACARS-3.4** Verify MIAM CRC + vendor real off-air media-advisory captures
- [ ] **ACARS-4** Demod / robustness
  - [ ] **ACARS-4.1** Emit `noise`/noise-floor + SNR (today only envelope RSSI) — see XM-1
  - [ ] **ACARS-4.2** Syndrome-table FEC (O(1) error-position lookup, acarsdec `syndrom.h`)
  - [ ] **ACARS-4.3** Off-air acarsdec head-to-head benchmark + CI count gate (no POA row exists today)
- [ ] **ACARS-5** Outputs
  - [ ] **ACARS-5.1** Emit the dropped acarsdec-JSON fields (`noise`, `sublabel`, `mfi`, `assstat`, nested `app`/`libacars`, OOOI fields) — ties FEED-2.4
  - [ ] **ACARS-5.2** MQTT output sink + per-label / per-channel counters (see VERIFY-9)

---

## VDL2 — VDL Mode 2 + AVLC + ATN (A2)

- [ ] **VDL2-1** Table/codegen-driven unaligned-PER ASN.1 core ◆ big bet (unlocks the next 5 at once)
  - [ ] **VDL2-1.1** ~44 unsupported CPDLC argument types (element walk currently stops at first undecodable arg)
  - [ ] **VDL2-1.2** CHOICE extension alternatives / integrityCheck / PER fragmentation
  - [ ] **VDL2-1.3** ACSE (AARQ/AARE/RLRQ/RLRE/ABRT) + Session (X.225 SPDU) layers
  - [ ] **VDL2-1.4** Full Context Management (TSAP/NSAP addrs; CMContactRequest/LogonResponse/ForwardRequest/Update)
  - [ ] **VDL2-1.5** Plain/unprotected CPDLC PDUs + forward/forward-response bodies
- [ ] **VDL2-2** CLNP + COTP reassembly → native ATN-B2 ADS-C ◆ big bet (the headline VDL2 gap)
  - [ ] **VDL2-2.1** Multipart CLNP reassembly + ATN security-label TLVs (traffic-type/ATSC-class/subnetwork-type)
  - [ ] **VDL2-2.2** COTP DC/ED/AK/EA/RJ TPDUs + full variable part (TPDU-size, checksum, ATN checksum 0x08, credit, EOT, extended seq) + multipart COTP reassembly
  - [ ] **VDL2-2.3** Native ATN-B2 ADS-C (ADSReport/RequestContract/Accept/Reject/PositiveAck/NonCompliance over CLNP/COTP)
- [ ] **VDL2-3** XID parameter completion — TG5(0x46), T3min(0x47), GS-address-filter(0x48), broadcast-connection(0x49), frequency-support-list(0xC0), airport-coverage(0xC1), nearest-airport(0xC3), ATN-router-NETs(0xC4), system-mask(0xC5), TG3(0xC6), TG4(0xC7) + ISO-8885 HDLC param set; decode autotune freq→MHz + timers→int
- [ ] **VDL2-4** X.25 completion — RESTART-REQ/CONF, facility naming, clear/reset/restart cause + diagnostic-code dictionaries, SNDCF compression facility
- [ ] **VDL2-5** AVLC polish — SABME (0x6F), expand FRMR info-field, pin one canonical FCS octet order; cross-check the v2.5.1 249-octet block-length bug (VERIFY-3)
- [ ] **VDL2-6** IDRP RIB-REFRESH + OPEN body fields + ERROR code/subcode text; ES-IS option TLVs (0x81/0x88/0xCF/0xC5)
- [ ] **VDL2-7** Demod — `--max-ppm` PPM/CFO reject filter; Gardner/matched timing-error detector for weakest-burst acquisition
- [ ] **VDL2-8** Outputs — full per-message signal line (noise+SNR+ppm, see XM-1); `--extended-header [S][L][F][#]`; aircraft enrichment (`--addrinfo`/`--bs-db`); msg-filter grammar; ZMQ publisher; raw-AVLC archive; `--dump-asn1`

---

## HFDL — HFDL / ARINC 635 (A3)

- [ ] **HFDL-1** HFNPDU full-record decode ★ quick win (pure parsing)
  - [ ] **HFDL-1.1** Split 0xD1 performance-data from the 0xD5 handler; decode the full 47-byte perf record (version, flight_leg, gs/freq_id, counters, freq_change_code)
  - [ ] **HFDL-1.2** Decode 0xD5 per-GS `{gs_id, prop_freqs, tuned_freqs}` arrays
  - [ ] **HFDL-1.3** 0xD2 system-table-request field parse; name 0xDE delayed-echo; name 0x2F logon-denied with reason table
- [ ] **HFDL-2** System table + GS naming
  - [ ] **HFDL-2.1** `--system-table` load/save persistence (cold-start enrichment)
  - [ ] **HFDL-2.2** Config-driven GS name file (IDs up to 127; fill the 12 hardcoded holes)
- [ ] **HFDL-3** Aircraft-ID→ICAO cache (`ac_cache`, `--aircraft-cache-ttl`) — ties XM-3
- [ ] **HFDL-4** Positions — lift `{lat,lon,utc}` into `MessageBody::Hfdl`; position from logon-request/resume (back-dated UTC); wire HFDL positions to SBS/Beast (ties XM-2.2); `--freq-as-squawk`
- [ ] **HFDL-5** Demod — populate `fec_corrected`; per-frame SNR/signal/CFO (see XM-1); FFT polyphase channelizer for many-channel CPU (verify LMS taps 7 vs 15 → VERIFY-5)
- [ ] **HFDL-6** Outputs — expand Prometheus to dumphfdl-StatsD granularity (preamble A2/M1, per-layer bad_fcs/too_short, dir counters, reasm states, cache gauges) + noise-floor gauge; zmq/kafka/file-rotation; `--raw-frames`/`--output-mpdus`/`--output-corrupted-pdus`; SPDU `rls_in_use`/`iso8208_supported` flags + per-slot assignment map

---

## AERO — Inmarsat Aero L + C-band bursts (A4, A5)

- [ ] **AERO-1** Full P-channel SU classifier (0x00–0x76; JAERO names the table, xng decodes ~6)
  - [ ] **AERO-1.1** Log-on/log-off control SUs (0x10–0x17) → structured AES↔GES session events
  - [ ] **AERO-1.2** `Call_announcement` 0x21, `T_channel_assignment` 0x51
  - [ ] **AERO-1.3** AES system-table broadcast (satellite_identification 0x0C, GES_beam_support 0x07, Psmc/Rsmc 0x05, broadcast_index 0x0A)
  - [ ] **AERO-1.4** EIRP-table 0x28, P/R-control-ISU 0x40, T-control-ISU 0x41, RQA 0x61, RACK/TACK 0x62, short-LSDU 0x74/0x76
- [ ] **AERO-2** Satellite/beam resolution → tag every message with the resolved satellite (self-configuring, L-band analogue of HFDL systable)
- [ ] **AERO-3** R/T-channel named control set (access-request/call-progress/telephony-ack/RQA/ACK); verify `SEQINDICATOR→(k,n)`
- [ ] **AERO-4** Interpret 16-bit P-channel frame header (formatid/superframe/framecounters) for superframe lock + AFC/DCD state machine
- [ ] **AERO-5** C-channel voice → WAV (AMBE decode behind a feature flag) + older AERO-H LPC path; wire `CChannelDecoder` into a runtime `Mode`
- [ ] **AERO-6** Demod — coherent 600/1200 path (close the ~2 dB gap to JAERO); populate `fec_corrected`; BER-vs-SNR curve
- [ ] **AERO-7** Outputs — expand `MessageBody::Aero` bodies (log-on/satellite-id/system-table/call); aircraft-DB enrichment + Aero position plotting
- [ ] **AERO-8** Aero-C consolidation (A5)
  - [ ] **AERO-8.1** Fix `Mode::AeroC` mislabel (`to_message` hard-tags `AeroL`) + wire scan-plan/dispatch/feed — OR fold aero-c into `aero` as a PHY-selected burst sub-decoder
  - [ ] **AERO-8.2** Typed SU classifier shared across P/R/T; C-channel descrambler `dl2`; emit `bit_rate`+channel tag; write `docs/notes/AERO.md`
  - [ ] **AERO-8.3** 10.5k A-QPSK burst path for aero-c
- [ ] **AERO-9** SwiftBroadband-Safety — roadmap watch only (FANS successor; no open PHY decoder today); no implementation

---

## STDC — Inmarsat STD-C / EGC (A6)

- [ ] **STDC-1** Geographic area-address decoder — biggest gap, **no OSS decoder does it** ★ quick win
  - [ ] **STDC-1.1** Rectangular (C2=04/34), circular (C2=24/44/14), NAVAREA/METAREA number (C2=31), coastal/NAVTEX (C2=13/73)
  - [ ] **STDC-1.2** Emit structured area geometry fields + map plotting of areas/coordinates
- [ ] **STDC-2** C-channel descriptor field depth — `0x7D` bulletin-board full fields, `0x6C` signalling-channel, `0x83` logical-channel-assignment, `0x92` login-ack (LES list), `0xAB` les-list, `0xA3`/`0xA8` short-text, `0x08` ack-request routing
- [ ] **STDC-3** Channel-frequency decode (uplink/downlink MHz from channel-number word; formula already in `STDC.md`)
- [ ] **STDC-4** LES/NCS operator-name table + ocean-region long names + service long names
- [ ] **STDC-5** Follow `0x83` → demodulate the LES message channel (closes the biggest functional gap vs tekmanoid) ◆ big bet
- [ ] **STDC-6** Text — ITA2/Baudot (presentation 6); typed presentation-7 binary capture
- [ ] **STDC-7** EGC polish — frame_number→UTC-of-day (`×8.64`); verify single `0xB0` vs double `0xB1`+`0xB2` (VERIFY-6); distress-specific position/alerting
- [ ] **STDC-8** Demod — RRC matched filter; optional CMA equalizer; SatDump `.frm` goldens; per-frame UW BER; populate `fec_corrected`; mid-frame polarity-flip recovery

---

## IRID — Iridium (A7)

- [ ] **IRID-1** Frame typing — AQ acquisition uplink (`IridiumAQMessage`), NXT (`IridiumNXTMessage`); verify ISY `"10"` vs `"11"` UL pattern; non-zero IBC `bc_type` sub-blocks
- [ ] **IRID-2** Upper-layer IP content — PPP-PAP credential frames + HTTP Basic-Auth headers in plaintext IIP/IIQ/IIR IP sessions (new decode target; ~88% of frames unencrypted)
- [ ] **IRID-3** GSM layer-3 — RR messages (Immediate-Assignment/Paging/System-Info) labelling; expose LCW layer; PCAP output (`-m lap`)
- [ ] **IRID-4** Positions — wire Iridium ADS-C / mt-position to SBS + web-map layer (`sbs.rs` is ModeS-only; ties XM-2.2); render mt-position
- [ ] **IRID-5** Demod — soft-decision / Chase BCH decoding (the next weak-frame lever); UW error-correction pre-classify; explicit SIMD select; GPU/OpenCL burst detection
- [ ] **IRID-6** Outputs — KML export; SigMF / burst-IQ capture; IBC-driven PPM clock self-calibration (`-m ppm`)
- [ ] **IRID-7** Multi-receiver TDOA (`-m tdoa`) — ties XM-4 (IRA ECEF + IBC iri_time primitives already exist)
- [ ] **IRID-8** ⚑ Verify Iridium time re-epoch handling (ERA3 2025-02-14; next 2026-01-14 18:08 UTC) in satellite-naming/SGP4 code — live bug risk (= VERIFY-1)

---

## AIS — AIS / ITU-R M.1371 (A8)

- [ ] **AIS-1** ASM (DAC/FID binary on 6/8/25/26) dispatch table — **the #1 AIS gap** (AIS-catcher decodes ~40 subtypes) ◆ big bet
  - [ ] **AIS-1.1** DAC=1 IMO SN.1/Circ.289 (FID 31/11 meteo-hydro, 21 weather-from-ship, 16 POB, 22/23 area-notice, 17 VTS, 24/25/26 static/cargo/sensor, 27-30 route/text, 32 tidal)
  - [ ] **AIS-1.2** DAC=200 Inland (FID 10/21/22/23/24/40/55)
  - [ ] **AIS-1.3** Regional DACs (235/250/366 AtoN-monitoring, 316/366 Seaway-meteo, 367 US-environmental, 265 STM-route); validate vs pyais oracle
- [ ] **AIS-2** Multi-fragment AIVDM reassembly across sentences (long type 5/6/8/26); type-24 Part A+B merge by MMSI; type-5 voyage merge; per-MMSI `AISTracker` state
- [ ] **AIS-3** Easy sub-field fills ★ quick win — ✅ types 1-3 ROT (`rot_deg_min` helper), position-accuracy, timestamp, maneuver, RAIM; ✅ type-5 ais-version, dimensions (bow/stern/port/starboard), EPFD (named), ETA, DTE *(both hand-verified vs the real type-1 + MT.MITCHELL vectors)*; ❌ remaining: type-21 name-extension/off_position/virtual/dims/epfd/raim, type 4/11 epfd/raim, type-18/19 accuracy/timestamp(+19 dims/epfd), type 24B vendor_id/model/serial/dims/mothership, radio comm-state. *(those per-type layouts need individual verification — the gpsd fast-fetch returned a wrong type-18 timestamp)*
- [x] **AIS-4** AIS-SART / MOB / EPIRB-AIS distress tagging — `fields::distress_class` (MMSI prefix 970/972/974) tags `distress` in `details`, surfaced in console + dashboard vessel; nav_status 14 (`AIS-SART`) and Msg-14 ACTIVE/TEST text already decode. *(VERIFY-4 resolved: 970/972/974 now mapped; ties XM-6 cross-mode distress overlay)*
- [ ] **AIS-5** Outputs — NMEA-over-UDP (+ fix README claim); AIVDO own-ship; NMEA tag-blocks; GPSd output/fusion; NMEA2000/N2K; HTTP aggregator push; per-type/MMSI filtering + rate downsample + output dedup; JSON_FULL `details`; fix `channel_letter` silent default-to-'A'
- [ ] **AIS-6** Demod — low-CPU Pi mode (1.4× headroom is thin); verify CIC5 droop compensation (VERIFY-4)

---

## ADSB — Mode S / ADS-B 1090 MHz (A9)

- [ ] **ADSB-1** Modern accuracy/integrity + intent layer — **entirely absent today** ★ quick win (bits already in-message) — *partially done on `feat/per-decoder-airframes-feeding`*
  - [x] **ADSB-1.1** TC31 operational-status decode → `adsb_status` (version, NIC-supp-A, NACp, SIL, SIL-supp, GVA, NICbaro), surfaced in console/dashboard/asf-2.0. *(NACv is a TC19 field — see ADSB-1.5)*
  - [x] **ADSB-1.2** ADS-B version read directly from TC31. *(heuristic version inference for v0 / non-TC31-emitting aircraft deferred)*
  - [ ] **ADSB-1.3** TC29 target-state (MCP/FCU selected alt, QNH, selected heading, AP/VNAV/APPROACH/LNAV flags)
  - [x] **ADSB-1.4** TC28 aircraft-status — emergency/priority state (mapped to label, flagged on the map) + ACAS-RA subtype flag. *(full RA decode = ADSB-3.1)*
  - [ ] **ADSB-1.5** Accuracy fields — ✅ NACp/SIL/NIC-supp-A/GVA via TC31; ❌ remaining: NUCp(v0), full NIC computation (TC + supplement), NACv (TC19), SDA, Rc
- [ ] **ADSB-2** Position/velocity completion — ✅ TC5-8 surface movement (ground speed) + ground track *(done: `decode::surface_velocity`, riddle-vector tested)*; ❌ remaining: TC9-18 Q=0 Gillham altitude, TC20-22 geometric altitude, VR source bit (GNSS vs baro), geom-minus-baro alt diff, emit NACv
- [ ] **ADSB-3** Comm-B BDS register expansion
  - [ ] **ADSB-3.1** BDS 3,0 ACAS/TCAS RA (high safety/intel value)
  - [ ] **ADSB-3.2** BDS 1,0 data-link-capability, 1,7/1,8/1,9 GICB capability, 2,1 registration markings
  - [ ] **ADSB-3.3** BDS 4,4 (wind/temp/pressure/turbulence/humidity), 4,5 (hazard), 5,3 (air-referenced state)
  - [ ] **ADSB-3.4** rs1090-style density/penalty BDS scoring (vs binary validate); extend inference beyond 4 registers
- [ ] **ADSB-4** DF coverage — DF19 military ES; DF24-27 Comm-D ELM; surface FS/DR/UM (alert/SPI/ground) from DF4/5/20/21
- [ ] **ADSB-5** DF18 CF-subtype classification (CF=0 non-transponder, 2/3/5 TIS-B fine/coarse/mgmt, 6 ADS-R) → source tag (VERIFY-7)
- [ ] **ADSB-6** Mode A/C decode (`--modeac`) — small add (also low-priority NEW; VERIFY-10)
- [ ] **ADSB-7** Demod/trust — phase-classified per-phase bit templates (close the ~3-frame gap to readsb); graduated position trust (json-reliable / position-persistence / NIC-aware)
- [ ] **ADSB-8** Outputs — readsb-schema `aircraft.json` (version/nic/nac/sil/gva/emergency/nav_*/acas_ra/wind/oat); true RX-clock Beast timestamps → MLAT-feedable (ties XM-4, VERIFY-12); mlat/tisb provenance flag; write `docs/notes/ADSB.md`

---

## ECO — Ecosystem / outputs / tooling / dashboard (B)

- [ ] **ECO-1** Persistence, history & replay — on-disk message DB + retention + search UI; per-aircraft trace history; BaseStation flight/aircraft DB; dashboard time-scrubber/replay of decoded history; position-density heatmap
- [ ] **ECO-2** Coverage / range analytics — measured/theoretical range outline (upintheair/HeyWhatsThat/polar), range rings, distance/RSSI columns + per-entity reception
- [ ] **ECO-3** Alerting & notifications — watchlists (tail/flight/ICAO/MMSI/label/text); special-category highlighting (dbFlags mil/interesting/PIA/LADD + emergency squawk 7500/7600/7700); push (Discord/Telegram/webhook/sound); MQTT Home Assistant autodiscovery
- [ ] **ECO-4** REST / data API — `/data/aircraft.json` + `/data/receiver.json` (readsb schema; makes xng drop-in for tar1090/graphs1090/HA); filtered/incremental `?since=`; WebSocket/SSE/delta
- [ ] **ECO-5** Export formats — GeoJSON / KML / GPX; `?screenshot` kiosk + shareable deep-link state
- [ ] **ECO-6** Map/dashboard UX — basemap switcher/overlays/offline MBTiles/weather; persistent trails; map+table filters (alt/speed/type/callsign/source); measure/ruler, gridlines, on-map labels, box-select, tableInView, column sort, dim/contrast/icon-scale controls
- [ ] **ECO-7** Statistics & graphs — built-in graphs page (msgs/min, per-freq, level histogram, CRC/error %, CPU/temp/uptime); expand Prometheus (per-mode/type counts, entity gauges, decode latency, FEC-corrected, reassembly, dropped/lagged-bus); first-party Grafana dashboard JSON
- [ ] **ECO-8** Health / watchdog / operability — health state machine + no-data alarm + auto-restart; continuous autogain during `listen` (VERIFY-7); web config/management UI
- [ ] **ECO-9** Multi-receiver aggregation & integrations — turn `xng ingest` into a fan-in dashboard/dedup hub (ties XM-5); SignalK delta; NMEA2000/N2K; NMEA tag-blocks + community-map push; GPSd input
- [ ] **ECO-10** Enrichment data — bundled/auto-updating aircraft DB + dbFlags; route/airline/operator/airport enrichment on dashboard; persistent vessel registry across restarts
- [ ] **ECO-11** Smaller dashboard/quality — per-mode entity expiry (not flat 300 s); message-stream scrollback backed by ring file; dark/light/scale controls; verify non-Iridium trail antimeridian wrapping (VERIFY-7)
- [ ] **ECO-12** Aggregator-network targets — feed/dedup against airplanes.live / adsb.lol (ODbL) / adsb.fi / OpenSky; study BelugaProject (air+sea fusion prior art)

---

## NEW — New modes / capabilities (D)

> Priority tiers from the consolidated viability matrix. `NEW-SKIP` items are **considered and declined** — kept so they aren't re-proposed.

- [ ] **NEW-P0-1** UAT 978 MHz — PRIORITY #1 (open DO-282B; same RF chain; fuses with `adsb`)
  - [ ] **NEW-P0-1.1** UAT ADS-B short/long messages (US GA positions)
  - [ ] **NEW-P0-1.2** FIS-B weather products (NEXRAD/METAR/TAF/PIREP/AIRMET-SIGMET/Winds-Temps/NOTAM/TFR/SUA)
  - [ ] **NEW-P0-1.3** TIS-B + ADS-R (`uat2esnt` → DF18 CF=6 integration path)
- [ ] **NEW-P0-2** COSPAS-SARSAT 406 MHz ELT/EPIRB/PLB — FGB (biphase-L, T.001) + SGB (OQPSK+DSSS, T.018); ties XM-6
- [ ] **NEW-P0-3** DSC — Digital Selective Calling, VHF Ch70 (FFSK 1200, BCH(10,7)) + HF/MF (M.493); GMDSS distress; ties XM-6
- [ ] **NEW-P1-1** Radiosondes (RS41/RS92/DFM/M10/M20/iMet/LMS6/iMS-100/MRZ) — GFSK 400–406 MHz; SondeHub/APRS-IS feed; fits the scanner
- [x] **NEW-P1-2** AIS-SART / MOB / EPIRB-AIS — done under **AIS-4** (cross-reference)
- [ ] **NEW-P1-3** EOT / HOT / DPU rail telemetry — 457/452 MHz Manchester FSK, BCH, clear (brake-pipe pressure, motion, marker) → live train-tail map
- [ ] **NEW-P1-4** NAVTEX — 518/490/4209.5 kHz SITOR-B / CCIR 476; pairs with DSC (HF FEC shared)
- [ ] **NEW-P2-1** ADS-L + FANET / OGNTP — EASA SRD860 868/915 MHz 2-GFSK + open LoRa-CSS; do before FLARM
- [ ] **NEW-P2-2** APRS / AX.25 (incl. HAB balloons) — AFSK1200 144.39/432; igate-style feed (demod reused by POCSAG)
- [ ] **NEW-P2-3** POCSAG / FLEX / FLEX-NEXT paging — reuses FSK chain; airport/airline/EMS ops-paging angle (VERIFY-11)
- [ ] **NEW-P2-4** ATCS 900 MHz rail signaling — FSK 4800 HDLC-LAPB, clear (Spec-200/ARES/Genisys/SCS-128 payload zoo)
- [ ] **NEW-P2-5** VDES / long-range AIS extensions — ASM channels (AIS 27/28 → ASM1/2); AIS-2.0 readiness (ITU-R M.2092)
- [ ] **NEW-P3** Parking lot (low priority / dependent) — FLARM (after open OGN), LoRa-APRS/Horus HAB (radiosonde rider), DMR-LRRP / P25-Unit-GPS / TETRA-SDS-LIP position PDUs (metadata only), TPMS (rtl_433 subset), Orbcomm STX (breadth flex), GTFS-realtime ingest (non-RF complement), WSPR/FT8 (HF-prop health niche)
- [ ] **NEW-SKIP** Considered & declined (documented in D) — VDL Mode 4 (regional/near-dead), DSRC/C-V2X (wrong band, sunsetting), toll/MDT, AEI/Eurobalise (passive/near-field), PTC/GSM-R/FRMCS (encrypted), weather imagery NOAA/Meteor/GOES (SatDump incumbent), cubesat/SatNOGS, Globalstar/Starlink/rocket (proprietary/encrypted), GNSS/SBAS (no entities), Drone Remote ID (off-axis; only via ADS-L Issue-2), WEFAX/Pactor/Winlink, wM-Bus, ERMES
- [ ] **NEW-V** Verify before committing — (a) which airports/airlines still run POCSAG/FLEX ground-ops paging; (b) AeroMACS/Gatelink demand; (c) whether xng L-band front-ends pass 406 MHz for SARSAT reuse; (d) whether generic-ISM (TPMS) scope is desired (= VERIFY-11)

---

## VERIFY — Research / correctness checks (Appendix + inline ❓)

> Mostly "read the code / confirm against a source" tasks; resolve these to confirm or retire the matching items above. `⚑` = possible live bug.

- [ ] **VERIFY-1** ⚑ Iridium time re-epoch correctness in satellite-naming/SGP4/TLE code (= IRID-8)
- [ ] **VERIFY-2** ACARS — raw MIN / 4th-char downlink-rule edge cases; per-label ACARS Prometheus counters exist?; any media-advisory v1+ exists?
- [ ] **VERIFY-3** VDL2 — SABME folded into `U?`?; Call-Request maintenance/init status bit; ES-IS option-TLV coverage; plain/unprotected CPDLC PDUs; StatsD `good_loud`; `pp_acars`; cross-check v2.5.1 249-octet-multiple block-length bug
- [ ] **VERIFY-4** AIS — CIC5 droop compensation in xng-dsp; mid-frame polarity-flip recovery. *(MMSI 970/972/974 mapping: RESOLVED — added in AIS-4)*
- [ ] **VERIFY-5** HFDL — LMS equalizer tap count 7 vs documented 15
- [ ] **VERIFY-6** STD-C — `0xB0` vs `0xB1`+`0xB2` distinction in surfaced details; mid-frame polarity flip recovery
- [ ] **VERIFY-7** Ecosystem — dashboard station/receiver-position pin exists?; continuous autogain during `listen`?; non-Iridium trail antimeridian wrapping?; DF18 CF-subtype classification (= ADSB-5)
- [ ] **VERIFY-8** Aero-C — exact `AEROTypeP/R` enumerator hex vs JAERO source before encoding a SU-type table
- [ ] **VERIFY-9** ACARS — acarsdec `mqttout.c` in current f00b4r0 4.x tree (post-SoapySDR refactor)?; xng per-label ACARS counters?
- [ ] **VERIFY-10** ADS-B — does `xng-mode-adsb` already emit Mode A/C? (README implies not; = ADSB-6)
- [ ] **VERIFY-11** New-mode commitments — POCSAG/FLEX airfield usage; AeroMACS/Gatelink demand; 406 MHz front-end reuse; generic-ISM scope desire (= NEW-V)
- [ ] **VERIFY-12** Beast MLAT counter usability — is the blocker jitter/monotonicity of system-clock timestamps rather than absence of GPS? (sharpen before citing as a hard MLAT blocker)
- [ ] **VERIFY-13** Feed — confirm VDL2 ingest format preference (dumpvdl2 `decoded:json` vs vdlm2dec); whether Airframes exposes public IMSL/IRDM/STD-C ports + the settled Iridium feeding mechanism (= FEED-4)
