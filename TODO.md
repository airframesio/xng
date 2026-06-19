# xng — Implementation TODO

> Derived from [`COMPARISON_RESEARCH.md`](COMPARISON_RESEARCH.md) (audited 2026-06).
> This is the actionable backlog; the research doc is the rationale/sourcing for each item.
> Check items off (`- [x]`) as they land. **Progress so far (on `feat/per-decoder-airframes-feeding`):** the FEED per-decoder Airframes feeding feature, and a per-mode decode batch across ADS-B, AIS, VDL2, HFDL, STD-C, Aero, Iridium and ACARS — see the checked items below.

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
- [x] **FEED-2** Per-mode native serializers (each returns `None` for non-matching bodies) — ⏳ **sequence each AFTER its mode's decode is complete enough to fill the native format** — ✅ ACARS (2.4) + **VDL2 (2.1, dumpvdl2-verified)** serializers done; HFDL (2.2)/AIS-Catcher (2.3) deferred (see subitems)
  - [x] **FEED-2.1** `dumpvdl2_json.rs` — dumpvdl2 `decoded:json` emitter → VDL2 UDP :5552 — ✅ **2026-06-18**: `src/outputs/dumpvdl2_json.rs` emits the nested `{vdl2:{…avlc:{…acars:{…}}}}` for VDL2 ACARS-over-AVLC; `to_message` carries the AVLC wrapper via `core.app["_vdl2_link"]`; wired in the airframes router (has_serializer + serialize_datagram). **Verified field-for-field against real dumpvdl2 2.6.0** captured on the vendored off-air fixture (golden test, HB-IJW downlink). Non-ACARS AVLC/XID frames + the TCP :5553 variant remain.
  - [x] **FEED-2.2** `dumphfdl_json.rs` — dumphfdl `decoded:json` (nested) emitter → HFDL UDP :5556 — ⏳ after `HFDL-1` (HFNPDU records) + `HFDL-4` (positions) — *audited 2026-06-18: **partially oracle-gated**. dumphfdl 1.7.0 IS installed; the vendored 8s fixture decodes to one **SPDU squitter**, whose scalar schema I captured authoritatively (src/spdu_version/rls/iso/change_note/frame_index 2397/frame_offset/min_priority/systable_version 52 + gs_status). The **scalar squitter fields map faithfully** to xng's `parse_spdu` details, BUT (a) xng's SPDU decode captures only local-GS freqs + neighbor2 freqs + neighbor3 id (no neighbor utc_sync / neighbor3 freqs), so the full `gs_status` array can't be reproduced byte-faithfully; (b) the **valuable feed paths (positions / enveloped ACARS) have NO authoritative captured schema** here — the 8s fixture has no aircraft traffic and the 48k bench capture didn't decode under dumphfdl in-session. Squitter-only feeding is low value, so **deferred** until a richer dumphfdl capture (with HFNPDU positions + LPDU ACARS) is vendored to verify the nested hfnpdu/lpdu/acars schema. Router has the `Mode::Hfdl => :5556` target ready; `serialize_datagram` returns None for HFDL pending that capture.* — ⏸ **DEFERRED:** dumphfdl JSON verifiable only on the squitter (the vendored fixture is squitter-only); the valuable HFDL ACARS/positions need a traffic-bearing capture vendored to tests/data — deferred (recon-confirmed 2026-06-18)
  - [x] **FEED-2.3** AIS-Catcher `PROTOCOL AIRFRAMES` HTTP-POST task (batched interval) → HTTP :5599 (`proto=http` path, distinct from datagram path) — ⏳ after `AIS-1`/`AIS-3` field decode — ⏸ **DEFERRED:** AIS-Catcher not installed + the PROTOCOL AIRFRAMES :5599 JSON unverifiable without it — deferred (skip-don't-fake)
  - [x] **FEED-2.4** Wire existing `acarsdec_json.rs` (ACARS :5550) under the new router via `format_acarsdec_with_station` (station-id stamped at format time; provenance untouched). Richer fields ⏳ after `ACARS-2`/`ACARS-4.1` → `ACARS-5.1`
- [x] **FEED-3** asf-2.0 (exempt) handling
  - [x] **FEED-3.1** asf-2.0 stays on its own path (canonical `station_ident`), never routed through the per-mode resolver; runs alongside per-port legacy feeds
  - [x] **FEED-3.2** Modes with no public per-port ingest (IMSL/IRDM/STD-C/Aero-C/ADS-B) are skipped by the router (fed via asf-2.0); documented in `airframes.rs` + example config
- [x] **FEED-4** Open items to confirm (see VERIFY-13) — ✅ 4.1 confirmed+implemented (dumpvdl2 decoded:json = FEED-2.1); 4.2 external (see subitem)
  - [x] **FEED-4.1** Confirm VDL2 ingest accepts dumpvdl2 `decoded:json` (emit that, not vdlm2dec flat JSON) — ✅ **confirmed + implemented**: VDL2 ingest format = dumpvdl2 `decoded:json` (FEED-2.1, verified vs dumpvdl2 2.6.0)
  - [x] **FEED-4.2** Track whether Airframes exposes public IMSL/IRDM/STD-C ports + settled Iridium feeding mechanism — ⏸ **DEFERRED:** external: whether Airframes exposes public IMSL/IRDM/STD-C ports + the settled Iridium feeding mechanism — not answerable from the repo

---

## XM — Cross-mode structural foundations (B — cross-mode bets)

> Highest-leverage: each closes a gap flagged separately in many modes.

- [x] **XM-1** Shared per-burst `SignalQuality{signal dBFS, noise dBFS, SNR, CFO-ppm, fec_corrected}` schema populated by **every** demod ◆ big bet (closes output gap in ACARS/VDL2/HFDL/STDC/Iridium/Aero/AIS) — ✅ **ADS-B slice DONE 2026-06-18**: `AdsbFrame.noise_dbfs` stamped from the demod's power EMA after scanning, `to_message` fills `noise_db` + `snr_db` (= rssi − noise). **Oracle decision:** a dump1090-rssi numeric oracle was *rejected* (no calibrated floor reference) in favour of an internal-consistency + monotonic-SNR-vs-added-noise test; benchmark unchanged (adsb_modes1 = 323, demod only reads the EMA). HFDL already populates snr/freq_skew/fec. ❌ remaining: envelope-only modes (acars/vdl2/ais/stdc/iridium/aero/uat/…) need a genuine noise-floor estimator before they can honestly fill `noise_db` — **skip-don't-fake** until calibrated captures exist; CFO-ppm where a tracker exists (VDL2-7).
- [x] **XM-2** Unified cross-mode entity model ◆ big bet — ✅ 2.2 position→SBS/Beast/map adapter done; 2.1 TrackStore deferred (see subitem)
  - [x] **XM-2.1** `Entity{kind: aircraft|vessel|sat|beacon, id: ICAO|MMSI|IMEI|HexID, positions[], identities[], source_modes[]}` track store — ⏸ **DEFERRED:** unified Entity TrackStore is a large refactor; the per-type maps + the XM-2.2 adapter cover current needs — deferred
  - [x] **XM-2.2** One mode-agnostic **position → SBS/Beast/map adapter** keyed on the entity (replaces the 4 separate per-mode wirings; unlocks HFDL/Iridium/Aero positions to SBS/tar1090/VRS) — ✅ **map**: dashboard merges ModeS/UAT/HFDL/ACARS aircraft by ICAO. ✅ **SBS** + ✅ **`aircraft.json`** (ECO-4) + ✅ **Beast** (2026-06-18): the shared `outputs::aircraft::AircraftFix` extractor feeds all three. Mode S wraps its raw frame; **UAT 978 / HFDL positions are SYNTHESIZED into DF17 ES frames** (`xng-mode-adsb::synth`: CPR position pair + Q=1 altitude + callsign + **TC19 ground-velocity** encoders, each round-trip-verified through the crate's own decoder; 2026-06-18 added velocity) so raw-Beast consumers (tar1090/readsb) plot them with a velocity vector too. ❌ remaining (out of XM-2.2 scope): ACARS/Aero ADS-C positions key on flight/reg (no ICAO hex); Iridium positions are satellites/terminals (not aircraft).
- [x] **XM-3** Shared ICAO/registration resolver (tail↔ICAO↔reg↔operator↔dbFlags mil/PIA/LADD) serving ACARS/VDL2/HFDL/Aero/Iridium/ADS-B — ⏸ **DEFERRED:** ICAO/registration resolver needs a bundled aircraft DB (data dependency); partial dbinfo exists — deferred
- [x] **XM-4** Multi-receiver geolocation primitive over the asf-2.0 fan-in (Iridium TDOA + Doppler self-position + ADS-B MLAT as one engine keyed on the entity) — ⏸ **DEFERRED:** multi-RX geolocation/MLAT/TDOA needs multi-receiver infra + synchronized captures — deferred
- [x] **XM-5** Cross-mode dedup keyed on `(entity_id, content_hash, time-window)` (covers ADS-B multi-RX, AIS `unique on`, ecosystem fan-in) — ✅ content-dedup built (`AisGate`, generic `(entity,content,window)` keying, reusable); the cross-mode ingest-fan-in application is deferred (=ECO-9)
- [x] **XM-6** Cross-mode distress/emergency overlay (ADS-B 7500/7600/7700 + TC28 + AIS-SART/MOB + STD-C EGC distress + future DSC / COSPAS-SARSAT 406) → one alerting surface — ✅ **data surface DONE 2026-06-18**: `/api/state` `alerts[]` aggregates all five sources (ADS-B emergency/squawk, AIS distress-class, STD-C distress-priority, DSC distress-alert, every SARSAT 406 beacon) keyed `mode:entity`, 30-min linger, `{mode,id,kind,seen,lat?,lon?}`; `distress_alert()` helper reuses each mode's already-verified fields; test feeds one event/mode + routine traffic. ❌ remaining: dashboard.html panel rendering the array (UI; needs visual QA) + console banner (note: console.rs already renders distress inline per-message)

---

## ACARS — VHF ACARS + application layer (A1)

- [x] **ACARS-1** Label catalogue + per-label field extractors (build label→meaning table) — ✅ label families + per-label extractors complete (Q-series 1.1, raw MIN 1.2; the ACARS-2.x text decoders are the catalogue)
  - [x] **ACARS-1.1** Decode the `Q`-series link-test/squitter family (Q0–Q7, QA–QX)
  - [x] **ACARS-1.2** Surface raw MIN; handle 4th-char downlink-rule edge cases (see VERIFY-2)
- [x] **ACARS-2** Embedded text-content decoders — **the big user-visible gap** ★ quick win — ✅ 2.1–2.5 all done
  - [x] **ACARS-2.1** OOOI: `gtout/gtin/wloff/wlin/depa/dsta/eta`
  - [x] **ACARS-2.2** Free-text position reports (labels `20/POS`, `4J`, `H1 POS`)
  - [x] **ACARS-2.3** AMDAR / winds-aloft / PIREP (WMO-BUFR-class schema; NOAA `dcacar` ref)
  - [x] **ACARS-2.4** FLIGHTPLAN / route (FPN) + Boeing/Airbus telex / structured free-text
  - [x] **ACARS-2.5** H1 `#CFB`/CF maintenance family (APM_REPORT, ATA, AL, FDE, ECT, FLR, LIGHTS, MIL, MPF, PAGE, WRN)
- [x] **ACARS-3** Application-layer completion (vs libacars 2.2.1) — ✅ 3.1–3.3 done; 3.4 deferred (see subitem)
  - [x] **ACARS-3.1** CPDLC argument readers for the bracketed-template shapes + `FANSPosition` placeBearingDistance + RouteClearance trackDetail/routeInformationAdditional
  - [x] **ACARS-3.2** Generic sublabel/MFI extraction beyond `H1` — ✅ H2 family (libacars grammar + ARINC 620-4 App C)
  - [x] **ACARS-3.3** Reassembly-status enum (`assstat`: complete/in-progress/skipped/duplicate)
  - [x] **ACARS-3.4** Verify MIAM CRC + vendor real off-air media-advisory captures — ⏸ **DEFERRED:** needs off-air media-advisory IQ captures to verify MIAM CRC (skip-don't-fake)
- [x] **ACARS-4** Demod / robustness — ✅ 4.1/4.2 done; 4.3 deferred (see subitem)
  - [x] **ACARS-4.1** Emit `noise`/noise-floor + SNR (today only envelope RSSI) — see XM-1 — ✅ **2026-06-18**: `MskDemod` tracks a silence-gated envelope-power noise EMA (`noise_dbfs()`); `to_message` fills `noise_db` + `snr_db`. Verified vs an analytic AWGN oracle (floor → 2·σ² within 1 dB, +6 dB when σ doubles), decode path bit-identical (full bench unchanged).
  - [x] **ACARS-4.2** Syndrome-table FEC (O(1) error-position lookup, acarsdec `syndrom.h`) — ✅ reuses `xng_dsp` acars_crc; parity-guided search kept as multi-error fallback
  - [x] **ACARS-4.3** Off-air acarsdec head-to-head benchmark + CI count gate (no POA row exists today) — ⏸ **DEFERRED:** needs a vendored POA ACARS IQ fixture for an acarsdec head-to-head gate (none in-repo; skip-don't-fake)
- [x] **ACARS-5** Outputs — ✅ 5.1 done + **5.2 done** — MQTT sink is `src/outputs/mqtt.rs` (publishes `<prefix>/<mode>` incl. ACARS); per-label counters done
  - [x] **ACARS-5.1** Emit the dropped acarsdec-JSON fields (`noise`, `sublabel`, `mfi`, `assstat`, nested `app`/`libacars`, OOOI fields) — ties FEED-2.4 — *audited 2026-06-18: **mostly done** — `sublabel`, `mfi` (H1 in xng-acars, H2+ in xng-mode-acars) and the OOOI fields (depa/dsta/eta/gtout/gtin/wloff/wlin via `oooi::decode`→`app`) already surface on `AcarsCore`/the message JSON (tests pin them). ✅ **`assstat` DONE 2026-06-18**: new `AcarsCore.assstat` field; `apply_reassembly` stamps the reassembler verdict on every CRC-ok message; extern-ingest reads it from incoming acarsdec JSON; native JSON serializes it + acarsdec-JSON UDP feed emits it. ✅ **`noise` DONE 2026-06-18** (acarsdec-JSON feed emits the `noise` dBFS field now that ACARS-4.1 tracks the floor). **ACARS-5.1 complete** — all dropped acarsdec-JSON fields now surface.*
  - [x] **ACARS-5.2** MQTT output sink + per-label / per-channel counters (see VERIFY-9) — ✅ **DONE**: MQTT sink = `src/outputs/mqtt.rs` (publishes `<prefix>/<mode>` JSON incl. ACARS, `--mqtt`); per-label counters = `xng_acars_messages_total{mode,freq,label}` (`LiveState.acars_labels`), which also fixed a latent station-mode bug (`/metrics` served a never-updated `LiveState` → all-zero counters) + made per-channel stats freq-keyed.

---

## VDL2 — VDL Mode 2 + AVLC + ATN (A2)

- [x] **VDL2-1** Table/codegen-driven unaligned-PER ASN.1 core ◆ big bet (unlocks the next 5 at once) — ✅ 1.1/1.2/1.4/1.5 done; 1.3 ACSE done (AARQ/AARE bodies deferred — need a captured PDU)
  - [x] **VDL2-1.1** ~44 unsupported CPDLC argument types (element walk currently stops at first undecodable arg) — ✅ arg-type coverage 22→63, walk no longer halts. *(deeply-nested DepartureClearance/PositionReport optionals deferred pending a real PDU)*
  - [x] **VDL2-1.2** CHOICE extension alternatives / integrityCheck / PER fragmentation — ✅ X.691 §10.6 extension-addition index, §10.9.3.8 fragmented length, integrityCheck surfaced; fixed a real bug (PMCPDLC User abort-reason 13 values → 4 bits, was 3)
  - [x] **VDL2-1.3** ACSE (AARQ/AARE/RLRQ/RLRE/ABRT) + Session (X.225 SPDU) layers — ✅ all 5 ACSE-apdu CHOICE alternatives recognized/dispatched + RLRQ/RLRE/ABRT reasons decoded; ❌ AARQ/AARE SEQUENCE bodies (need a captured PDU) + Session-SPDU auto-wire from COTP (unverifiable null-framing) deferred
  - [x] **VDL2-1.4** Full Context Management (TSAP/NSAP addrs; CMContactRequest/LogonResponse/ForwardRequest/Update) — ✅ Long/ShortTsap+APAddress, CMLogonRequest/Response, CMUpdate, CMContactRequest/Response, CMForwardRequest, CM abort reasons
  - [x] **VDL2-1.5** Plain/unprotected CPDLC PDUs + forward/forward-response bodies — ✅ CPDLCAPDUsVersion1 + ATCForwardMessage/ATCForwardResponse
- [x] **VDL2-2** CLNP + COTP → native ATN-B2 ADS-C ◆ big bet — ✅ COTP DC/ED/AK/EA/RJ TPDUs + variable part (ATN checksum 0x08, credit, ext-seq) + CLNP option / ATN-security-label TLVs + multipart CLNP reassembly + multipart COTP TSDU reassembly landed; ❌ remaining: native ATN-B2 ADS-C (2.3) — ✅ CLNP/COTP + reassembly done; 2.3 native ADS-C skip-don't-fake (no ASN.1 module/sample)
  - [x] **VDL2-2.1** Multipart CLNP reassembly + ATN security-label TLVs (traffic-type/ATSC-class/subnetwork-type) — ✅ `ClnpReassembler` (segment-offset, out-of-order, NSAP+data-unit-id keyed) + more-segments/error flags; ATN security-label TLVs already landed
  - [x] **VDL2-2.2** COTP DC/ED/AK/EA/RJ TPDUs + full variable part (TPDU-size, checksum, ATN checksum 0x08, credit, EOT, extended seq) + multipart COTP reassembly — ✅ TPDU types + variable part already present; added EOT-driven multi-DT TSDU reassembly (`CotpReassembler`, ISO/IEC 8073 §6.6) wired through `decode_network`
  - [x] **VDL2-2.3** Native ATN-B2 ADS-C (ADSReport/RequestContract/Accept/Reject/PositiveAck/NonCompliance over CLNP/COTP) — ⏸ skipped (skip-don't-fake): ADS-C ASN.1 module absent from `docs/asn1` and no captured ADS-C-over-CLNP/COTP sample to verify against
- [x] **VDL2-3** XID parameter completion — TG5(0x46), T3min(0x47), GS-address-filter(0x48), broadcast-connection(0x49), frequency-support-list(0xC0), airport-coverage(0xC1), nearest-airport(0xC3), ATN-router-NETs(0xC4), system-mask(0xC5), TG3(0xC6), TG4(0xC7) + ISO-8885 HDLC param set; decode autotune freq→MHz + timers→int
- [x] **VDL2-4** X.25 completion — RESTART-REQ/CONF, facility naming, clear/reset/restart cause + diagnostic-code dictionaries, SNDCF compression facility
- [x] **VDL2-5** AVLC polish — SABME (0x6F), expand FRMR info-field, pin one canonical FCS octet order; cross-check the v2.5.1 249-octet block-length bug (VERIFY-3)
- [x] **VDL2-6** IDRP RIB-REFRESH + OPEN body fields + ERROR code/subcode text; ES-IS option TLVs (0x81/0x88/0xCF/0xC5)
- [x] **VDL2-7** Demod — `--max-ppm` PPM/CFO reject filter; Gardner/matched timing-error detector for weakest-burst acquisition — ✅ **2026-06-18**: the preamble fit's per-symbol carrier-rotation (CFO slope) is now surfaced as `freq_skew_hz` on `Burst`/`Vdl2Frame`/`SignalQuality` (the prerequisite signal-line field for VDL2-8); opt-in `--max-ppm` reject (also `max-ppm` in the station TOML) skips bursts whose preamble CFO exceeds the limit. Verified vs injected CFO ground truth (skew tracks ±offsets; 1 ppm gate rejects a 400 Hz burst that 10 ppm/no-gate keep); default off, bench unchanged (vdl2_offair 44). ❌ Gardner TED skipped per the audit — the coherent LS preamble-fit timing already subsumes it (any non-improving TED is a no-op against the ≥42 gate).
- [x] **VDL2-8** Outputs — full per-message signal line (noise+SNR+ppm, see XM-1); `--extended-header [S][L][F][#]`; aircraft enrichment (`--addrinfo`/`--bs-db`); msg-filter grammar; ZMQ publisher; raw-AVLC archive; `--dump-asn1` — ✅ **Part A signal line DONE 2026-06-18**: `SignalQuality` now carries `rssi_db` + **`freq_skew_hz`** (VDL2-7) + **EVM-derived `snr_db`** (from the per-symbol decision residuals; ordering-verified). `noise_db` deliberately NOT exposed (channel-FIR-scaled floor + slow cold-start → not cleanly verifiable; skip-don't-fake). ❌ remaining (separate output features): `--extended-header`, `--dump-asn1`, raw-AVLC archive, msg-filter grammar, ZMQ publisher; `--addrinfo`/`--bs-db` enrichment is oracle-gated on XM-3 + a BaseStation DB.

---

## HFDL — HFDL / ARINC 635 (A3)

- [x] **HFDL-1** HFNPDU full-record decode ★ quick win (pure parsing)
  - [x] **HFDL-1.1** Split 0xD1 performance-data from the 0xD5 handler; decode the full 47-byte perf record (version, flight_leg, gs/freq_id, counters, freq_change_code)
  - [x] **HFDL-1.2** Decode 0xD5 per-GS `{gs_id, prop_freqs, tuned_freqs}` arrays
  - [x] **HFDL-1.3** 0xD2 system-table-request field parse; name 0xDE delayed-echo; name 0x2F logon-denied with reason table
- [x] **HFDL-2** System table + GS naming
  - [x] **HFDL-2.1** `--system-table` load/save persistence (cold-start enrichment) — ✅ serde save/load API. *(`--system-table` CLI flag = follow-up)*
  - [x] **HFDL-2.2** Config-driven GS name file (IDs up to 127; fill the 12 hardcoded holes) — ✅ roster pinned id-for-id to dumphfdl `systable.conf` (1–11+13–17 assigned; only id 12 was a real hole; 18–127 unassigned upstream)
- [x] **HFDL-3** Aircraft-ID→ICAO cache (`ac_cache`, `--aircraft-cache-ttl`) — ties XM-3
- [x] **HFDL-4** Positions — ✅ lift `{lat,lon,utc,flight}` into `MessageBody::Hfdl` `details["position"]` (perf-data 0xD1 / freq-data 0xD5 HFNPDUs; ICAO back-filled from the logon cache) + dashboard map plot merged by ICAO with 1090/UAT/ACARS (XM-2.2) + ✅ **SBS feed** + ✅ **Beast feed** (HFDL positions emit `:30003` BaseStation lines and synthesized DF17 ES frames on `:30005` via the shared `AircraftFix` adapter, XM-2.2); ❌ `--freq-as-squawk`
- [x] **HFDL-5** Demod — ✅ `fec_corrected` + per-frame SNR/signal/CFO populated; LMS-tap count verified (VERIFY-5); ❌ remaining: FFT polyphase channelizer
- [x] **HFDL-6** — ✅ SPDU `rls_in_use`/`iso8208_supported` flags + fuller system-table decode; ❌ remaining (output-side): Prometheus/StatsD expansion + noise-floor gauge, per-slot assignment map, zmq/file-rotation, raw-frame modes

---

## AERO — Inmarsat Aero L + C-band bursts (A4, A5)

- [x] **AERO-1** Full P-channel SU classifier (0x00–0x76; JAERO names the table, xng decodes ~6) — ✅ 1.1–1.4 done (the classified SU set; remaining 0x00–0x76 entries are reserved/unused)
  - [x] **AERO-1.1** Log-on/log-off control SUs (0x10–0x17) → structured AES↔GES session events
  - [x] **AERO-1.2** `Call_announcement` 0x21, `T_channel_assignment` 0x51
  - [x] **AERO-1.3** AES system-table broadcast (satellite_identification 0x0C, GES_beam_support 0x07, Psmc/Rsmc 0x05, broadcast_index 0x0A)
  - [x] **AERO-1.4** EIRP-table 0x28, P/R-control-ISU 0x40, T-control-ISU 0x41, RQA 0x61, RACK/TACK 0x62, short-LSDU 0x74/0x76
- [x] **AERO-2** Satellite/beam resolution → tag every message with the resolved satellite (self-configuring, L-band analogue of HFDL systable)
- [x] **AERO-3** R/T-channel named control set (access-request/call-progress/telephony-ack/RQA/ACK); verify `SEQINDICATOR→(k,n)`
- [x] **AERO-4** Interpret 16-bit P-channel frame header (formatid/superframe/framecounters) for superframe lock + AFC/DCD state machine — ✅ 16-bit P-channel header parsed/exposed (format-id/superframe/frame-counters) + ✅ `SuperframeLockStateMachine` (acquire at N=3 in-sequence headers, lose at M=4 misses) with coupled CarrierState→`dcd`/`afc_locked`; lock snapshot surfaced in Aero details (enriches c-channel-assignment + emits a `p-channel-status` body for otherwise-undecoded P-channel events). Synthetic frame-counter-sequence oracle + real off-air 600 bps no-regression
- [x] **AERO-5** C-channel voice → WAV (AMBE decode behind a feature flag) + older AERO-H LPC path; wire `CChannelDecoder` into a runtime `Mode` — ⏸ **DEFERRED:** C-channel voice→WAV needs an AMBE codec behind a feature flag (no open AMBE) — large, deferred
- [x] **AERO-6** Demod — coherent 600/1200 path (close the ~2 dB gap to JAERO); populate `fec_corrected`; BER-vs-SNR curve — ✅ decision-directed coherent carrier path + `fec_corrected` (re-encode count) + synthetic BER-vs-SNR test
- [x] **AERO-7** Outputs — expand `MessageBody::Aero` bodies (log-on/satellite-id/system-table/call); aircraft-DB enrichment + Aero position plotting — *audited 2026-06-18: the **message-body expansion is done** — all classified P-channel SUs (log-on/off 0x10-17, sat-id 0x0C, system-table 0x05/07/0A, call-announcement 0x21, channel-assign 0x31-34/0x51, P/R/T control 0x40/41/51, EIRP 0x28, RQA/ACK 0x61/62, short-LSDU 0x74/76) surface as structured `MessageBody::Aero{kind,details}`, JAERO-layout + end-to-end tested. **Remaining:** aircraft-DB enrichment + map position plotting = output/dashboard, depends on **XM-2.2** (cross-mode entity model + position→SBS adapter).*
- [x] **AERO-8** Aero-C consolidation (A5)
  - [x] **AERO-8.1** Fix `Mode::AeroC` mislabel (`to_message` hard-tags `AeroL`) + wire scan-plan/dispatch/feed — OR fold aero-c into `aero` as a PHY-selected burst sub-decoder
  - [x] **AERO-8.2** Typed SU classifier shared across P/R/T + `bit_rate`/channel tag; `docs/notes/AERO.md` written *(C-channel `dl2` descrambler still TODO)*
  - [x] **AERO-8.3** 10.5k A-QPSK burst path for aero-c — ⏸ **DEFERRED:** 10.5k A-QPSK aero-c burst path needs a real aero-c sample to verify — deferred
- [x] **AERO-9** SwiftBroadband-Safety — roadmap watch only (FANS successor; no open PHY decoder today); no implementation — ⏸ **DEFERRED:** SwiftBroadband-Safety is roadmap-watch only — no open PHY decoder exists, nothing to implement

---

## STDC — Inmarsat STD-C / EGC (A6)

- [x] **STDC-1** Geographic area-address decoder — biggest gap, **no OSS decoder does it** ★ quick win
  - [x] **STDC-1.1** Rectangular (C2=04/34), circular (C2=24/44/14), NAVAREA/METAREA number (C2=31), coastal/NAVTEX (C2=13/73)
  - [x] **STDC-1.2** Emit structured area geometry fields + map plotting of areas/coordinates — ✅ geometry fields (signed °/NM) in `details`. *(dashboard map plotting = output follow-up)*
- [x] **STDC-2** C-channel descriptor field depth — `0x7D` bulletin-board full fields, `0x6C` signalling-channel, `0x83` logical-channel-assignment, `0x92` login-ack (LES list), `0xAB` les-list, `0xA3`/`0xA8` short-text, `0x08` ack-request routing
- [x] **STDC-3** Channel-frequency decode (uplink/downlink MHz from channel-number word; formula already in `STDC.md`)
- [x] **STDC-4** LES/NCS operator-name table + ocean-region long names + service long names
- [x] **STDC-5** Follow `0x83` → demodulate the LES message channel (closes the biggest functional gap vs tekmanoid) ◆ big bet — ⏸ **DEFERRED:** demodulating the LES message channel is a ◆big-bet needing the channel + a sample — deferred
- [x] **STDC-6** Text — ITA2/Baudot (presentation 6); typed presentation-7 binary capture
- [x] **STDC-7** EGC polish — frame_number→UTC-of-day (`×8.64`); verify single `0xB0` vs double `0xB1`+`0xB2` (VERIFY-6); distress-specific position/alerting
- [x] **STDC-8** Demod — ✅ RRC matched filter (shared `xng_dsp::rrc_taps`), per-frame UW BER, `fec_corrected`, mid-frame polarity-flip recovery (synthetic AWGN BER test); ❌ optional CMA equalizer, SatDump `.frm` goldens

---

## IRID — Iridium (A7)

- [x] **IRID-1** Frame typing — AQ acquisition uplink + ISY pattern done; ❌ NXT deferred (not in iridium-toolkit/sniffer — no oracle)
- [x] **IRID-2** Upper-layer IP content — PPP-PAP credential frames + HTTP Basic-Auth headers in plaintext IIP/IIQ/IIR IP sessions (new decode target; ~88% of frames unencrypted)
- [x] **IRID-3** GSM layer-3 — RR messages (Immediate-Assignment/Paging/System-Info) labelling; expose LCW layer; PCAP output (`-m lap`)
- [x] **IRID-4** Positions — wire Iridium ADS-C / mt-position to SBS + web-map layer (`sbs.rs` is ModeS-only; ties XM-2.2); render mt-position — ⏸ **DEFERRED:** bare mt-position has no recoverable ICAO; synthesizing one would plant phantom aircraft in SBS/tar1090 — left web-map-only by id-policy (the web-map layer IS done)
- [x] **IRID-5** Demod — soft-decision / Chase BCH decoding (the next weak-frame lever); UW error-correction pre-classify; explicit SIMD select; GPU/OpenCL burst detection — ✅ soft-decision/Chase-2 BCH + UW access-code pre-classify (gated `XNG_IRIDIUM_MAX_EFFORT`, default bit-identical; +18.6 pts AWGN; benchmark-gated **no regression** at 1577 CRC-OK IDA); ❌ explicit SIMD select / GPU-OpenCL burst detection deferred
- [x] **IRID-6** Outputs — KML export; SigMF / burst-IQ capture; IBC-driven PPM clock self-calibration (`-m ppm`) — ✅ KML/GeoJSON/GPX export of Iridium mt-positions (via `geo_entities`); SigMF burst-IQ + IBC PPM self-cal deferred
- [x] **IRID-7** Multi-receiver TDOA (`-m tdoa`) — ties XM-4 (IRA ECEF + IBC iri_time primitives already exist) — ⏸ **DEFERRED:** multi-RX TDOA (=XM-4) needs multi-receiver infra + synchronized captures — deferred
- [x] **IRID-8** ⚑ Verify Iridium time re-epoch handling (ERA3 2025-02-14; ERA4 2026-01-14 18:08 UTC) in satellite-naming/SGP4 code — live bug risk (= VERIFY-1) — ✅ re-verified 2026-06-18 with ERA4 now active: correct (see VERIFY-1)

---

## AIS — AIS / ITU-R M.1371 (A8)

- [x] **AIS-1** ASM (DAC/FID binary on 6/8) dispatch table ◆ big bet — ✅ dispatch + DAC=200 Inland (FID 10/23/24/40, pyais-verified); ❌ remaining: DAC=1 IMO Circ.289 (no pyais oracle), regional DACs
  - [x] **AIS-1.1** DAC=1 IMO SN.1/Circ.289 (FID 31/11 meteo-hydro, 21 weather-from-ship, 16 POB, 22/23 area-notice, 17 VTS, 24/25/26 static/cargo/sensor, 27-30 route/text, 32 tidal) — ✅ FIDs 11/16/17/24/27/28/29/30/31/32 full + 21/22/23/25/26 header-only (spec-derived; no pyais oracle for DAC=1)
  - [x] **AIS-1.2** DAC=200 Inland (FID 10/21/22/23/24/40/55) — ✅ 10/23/24/40 (pyais) + 21 ETA / 22 RTA / 55 persons-on-board (spec-derived: UNECE SC.3/176, gpsd + e-Navigation.nl cross-checked)
  - [x] **AIS-1.3** Regional DACs (235/250/366 AtoN-monitoring, 316/366 Seaway-meteo, 367 US-environmental, 265 STM-route); validate vs pyais oracle — ✅ DAC 235/250 AtoN-monitoring full + regional header-only (DAC/FID/identification) for the rest; ❌ full 316/366/367/265 bodies (pyais has no oracle — skip-don't-fake)
- [x] **AIS-2** Multi-fragment AIVDM reassembly across sentences (long type 5/6/8/26); type-24 Part A+B merge by MMSI; type-5 voyage merge; per-MMSI `AISTracker` state
- [x] **AIS-3** Easy sub-field fills — types 1-3 (ROT/accuracy/timestamp/maneuver/RAIM), type 5 (version/dims/EPFD/ETA/DTE), type 4/11 (accuracy/EPFD/RAIM/UTC), type 18 (accuracy/timestamp/RAIM), type 19 (+dims/EPFD/DTE), type 21 AtoN (accuracy/dims/EPFD/timestamp/off-position/RAIM/virtual/name-ext), type 24B (vendor_id/model/serial/callsign + dims-or-mothership). **All verified against pyais vectors** (1-3 & 5 hand-decoded; 4/18/19/21/24B vs the pyais test-suite vectors). ✅ SOTDMA/ITDMA radio communication-state now decoded too (slot timeout/offset, sub-message by sync-state) — the last sub-field gap is closed.
- [x] **AIS-4** AIS-SART / MOB / EPIRB-AIS distress tagging — `fields::distress_class` (MMSI prefix 970/972/974) tags `distress` in `details`, surfaced in console + dashboard vessel; nav_status 14 (`AIS-SART`) and Msg-14 ACTIVE/TEST text already decode. *(VERIFY-4 resolved: 970/972/974 now mapped; ties XM-6 cross-mode distress overlay)*
- [x] **AIS-5** Outputs — ✅ **NMEA-over-UDP** (`--nmea-udp`, src/outputs/nmea_udp.rs; README claim fixed to "TCP server + UDP push") + ✅ **NMEA tag-blocks** (`--nmea-tag-blocks`, `\s:<station>,c:<ts>*HH\` on both NMEA sinks; `xng_mode_ais::nmea::tag_block`) + ✅ **`channel_letter` fix** (±12.5 kHz A/B bands, `'?'` for non-AIS freqs instead of silently 'A') — *all 2026-06-18*; + ✅ **per-type/MMSI filter + rate downsample + output dedup** (AIS-5h, 2026-06-18: `AisFilter`/`AisGate` pre-bus in `decode_loop`, configured per-session in the station TOML; rate-thins dynamic position types per MMSI + collapses content-duplicate reports; generic enough to reuse for XM-5). + ✅ **AIVDO own-ship encoder** (AIS-5c, 2026-06-18: `own_ship_position(mmsi,lat,lon)` → Type-1 AIVDO sentence; `fields::encode_position_report` inverse of the 1..=3 decode arm, round-trip-verified through the pyais-oracle decoder, not-available kinematics not fabricated). ✅ **periodic emit wired** (2026-06-18: station `own-ship-mmsi` + a session `receiver-pos` → `run_station` injects an AIVDO fix every 30 s onto the bus, clean stop-flag shutdown). ❌ remaining: GPSd output/fusion; NMEA2000/N2K (canboat-GPL/no oracle — skip); HTTP aggregator push (= AIS-Catcher :5599, ties FEED-2.3); JSON_FULL `details`; single-session CLI flags for the AIS-5h filter (station TOML done)
- [x] **AIS-6** Demod — low-CPU Pi mode (1.4× headroom is thin); verify CIC5 droop compensation (VERIFY-4) — ⏸ **DEFERRED:** low-CPU Pi mode is an optimization; CIC droop moot (VERIFY-4: no CIC) — deferred

---

## ADSB — Mode S / ADS-B 1090 MHz (A9)

- [x] **ADSB-1** Modern accuracy/integrity + intent layer — **entirely absent today** ★ quick win (bits already in-message) — *partially done on `feat/per-decoder-airframes-feeding`* — ✅ 1.1–1.5 done
  - [x] **ADSB-1.1** TC31 operational-status decode → `adsb_status` (version, NIC-supp-A, NACp, SIL, SIL-supp, GVA, NICbaro), surfaced in console/dashboard/asf-2.0. *(NACv is a TC19 field — see ADSB-1.5)*
  - [x] **ADSB-1.2** ADS-B version read directly from TC31. *(heuristic version inference for v0 / non-TC31-emitting aircraft deferred)*
  - [x] **ADSB-1.3** TC29 target-state (MCP/FCU selected alt, QNH, selected heading, AP/VNAV/APPROACH/LNAV flags)
  - [x] **ADSB-1.4** TC28 aircraft-status — emergency/priority state (mapped to label, flagged on the map) + ACAS-RA subtype flag. *(full RA decode = ADSB-3.1)*
  - [x] **ADSB-1.5** Accuracy fields — NACp/SIL/NIC-supp-A/GVA (TC31) + NUCp(v0), version-aware NIC (TC+supplement), NACv (TC19), SDA, HRD; folded into adsb_status (pyModeS uncertainty-table verified)
- [x] **ADSB-2** Position/velocity completion — TC5-8 surface movement+track; TC9-18 Q=0 Gillham altitude (routed through the dump1090-verified ladder — fixed a latent −100 ft bug); TC20-22 geometric altitude; VR source + geom-minus-baro; NACv (dump1090/pyModeS verified)
- [x] **ADSB-3** Comm-B BDS register expansion — ✅ 3.1–3.4 done
  - [x] **ADSB-3.1** BDS 3,0 ACAS/TCAS RA (high safety/intel value)
  - [x] **ADSB-3.2** BDS 1,0 data-link-capability, 1,7/1,8/1,9 GICB capability, 2,1 registration markings
  - [x] **ADSB-3.3** BDS 4,4 (wind/temp/pressure/turbulence/humidity), 4,5 (hazard), 5,3 (air-referenced state)
  - [x] **ADSB-3.4** rs1090-style density/penalty BDS scoring (vs binary validate); extend inference beyond 4 registers
- [x] **ADSB-4** DF coverage — DF19 military ES; DF24-27 Comm-D ELM; surface FS/DR/UM (alert/SPI/ground) from DF4/5/20/21
- [x] **ADSB-5** DF18 CF-subtype classification (CF=0 non-transponder, 2/3/5 TIS-B fine/coarse/mgmt, 6 ADS-R) → source tag (VERIFY-7)
- [x] **ADSB-6** Mode A/C decode — decode kernel done (octal squawk / SPI / Gillham ladder, dump1090-oracle-verified); RF framing-pulse demod still deferred
- [x] **ADSB-7** Demod/trust — ✅ graduated position trust (`PosTrust` grade GlobalUnambiguous / LocalContained / LocalReceiver; NIC/NUCp containment + dump1090 half-CPR-zone cap + speed-gate jump reject; surfaced in `adsb_status.position_trust`); ❌ phase-classified per-phase bit templates (the ~3-frame demod gap to readsb)
- [x] **ADSB-8** Outputs — readsb-schema `aircraft.json` (version/nic/nac/sil/gva/emergency/nav_*/acas_ra/wind/oat); ✅ **true RX-clock Beast timestamps → MLAT-feedable** (2026-06-18: `PpmDemod` tracks a drained-samples base → per-frame absolute sample offset → monotonic 12 MHz tick on `SignalQuality.rx_ticks_12mhz` → Beast counter; wall-clock fallback only when absent; benchmark unchanged at 323); ✅ **readsb-schema `aircraft.json`** (2026-06-18, = ECO-4); ✅ **tisb/adsr provenance `type`** (2026-06-18, ADSB-8a: DF18 CF-class → tisb_icao/tisb_other/adsr_icao/adsb_icao_nt/adsb_other, DF17/19 → adsb_icao, bare Mode S → mode_s, no-downgrade precedence; "mlat" deliberately NOT emitted — passive single RX); ✅ **`docs/notes/ADSB.md` Beast-MLAT bullet corrected** to as-built RX-clock state (2026-06-18, ADSB-8b). ADSB-8 outputs now complete.

---

## ECO — Ecosystem / outputs / tooling / dashboard (B)

- [x] **ECO-1** Persistence, history & replay — on-disk message DB + retention + search UI; per-aircraft trace history; BaseStation flight/aircraft DB; dashboard time-scrubber/replay of decoded history; position-density heatmap — ⏸ **DEFERRED:** on-disk message DB + retention + replay + heatmap is a large persistence subsystem — deferred
- [x] **ECO-2** Coverage / range analytics — measured/theoretical range outline (upintheair/HeyWhatsThat/polar), range rings, distance/RSSI columns + per-entity reception — ⏸ **DEFERRED:** coverage/range analytics is dashboard UI + range modeling (needs visual QA) — deferred
- [x] **ECO-3** Alerting & notifications — watchlists (tail/flight/ICAO/MMSI/label/text); special-category highlighting (dbFlags mil/interesting/PIA/LADD + emergency squawk 7500/7600/7700); push (Discord/Telegram/webhook/sound); MQTT Home Assistant autodiscovery — ⏸ **DEFERRED:** watchlist matching is implementable, but the push channels (Discord/Telegram/webhook/MQTT-HA) need external services to verify end-to-end — deferred as a unit (emergency highlighting already done via XM-6)
- [x] **ECO-4** REST / data API — ✅ **`/data/aircraft.json` + `/data/receiver.json`** served off the dashboard port (2026-06-18: readsb field schema — hex/flight/r/t/alt_baro/gs/track/squawk/nac_p/sil/version/lat/lon/seen/seen_pos/messages — merged across ADS-B/UAT/HFDL by ICAO; receiver.json carries version + `receiver-pos`; makes xng a drop-in for tar1090/graphs1090/VRS); ✅ **incremental `?since=<unix>`** (2026-06-18: `aircraft.json?since=` returns only aircraft heard at/after that time, tar1090 poll pattern; `query_since` parser + filter tested); ❌ remaining: WebSocket/SSE/delta stream
- [x] **ECO-5** Export formats — GeoJSON / KML / GPX; `?screenshot` kiosk + shareable deep-link state — ✅ **GeoJSON + GPX + KML 2026-06-18**: `/data/export.geojson` (RFC 7946 FeatureCollection: Point per fix + LineString per trail) + `/data/export.gpx` (GPX 1.1 wpt/trk) + `/data/export.kml` (OGC KML 2.2 Placemark/Point + LineString), shared `geo_entities()` iterator over aircraft/vessels/beacons, coordinate-order asymmetry (GeoJSON/KML [lon,lat] vs GPX lat/lon attrs) tested. ❌ remaining: `?screenshot` kiosk + deep-link hash state (dashboard.html UI)
- [x] **ECO-6** Map/dashboard UX — basemap switcher/overlays/offline MBTiles/weather; persistent trails; map+table filters (alt/speed/type/callsign/source); measure/ruler, gridlines, on-map labels, box-select, tableInView, column sort, dim/contrast/icon-scale controls — ⏸ **DEFERRED:** map/dashboard UX is pure front-end needing visual QA — deferred
- [x] **ECO-7** Statistics & graphs — built-in graphs page (msgs/min, per-freq, level histogram, CRC/error %, CPU/temp/uptime); expand Prometheus (per-mode/type counts, entity gauges, decode latency, FEC-corrected, reassembly, dropped/lagged-bus); first-party Grafana dashboard JSON — ✅ FEC-corrected counter + per-(mode,freq) frame/crc/level + per-label ACARS counters done; built-in graphs page + Grafana JSON = UI deferred
- [x] **ECO-8** Health / watchdog / operability — health state machine + no-data alarm + auto-restart; continuous autogain during `listen` (VERIFY-7); web config/management UI — ⏸ **DEFERRED:** health/watchdog + web config UI is a large operability subsystem; continuous autogain captured under VERIFY-7 — deferred
- [x] **ECO-9** Multi-receiver aggregation & integrations — turn `xng ingest` into a fan-in dashboard/dedup hub (ties XM-5); SignalK delta; NMEA2000/N2K; NMEA tag-blocks + community-map push; GPSd input — ⏸ **DEFERRED:** multi-RX aggregation hub = XM-4/XM-5 over the asf-2.0 fan-in + external integrations (SignalK / NMEA2000 canboat-GPL / GPSd) — deferred
- [x] **ECO-10** Enrichment data — bundled/auto-updating aircraft DB + dbFlags; route/airline/operator/airport enrichment on dashboard; persistent vessel registry across restarts — ⏸ **DEFERRED:** enrichment needs a bundled/auto-updating aircraft+vessel DB (data dependency); partial dbinfo (country/db lookup) exists — deferred
- [x] **ECO-11** Smaller dashboard/quality — per-mode entity expiry (not flat 300 s); message-stream scrollback backed by ring file; dark/light/scale controls; verify non-Iridium trail antimeridian wrapping (VERIFY-7) — ⏸ **DEFERRED:** per-mode expiry is design-ambiguous for multi-source entities; scrollback-ring + theme controls are UI — deferred (flat 300 s works; antimeridian wrap done in VERIFY-7)
- [x] **ECO-12** Aggregator-network targets — feed/dedup against airplanes.live / adsb.lol (ODbL) / adsb.fi / OpenSky; study BelugaProject (air+sea fusion prior art) — ⏸ **DEFERRED:** aggregator-network feeds need each network's endpoint/protocol + accounts (external) — deferred

---

## NEW — New modes / capabilities (D)

> Priority tiers from the consolidated viability matrix. `NEW-SKIP` items are **considered and declined** — kept so they aren't re-proposed.

- [x] **NEW-P0-1** UAT 978 MHz — **crate `xng-mode-uat`**: ADS-B downlink (state vector) + FIS-B uplink (APDU framing + DLAC text products), oracle-verified. *(IQ demod + bin/Mode wiring = follow-up)*
  - [x] **NEW-P0-1.1** UAT ADS-B short/long messages (US GA positions) — ✅ UAT ADS-B short/long state vectors decode in `xng-mode-uat` (oracle-verified)
  - [x] **NEW-P0-1.2** FIS-B weather products (NEXRAD/METAR/TAF/PIREP/AIRMET-SIGMET/Winds-Temps/NOTAM/TFR/SUA) — *audited 2026-06-18: the DLAC text products (20-27, 411-413) + APDU frame headers are done and dump978-verified; the non-DLAC text products (0-13) and graphical products (51-102) are **blocked on a missing oracle** — dump978's `uat2text` prints 0-13 raw (no frame decode) and no public reference decodes the graphical rasters. Skip-don't-fake until a fixture exists.*
  - [x] **NEW-P0-1.3** TIS-B + ADS-R (`uat2esnt` → DF18 CF=6 integration path) — ✅ **2026-06-18**: `synth::EsSource` (Adsb→DF17 CA=5, TisB→DF18 CF=2, AdsR→DF18 CF=6); `AircraftFix.source` from the UAT `address_qualifier` (tisb_*/adsr_other); Beast routes accordingly so UAT 978 TIS-B/ADS-R keep their class on 1090. Round-trip-verified through the oracle-validated `df18_cf_class` (+ CPR still decodes); Beast unwrap asserts DF18 CF=6 for ADS-R. DF17 native path unchanged.
- [x] **NEW-P0-2** COSPAS-SARSAT 406 — **crate `xng-mode-sarsat`**: FGB (T.001) message + BCH decode, amsa-code-verified. *(SGB T.018 + IQ demod = follow-up — audited 2026-06-18: SGB (2nd-gen, 250-bit OQPSK+DSSS, distinct layout/FEC) is **blocked on a missing oracle**: no vendored SGB vectors, no `amsa-code/sgb-decoder` compliance kit, no published T.018 worked example. Skip-don't-fake until a reference message exists.)*
- [x] **NEW-P0-3** DSC — **crate `xng-mode-dsc`**: ITU-R M.493 symbol + message decode. *(IQ demod = follow-up)*
- [x] **NEW-P1-1** Radiosondes — **crate `xng-mode-sonde`**: RS41 RS-FEC + frame (STATUS/GPS/PTU) decode, real-off-air verified (119/119 vs rs1729 `rs41mod`). *(GFSK demod + RS92/DFM/M10/… = follow-up — audited 2026-06-18: additional sonde types **blocked on a missing oracle** — no vendored capture or published worked-example frames for RS92/DFM/M10 in-crate; skip-don't-fake until one exists)*
- [x] **NEW-P1-2** AIS-SART / MOB / EPIRB-AIS — done under **AIS-4** (cross-reference)
- [x] **NEW-P1-3** EOT / HOT / DPU rail telemetry — **crate `xng-mode-eot`**: Manchester-FSK + AAR S-9152 frame (unit address, brake-pipe pressure, motion, marker light + battery, turbine/valve, BCH); direction (eot/hot) by RX freq (457.9375 / 452.9375 MHz). Runtime-wired to `--mode eot`. *(reverse-engineered field semantics per cited open decoders; demod synthetic AWGN-BER — no public IQ; train-tail map plotting = follow-up)*
- [x] **NEW-P1-4** NAVTEX — **crate `xng-mode-navtex`**: CCIR 476 + FEC-B + ZCZC message decode. *(IQ demod = follow-up)*
- [x] **NEW-P2-1** ADS-L — **crate `xng-mode-adsl`** (the "ADS-K" item): EASA SRD860 message decode. *(FANET/OGNTP + IQ demod = follow-up)*
- [x] **NEW-P2-2** APRS / AX.25 (incl. HAB balloons) — **crate `xng-mode-aprs`**: AFSK1200 (Bell 202) over FM + AX.25 v2.2 UI (callsign/SSID/digipeaters, X.25 FCS) + APRS payload — uncompressed + Base-91 compressed position (incl. course/speed/alt), **Mic-E**, weather, message, status, object, **item, bulletin/announcement, general-query, PHG/DFS/RNG data-extensions, Maidenhead grid**, **telemetry (T# data values + PARM/UNIT/EQNS/BITS definition messages, APRS 1.0.1 ch.13, spec-worked-example verified)**. Runtime-wired to `--mode aprs` (144.39 NA / 144.8 EU / 432.5 UK). *(igate feed = follow-up; demod synthetic AWGN-BER — real-RF unconfirmed: 144.39 quiet at the KSMF soak site, decode pipeline verified by spec)*
- [x] **NEW-P2-3** POCSAG / FLEX / FLEX-NEXT paging — ✅ **POCSAG** (**crate `xng-mode-pocsag`**: 2-FSK 512/1200/2400 multi-baud auto-detect + CCIR Code No.1 / ITU-R M.584-2, BCH(31,21,2), numeric/alpha/tone) + ✅ **FLEX** (**crate `xng-mode-flex`**: 1600 bps 2-FSK **+ 4-level 3200/6400**, auto rate-detect from the Sync-1 A-code, FLEX sync/FIW/BIW, BCH(31,21), alpha/numeric/tone, off-air garbage rejection + alpha header strip), both runtime-wired (`--mode pocsag` / `--mode flex`, FLEX opened `baud=0`=auto). FLEX **real off-air validated** (929 MHz 6400 4-level US paging: recovers clean alpha pages); POCSAG demod synthetic AWGN-BER. ❌ FLEX-NEXT still open (per-session baud knob moot — rate auto-detected)
- [x] **NEW-P2-4** ATCS — **crate `xng-mode-atcs`**: HDLC/LAPB framer + Spec-200 address/header decode. *(Genisys/ARES payload + IQ demod = follow-up — audited 2026-06-18: the codeline payload protocols (Genisys/ARES/SCS-128) are **proprietary AAR systems with no public spec or OSS reference**; sigidwiki shows message-type tags but no payload format. Blocked — declined per skip-don't-fake; raw `user_data` preserved.)*
- [x] **NEW-P2-5** VDES / long-range AIS extensions — ✅ **ASM** via **crate `xng-mode-vdes`**: GMSK 9600 + HDLC/CRC-16 transport on the ASM 1/2 channels (former AIS 27/28), AIS Msg 6/8 ASM headers (source/dest MMSI + DAC/FID), and **7 spec-cited DAC/FID application payloads** — DAC=1 FID 11 (met/hydro IMO236), 16 (POB), 17 (VTS synthetic target), 18 (clearance-time), 31 (deep IMO289 weather block); DAC=200 FID 10 (Inland static&voyage), 55 (persons-on-board) — + raw `data_hex` fallback. pyais + gpsd spec verified. Runtime-wired to `--mode vdes` (161.950/162.000 MHz). *(demod synthetic AWGN-BER; sparse public VDES spec)*; ❌ variable-length/repeated-block ASM bodies w/o a hand-verifiable vector (FID 14/20/22/23/25, regional DACs), VDE-SAT/terrestrial data channels, full AIS-2.0 still open
- [x] **NEW-P3** Parking lot (low priority / dependent) — FLARM (after open OGN), LoRa-APRS/Horus HAB (radiosonde rider), DMR-LRRP / P25-Unit-GPS / TETRA-SDS-LIP position PDUs (metadata only), TPMS (rtl_433 subset), Orbcomm STX (breadth flex), GTFS-realtime ingest (non-RF complement), WSPR/FT8 (HF-prop health niche) — ⏸ **DEFERRED:** parking lot — explicitly low-priority / dependent future modes
- [x] **NEW-SKIP** Considered & declined (documented in D) — VDL Mode 4 (regional/near-dead), DSRC/C-V2X (wrong band, sunsetting), toll/MDT, AEI/Eurobalise (passive/near-field), PTC/GSM-R/FRMCS (encrypted), weather imagery NOAA/Meteor/GOES (SatDump incumbent), cubesat/SatNOGS, Globalstar/Starlink/rocket (proprietary/encrypted), GNSS/SBAS (no entities), Drone Remote ID (off-axis; only via ADS-L Issue-2), WEFAX/Pactor/Winlink, wM-Bus, ERMES — ⏸ **DEFERRED:** considered & declined — terminal by definition (rationale in COMPARISON_RESEARCH §D)
- [x] **NEW-V** Verify before committing — (a) which airports/airlines still run POCSAG/FLEX ground-ops paging; (b) AeroMACS/Gatelink demand; (c) whether xng L-band front-ends pass 406 MHz for SARSAT reuse; (d) whether generic-ISM (TPMS) scope is desired (= VERIFY-11) — ⏸ **DEFERRED:** external market/hardware research questions (=VERIFY-11) — not code

---

## VERIFY — Research / correctness checks (Appendix + inline ❓)

> Mostly "read the code / confirm against a source" tasks; resolve these to confirm or retire the matching items above. `⚑` = possible live bug.

- [x] **VERIFY-1** ⚑ Iridium time re-epoch correctness in satellite-naming/SGP4/TLE code (= IRID-8) — ✅ **resolved 2026-06-18 (live)**: `iri_time_unix_at()` (`ira.rs`) selects the newest era-epoch whose base ≤ reference time; all four era bases independently checked vs MetOcean bulletin instants (ERA2 2014-05-11, ERA3 2025-02-14, **ERA4 2026-01-14T18:08Z — active today**), boundary inclusive of the newer era (no off-by-one), ERA2 leap-seconds applied only in-range. Post-2026-01-14 frames decode to 2026, not 2014. 4 unit tests pin it; demod bit/field-exact oracles guard no regression.
- [x] **VERIFY-2** ACARS — raw MIN / 4th-char downlink-rule edge cases; any media-advisory v1+ exists? — ✅ **resolved 2026-06-18**: raw 4-char MIN surfaces on every carrier (`AcarsCore.msg_num`); split MIN (3-char + 4th-seq + reassembly index) on the byte path (`min.rs::split_downlink`, consumed by HFDL/Aero); 4th-char rule correct (`A`-`Z`→seq, digit/punct→None, used in reassembly skip); media-advisory `SA` is v0-only by libacars-faithful design and MIAM (ARINC 841) already covers v1/v2 CORE PDUs + file transfer. *(low-pri follow-up: the native VHF/POA path doesn't yet forward the **split** MIN to the serialized `AcarsCore` — needs a shared field on `AcarsCore`; raw MIN is already there. Per-label Prometheus counters → VERIFY-9.)*
- [x] **VERIFY-3** VDL2 — ✅ resolved: SABME already folded into the U-frame arm (not `U?`); ES-IS option-TLVs present; plain/unprotected CPDLC added (VDL2-1.5); the v2.5.1 249-octet-multiple block-length bug does NOT affect xng (RS row count = `div_ceil(tl_bits/1992)`, exact at the 1992-bit boundary — regression-guarded). *(StatsD `good_loud`/`pp_acars` are output-side, tracked under VDL2-8)*
- [x] **VERIFY-4** AIS — CIC5 droop compensation in xng-dsp; mid-frame polarity-flip recovery — ✅ **resolved 2026-06-18**: (a) **moot** — there is no CIC anywhere in the DSP (repo-wide grep: zero CIC/comb/integrator hits); the AIS front end uses **FIR flat-passband** decimation, so droop compensation is unnecessary. (b) polarity/NRZI-ambiguity recovery is **already present** — NRZI decodes on level-change (inherently polarity-invariant) + an explicit π-flip trellis seed + decision-directed phase tracking. *(MMSI 970/972/974 mapping: RESOLVED — AIS-4)*
- [x] **VERIFY-5** HFDL — resolved: as-built is a 7-tap **symbol-spaced** LMS equalizer; dumphfdl's 15 is **T/2 (half-symbol)-spaced** — both correct, different spacing convention (code comments fixed)
- [x] **VERIFY-6** STD-C — resolved: `0xB0` vs `0xB1`+`0xB2` distinction surfaced in details; mid-frame polarity-flip recovery added (STDC-8)
- [x] **VERIFY-7** Ecosystem — ✅ **resolved 2026-06-18**: (a) receiver-position **pin** — was missing, now **implemented** (`session_descriptor` emits `receiver_pos`; dashboard renders a 📡 station marker; verified live at 38.5125,-121.4925); (b) non-Iridium **antimeridian trail wrapping** — was missing (only Iridium sat trails unwrapped), now **implemented** (`upsert` applies `unwrapTrail` to aircraft/vessel/beacon trails); (c) DF18 CF-subtype (= ADSB-5) confirmed done (`df18_cf_class`, DO-260B). ❌ remaining: **continuous autogain during `listen`** (only `survey --tune-gain` exists today) — a larger feature, deferred.
- [x] **VERIFY-8** Aero-C — resolved: `AEROTypeP/R` enumerator hex verified against the JAERO source (AERO-6)
- [x] **VERIFY-9** ACARS — acarsdec `mqttout.c` in current f00b4r0 4.x tree (post-SoapySDR refactor)?; xng per-label ACARS counters? — ✅ **per-label counters DONE 2026-06-18**: `LiveState.acars_labels` (freq,label→count) map; `decode_loop` tallies each published ACARS message; `metrics.rs` emits `xng_acars_messages_total{mode,freq,label}` (escaped label values). En route, fixed a latent station-mode bug: `run_station` served `/metrics` off a fresh `LiveState` the decode loops never updated (frame/CRC counters always 0) — now one shared `LiveState`, per-channel stats freq-keyed so multi-session stations don't clobber rows. ✅ MQTT output sink confirmed present (`src/outputs/mqtt.rs`, = ACARS-5.2). *(mqttout f00b4r0-4.x source question = external research, immaterial — xng's MQTT output is independent.)*
- [x] **VERIFY-10** ADS-B — does `xng-mode-adsb` already emit Mode A/C? (= ADSB-6) — ✅ **resolved 2026-06-18**: it does **not** emit Mode A/C — the decode **kernel** exists (`mode_ac.rs`: octal squawk / SPI / Gillham ladder, dump1090-verified) but is **unwired** (no framing-pulse demod), exactly as ADSB-6/PROVENANCE state. The README does not overclaim (its "altitude replies" = Mode S DF4/20, not Mode A/C). No action — remaining work is the RF framing-pulse demod (ADSB-6, deferred).
- [x] **VERIFY-11** New-mode commitments — POCSAG/FLEX airfield usage; AeroMACS/Gatelink demand; 406 MHz front-end reuse; generic-ISM scope desire (= NEW-V) — ⏸ **DEFERRED:** external market/hardware research questions — not code
- [x] **VERIFY-12** Beast MLAT counter usability — ✅ **resolved 2026-06-18**: confirmed the blocker was the wall-clock-derived (jittery, non-monotonic) Beast timestamps, not GPS absence — then **fixed it**: `PpmDemod` now tracks a drained-samples base, so each frame carries its absolute stream sample offset, converted to a monotonic consistent-rate 12 MHz tick and stamped on the Beast counter (`rx_ticks_12mhz`, wall-clock fallback if absent). The feed is now well-formed for an MLAT client to fit the RTL clock drift. Passive readout — frame counts unchanged (benchmark adsb_modes1 = 323). New Beast unit test. (= ADSB-8 Beast-timestamp item.)
- [x] **VERIFY-13** Feed — confirm VDL2 ingest format preference (dumpvdl2 `decoded:json` vs vdlm2dec); whether Airframes exposes public IMSL/IRDM/STD-C ports + the settled Iridium feeding mechanism (= FEED-4) — ✅ VDL2 format confirmed = dumpvdl2 decoded:json (FEED-2.1); IMSL/IRDM/STD-C ports + Iridium mechanism remain external (FEED-4.2)

---

# PHASE 2 — next-phase backlog (researched 2026-06-19)

> Added after the v0.21.0 release. Sources for this phase: a 15-angle gap-analysis
> sweep (per-mode-vs-oracle + outputs/ecosystem + benchmark methodology), a web
> survey of the reference decoders' current feature sets + emerging protocols, and
> two real-RF captures contributed by **Opflasher** (Airframes Discord). Same rules
> as Phase 1: clean-room provenance, **external-oracle** verification (never
> self-consistency), **skip-don't-fake**, never reduce a benchmark. IDs continue
> each category's numbering (stable, never renumbered); `BENCH-*` is a new category.
>
> **Theme of the phase: accuracy & compatibility first.** Phase 1 reached broad
> mode coverage (8→20 modes) but most demods are benchmarked only synthetically or
> field-exact, and several outputs drift from the readsb/acarsdec/AIS-catcher wire
> formats. Phase 2 closes those gaps and stands up real-RF + sensitivity gates.
>
> **Opflasher capture status (2026-06-19, characterized in-session):**
> - `discord-opflasher-acars1.cf32` — **cracked**: complex-float32, **3.0 MS/s**,
>   single active ACARS channel ≈ −50 kHz from capture center, real **Korean Air**
>   traffic (HL8537 / KE0402, YSSY→RKSI, 17JUN26, H1 `#CFB`/`#DFB` maintenance).
>   xng decodes **15 CRC-OK** over the full 120 s file (sparse — one aircraft).
>   Authentic and CI-vendorable → unblocks **ACARS-4.3** (see `BENCH-1`).
> - `discord-opflasher-vdl1.cf32` — complex-float32, 360M samples; **params not yet
>   pinned**: does NOT decode at 3.0 MS/s across the full 25 kHz VDL2 raster (either
>   I/Q sense, scaled or not). Either a different sample rate/center or too weak.
>   **Cheapest unblock: ask Opflasher for the exact rate + center** (see `BENCH-2`).

## BENCH — Benchmark coverage & real-RF captures (highest leverage)

- [x] **BENCH-1** ★ Vendor the Opflasher ACARS capture as the first **real off-air ACARS** CI fixture + acarsdec head-to-head (unblocks **ACARS-4.3**) — ✅ **2026-06-19**: downconverted the single POA channel to baseband + decimated 3.0 MS/s → 24 kS/s, vendored `bench/data/acars_24k.cs16` (release asset, gitignored). Head-to-head on the same signal: **xng 13 CRC-OK vs acarsdec 3.7 9** (real Korean Air HL8537/KE0402; sublabels C36I–M/D57A–C match field-for-field) — xng leads, no sensitivity gap. `acars_offair` floor 10 added to `baselines.json` + a CRC-OK `count_crc` check in `bench/run.sh`; row + section in `BENCHMARKS.md`; ACARS.md + REFERENCES.md updated. (resolves the deferred **ACARS-4.3**)
- [ ] **BENCH-2** Pin the Opflasher **VDL2** capture parameters → second real-RF VDL2 benchmark vs dumpvdl2. First ask the contributor for sample-rate + center (it did not decode at the ACARS 3.0 MS/s); if a richer offline sweep is preferred, sweep rate × center × I/Q-sense over the full file. Then add a `vdl2_offair2` row + gate and confirm the ~98% dumpvdl2 parity generalizes across a second antenna/path. *Oracle: dumpvdl2 2.6.0.*
- [ ] **BENCH-3** ◆ Synthetic-AWGN **BER-floor CI gates** for the modes that have no real-RF count gate today (STD-C, Aero, Iridium-IDA, and the synthetic-only new modes). Formalize the existing `matched_filter_recovers_at_lower_snr` (STD-C) and `coherent_beats_discriminator_ber_vs_snr` (Aero) harnesses into **required** floors (e.g. "≥95% frame recovery at SNR=X dB") and wire them into `bench/run.sh` via `cargo test`. Catches demod-sensitivity regressions without a vendorable capture. *Oracle: internal modulate→AWGN→demod (an allowed synthetic oracle).*
- [ ] **BENCH-4** ★ Publish a unified benchmark methodology + 1-page **gap matrix** in `BENCHMARKS.md`: per mode, which of {real-RF count gate · synthetic BER floor · field-exact oracle test} exists, the per-mode sensitivity target, and how to benchmark a new mode. Makes "what is actually verified" auditable (today: ADS-B/VDL2/HFDL/AIS/RS41/NAVTEX/UAT have count gates; STD-C/Aero/Iridium are field-exact only; ACARS had none until BENCH-1).
- [ ] **BENCH-5** Capture-sourcing campaign for the capture-gated deferrals (consolidates many `needs-capture` items). Targets + leads found in research: **traffic-bearing HFDL** with HFNPDU positions + LPDU ACARS (KiwiSDR `--kiwi-wav` GNSS-timestamped recording; sigidwiki skip.land 2024-11-05 21931 kHz) → unblocks `FEED-2.2`; **Inmarsat Aero-C 10.5k**; **ACARS media-advisory / MIAM**; **radiosonde RS92/DFM/M10/M20** (radiosonde_auto_rx perf samples); real off-air for **POCSAG/DSC/EOT/ADS-L**. Vendor each as it arrives; each unblocks its mode's deferred decode/feed verification.

## ACARS / VDL2 / HFDL / AIS / ADS-B — accuracy & compatibility (verify-then-fill)

> Several Phase-1 items are marked done at the decode layer but the **serializer /
> wire-format** path may stop short. Each item below is "audit the emitted output
> against the live oracle, then fill any gap" — phrased so we confirm before claiming.

- [ ] **ACARS-6** ★ ⚑ acarsdec-JSON field-parity audit vs **acarsdec 4.x** output: confirm `sublabel`, `mfi`, nested `app`/libacars envelope, the 4-char **MIN** (`msg_num`), and `assstat` are all *emitted* on the `:5550` feed (not just carried internally), since aggregators (acars_router, Airframes) key dedup/reassembly on MIN. Fill any field the serializer drops. *Oracle: acarsdec 4.x JSON; acars_router dedup rules.* (extends `ACARS-5.1`/`FEED-2.4`)
- [ ] **ADSB-9** ★ `aircraft.json` + `receiver.json` schema completeness vs **readsb/dump1090-fa** (drop-in for tar1090/graphs1090/VRS): add the missing precision fields — **GVA, NIC-baro, NACv, SDA**, emitter `category`, `modeac_count` — and receiver fields **uuid** (stable across restarts), **max_range**, **mil**. *Oracle: readsb `aircraft.json`/`receiver.json` schema; pyModeS DO-260B tables.* (extends `ECO-4`/`ADSB-1`)
- [ ] **ADSB-10** ⚑ Verify the version-aware accuracy fields (TC31 version/NIC/NACp/SIL/GVA, TC5-8 surface, TC29 target-state, DF18 CF source) are actually **serialized into the message JSON + Beast/SBS + feeds**, not only decoded internally — several finders suspected the serializer stops short. (= `VERIFY-16`; ties `ADSB-1`/`ADSB-5`)
- [ ] **HFDL-7** ★ `--freq-as-squawk` option — **confirmed a real dumphfdl feature** (conveys the HFDL channel freq in the Basestation squawk field); xng lists it not-done. Plus the **AC-ID→ICAO logon cache** wired into the position/feed path so HFDL aircraft carry a real hex for Airframes/SBS identity. *Oracle: dumphfdl `--freq-as-squawk`; `src/ac_cache.c` (facts).* (extends `HFDL-3`/`HFDL-4`)
- [ ] **AIS-7** Align AIS JSON to **ITU-R M.1371-6** (2026) and match AIS-catcher's recent additions: expose the Msg **25/26** addressed/broadcast ASM envelope + recognised (DAC,FID) bodies, expand Msg **28**, and apply the M.1371-6 field renames (bits previously labelled regional/reserved). Deepen the ASM dispatch (e.g. DAC 366 FID 10 IALA AtoN monitor). *Oracle: AIS-catcher (M.1371-6-aligned JSON) + pyais.* (extends `AIS-1`/`AIS-5`)
- [ ] **VDL2-9** Emit **non-ACARS AVLC/XID** frames as dumpvdl2 `decoded:json` (dumpvdl2 2.6.0 now emits full JSON for *all* protocols/message types; FEED-2.1 covers only ACARS-over-AVLC today) + the **TCP :5553** feed variant. *Oracle: dumpvdl2 2.6.0 JSON.* (extends `FEED-2.1`/`VDL2-8`)
- [ ] **AERO-10** Wire **Aero positions → SBS/Beast/map** now that the XM-2.2 `AircraftFix` adapter exists (Aero ADS-C/position SUs decode; the cross-mode entity dependency that blocked `AERO-7` is satisfied). (extends `AERO-7`, ties `XM-2.2`)

## ECO — outputs & ecosystem compatibility

- [ ] **ECO-13** WebSocket/SSE **delta stream** for `aircraft.json` (new/moved/removed) — tar1090/readsb-native live subscription, lower latency + bandwidth than the current 1 Hz poll; the aggregator-facing complement to `ECO-4`.
- [ ] **ECO-14** First-party **Grafana dashboard JSON** + **acarshub-compatible** Prometheus families (e.g. a `good_loud`/high-SNR counter, per-mode noise-floor + rssi gauges) so xng drops into existing acarshub/Grafana monitoring without schema surgery. (extends `ECO-7`)
- [ ] **ECO-15** **I/Q-on-stdin** input path (read cf32/cs16 from a pipe) — dumphfdl added this for GNURadio/KiwiSDR interop; lets xng decode KiwiSDR `--kiwi-wav`/GNURadio streams and feeds the BENCH-5 capture work. *Oracle: dumphfdl stdin I/Q + `--read-buffer-size`.*

## VERIFY — Phase-2 correctness audits

- [ ] **VERIFY-14** ⚑ Confirm **version-aware NIC is NOT fed into the position-trust containment gate** (a finder flagged a possible double-use of NIC in `position_quality`); audit the compute path and add a regression test.
- [ ] **VERIFY-15** ⚑ Iridium **post-ERA4 live decode audit** — with ERA4 (2026-01-14) months-active in production, spot-check IRA/IBC frames for satellite-naming/SGP4/TLE-freshness edge cases (frames decoding to the wrong year, stale TLE). (re-runs the `IRID-8`/`VERIFY-1` check against live traffic)
- [ ] **VERIFY-16** ⚑ Serializer-reaches-the-wire audit (= `ADSB-10`, generalized): for ADS-B, AIS, VDL2 confirm that every decoded field a finder flagged (TC31/TC5-8/TC29, DF18-CF source, AIS ASM envelope) actually appears in the JSON/Beast/SBS/feed output, with a test per claim. Cheap, high-confidence.

## Big bets (P2 — large, high-leverage)

- [ ] **VDL2-10** ◆ Table/codegen-driven **unaligned-PER ASN.1 core** for the ATN stack — the single highest-leverage VDL2 bet: closes the remaining CPDLC argument shapes, CHOICE-extension/fragmentation completeness, the AARQ/AARE ACSE bodies, and native **ATN-B2 ADS-C** (`VDL2-2.3`) all at once, replacing the hand-written UPER walkers. *Oracle: dumpvdl2's asn1c-generated decoders; ISO PER/ACSE/Session/ADS-C modules. Needs captured PDUs (Opflasher VDL2 capture once pinned, or community samples).* (unifies `VDL2-1`/`VDL2-2`/`VDL2-1.1`/`VDL2-1.3`)
- [ ] **STDC-9** ◆ Demodulate the **LES message channel** (follow the `0x83` logical-channel-assignment) — the biggest functional gap vs tekmanoid; `STDC-5` re-stated as a Phase-2 bet now that the descriptor decode (STDC-2/3) is in place. *Needs an Inmarsat-C message-channel capture.*
- [ ] **ADSB-11** ◆ Mode A/C **RF framing-pulse demod** — the decode kernel (octal squawk / SPI / Gillham) is done and dump1090-verified but unwired (`ADSB-6`/`VERIFY-10`); add the pulse-pair acquisition front end. *Oracle: dump1090-fa Mode A/C path.*
- [ ] **ADSB-12** ◆ Phase-classified **per-phase bit templates** in the 1090 demod — the residual ~3-frame gap to readsb on dense captures (`ADSB-7`). *Oracle: readsb demod; needs a genuinely weak/dense capture to move the count.*
- [ ] **STDC-10** EGC/area output polish: render rectangular/circular/NAVAREA areas on the dashboard map + full LES/NCS operator-name table + per-frame SNR/noise in `SignalQuality` (envelope-mode noise-floor, ties `XM-1`). (extends `STDC-1`/`STDC-4`/`STDC-8`)

## NEW — new protocols & watch list

- [ ] **NEW-P4-1** COSPAS-SARSAT **SGB (2nd-gen, C/S T.018)** *message* decode — **oracle now exists**: `amsa-code/sgb-decoder` (Java, T.018 Rev.9) + a Python SGB codec, so the 250-bit message/BCH layer is now verifiable (it was deferred "no oracle"). The OQPSK+DSSS *demod* still needs a real SGB capture. Land the message layer against the oracle now; gate the demod on a capture. (unblocks the message half of `NEW-P0-2`) *Oracle: amsa-code/sgb-decoder.*
- [ ] **NEW-P4-2** VDES **VDE-SAT** (satellite ASM) + **VDE-TER** (π/4-QPSK / 8-PSK / 16-APSK terrestrial data) — the forward-looking AIS-2.0/VDES rollout beyond the ASM channels xng already does (`NEW-P2-5`). ◆ big bet; sparse public spec — watch + prototype as samples appear.
- [ ] **NEW-P4-3** **LDACS** (L-band Digital Aeronautical Communications System) — **roadmap-watch**: ICAO SARPS 2022, compatibility testing through end-2025, only GNU Radio research code exists (no production decoder). No action yet; track for when an open PHY decoder/spec matures. (companion to the `AERO-9` SB-S watch)
- [ ] **NEW-P4-4** **DO-260C / ADS-B v3** field expansions — **watch**: no evidence readsb/dump1090-fa implement v3 yet; pre-stage the operational-status/accuracy field additions so xng is ready when traffic appears. (extends `ADSB-1`)
- [ ] **NEW-SKIP-2** Re-confirmed declines (landscape rechecked 2026-06): **Drone Remote ID** broadcast is BLE/WiFi transport (out of xng's SDR/airband scope) *except* where carried over **ADS-L Issue 2** at 868 MHz — keep that under `NEW-P2-1`, not a separate mode. Open Drone ID parsers (OpenDroneID, open-remote-id-parser) are reference only.
