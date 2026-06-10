# References

Every external source used while building xng, by area. Per-crate
PROVENANCE.md files record what each decode core took from where; this is
the master list for later referral.

Sourcing policy: protocol *facts* may come from any source including
existing decoder source code (facts are not copyrightable); code/text is
ported only from permissively licensed projects (MIT/BSD), with
attribution. GPL projects are listed as fact references only.

## Specifications and standards

| Document | Used for | Access |
|---|---|---|
| ICAO Annex 10 Vol III Part I, Ch. 6 | VDL2 PHY: D8PSK Gray map (Table 6-1), burst/training structure, header FEC H matrix (Table 6-2), scrambler (Fig 6-2), RS(255,249) + interleaver | https://ffac.ch/wp-content/uploads/2020/09/ICAO-Annex-10-Aeronautical-Telecommunications-Vol-III-Communication-Systems.pdf |
| ICAO Annex 10 Vol IV | Mode S PPM, preamble, CRC-24, DF formats | summarized via open references (1090 Riddle) |
| ARINC 618-6 (Air/Ground Character-Oriented Protocol) | ACARS framing: differential MSK (§4.4.2), odd parity LSB-first (§4.4.2.1), preamble (§4.2-4.3), block format (§2.1-2.3), BCS CRC + "K7" worked example (§2.2.10) | public copy: https://pdfcoffee.com/324981622-618-6-airground-character-oriented-protocol-specification-pdfpdf-pdf-free.html |
| ETSI EN 301 841-1 V1.4.1 | VDL2 radio conformance (mirrors Annex 10) | https://www.etsi.org/deliver/etsi_en/301800_301899/30184101/01.04.01_60/en_30184101v010401p.pdf |
| ETSI EN 301 841-2 V1.2.1 | AVLC link layer: frame structure (Table 5.1a), addresses (Table 5.2/5.3), control field (Table 5.5) | https://www.etsi.org/deliver/etsi_en/301800_301899/30184102/01.02.01_60/en_30184102v010201p.pdf |
| ITU-R M.1371-5 | AIS: GMSK/NRZI, HDLC framing, training sequence, message bit ordering | freely published by ITU |
| ISO/IEC 13239 | HDLC framing conventions (flags, stuffing, FCS) referenced by AVLC and AIS | via EN 301 841-2 citations |
| ISO/IEC TR 9577 | AVLC payload protocol identification (0xFF ACARS escape, 0x81 CLNP, 0x82 ES-IS, 0x83 IDRP) | via Wiley excerpt + GE patent below |
| ARINC 622 / 745 (via libacars) | ATS envelope, ADS-C field layouts | via libacars source (MIT) |
| reveng CRC catalogue | CRC-16/KERMIT identification for ACARS BCS | https://reveng.sourceforge.io/crc-catalogue/16.htm |

## Source code used as reference or ported

| Project | License | How used | URL |
|---|---|---|---|
| libacars (szpajder) | MIT | **Ported** (attributed): ARINC 622 envelope, ADS-C decoder, media advisory, sublabel/MFI rules; 4 real ADS-C test vectors from examples/adsc_get_position.c | https://github.com/szpajder/libacars |
| JAERO (jontio) | MIT | **Ported** (attributed): P-channel framing, interleaver, scrambler, SU/ISU/SSU layer, ACARS carriage; 10.5k OQPSK demod ported from oqpskdemodulator.cpp + coarsefreqestimate.cpp (square-law timing, tanh cross-product carrier loop, squared-signal two-tone coarse CFO); 600/1200 MSK demod intentionally diverges (see xng-mode-aero/PROVENANCE.md). Off-air samples **used for validation** (600bps: 11 ACARS decoded; 10.5k: 144 ACARS decoded; 12 s vendored as CI fixture with attribution) | https://github.com/jontio/JAERO |
| iridium-toolkit (muccc) | BSD-2 | Planned port source for Iridium frame parsing (wave 2) | https://github.com/muccc/iridium-toolkit |
| acars crate (xoolive) | MIT | Inspected for API/coverage comparison; test fixtures noted as borrowable | https://crates.io/crates/acars |
| ship162 (xoolive) | MIT | Noted as AIS reference (our core ended up independent) | https://github.com/xoolive/ship162 |
| rs1090/jet1090 (xoolive) | MIT | Architectural comparison; candidate dep for deep Mode S decode | https://github.com/xoolive/jet1090 |
| dumpvdl2 (szpajder) | GPL-3 | **Facts only** (not read for xng so far; clean-room held for VDL2) | https://github.com/szpajder/dumpvdl2 |
| dumphfdl (szpajder) | GPL-3 | **Facts only**: src/systable.c + src/hfnpdu.c read for the system-table wire layout; src/hfdl.c read for A/M/T sequence values and framer thresholds; the compiled binary used as ground truth for off-air validation (sigidwiki 21931 kHz capture) | https://github.com/szpajder/dumphfdl |
| sigidwiki HFDL sample (skip.land) | CC BY-SA | Off-air 21931 kHz IQ capture used for HFDL validation; 8 s vendored as CI fixture with attribution | https://www.sigidwiki.com/wiki/High_Frequency_Data_Link_(HFDL) |
| sigidwiki VDL-M2 sample | CC BY-SA | Off-air VDL2 IQ capture (Amsterdam area, inverted I/Q convention) used for VDL2 validation with dumpvdl2 2.6.0 as ground truth; 6 s vendored as CI fixture with attribution | https://www.sigidwiki.com/wiki/VHF_Data_Link_-_Mode_2_(VDL-M2) |
| acarsdec (TLeconte / f00b4r0) | LGPL/GPL-2 | Facts only (display conventions) | https://github.com/TLeconte/acarsdec |
| AIS-catcher (jvde-github) | GPL-3 | Facts only (landscape research) | https://github.com/jvde-github/AIS-catcher |
| gr-iridium (muccc) | GPL-3 | Facts only (wave 2 planning) | https://github.com/muccc/gr-iridium |
| SatDump | GPL-3 | Facts only (landscape research) | https://github.com/SatDump/SatDump |
| Scytale-C | GPL-3 | STD-C facts reference (see docs/notes/STDC.md) | https://bitbucket.org/scytalec/scytalec |
| inmarsatc (cropinghigh) | GPL-3 | STD-C facts reference (Scytale-C port; constants cross-verified) | https://github.com/cropinghigh/inmarsatc |
| dump1090-fa / readsb | GPL | Facts only (landscape research) | https://github.com/flightaware/dump1090 |

## Protocol documentation and articles

| Source | Used for |
|---|---|
| "The 1090 MHz Riddle" (junzis) — https://mode-s.org/decode/ | Mode S field layouts, ident charset, Q-bit altitude, published example frames (8D4840D6... → KLM1023; 8D40621D... → 38000 ft) |
| gpsd AIVDM documentation — https://gpsd.gitlab.io/gpsd/AIVDM.html | Canonical AIVDM test sentence (type 1, MMSI 477553000, channel B, *5C), armoring conventions |
| sigidwiki ACARS + Talk page — https://www.sigidwiki.com/wiki/Aircraft_Communications_Addressing_and_Reporting_System_(ACARS) | Bit-level walkthrough cross-check |
| WAVECOM ACARS / VDL-M2 decoder docs — https://www.wavecom.ch/content/ext/DecoderOnlineHelp/worddocuments/acars.htm | Odd-parity confirmation, label `_d` display convention, VDL2 burst corroboration |
| Universal Radio "ACARS Introduction" — https://www.universal-radio.com/catalog/decoders/acarsweb.pdf | Address dot-padding, MSN conventions |
| US Patent 4,569,061 | ACARS differential MSK confirmation |
| GE Patent US2016/0134682A1 — https://patents.google.com/patent/US20160134682A1/en | AOA 0xFF IPI + SOH EPI structure |
| Wiley *Aeronautical Air-Ground Data Link Communications* excerpt — https://catalogimages.wiley.com/images/db/pdf/9781848217416.excerpt.pdf | AOA vs ATN multiplexing on AVLC |
| J.-M. Friedt SDRA-2020 ACARS slides — http://jmfriedt.free.fr/sdra_acars.pdf | Preamble waveform cross-check |
| sigidwiki Inmarsat-C TDM — https://www.sigidwiki.com/wiki/Inmarsat-C_TDM | STD-C IQ test capture (Inmarsat-C_TDM_EGC_IQ.zip) |
| IMO/IHO SafetyNET manual — https://iho.int/uploads/user/Inter-Regional%20Coordination/WWNWS/Document%20Review/DRWG17/DRWG17_2019_3_EN-Inmarsat_SafetyNET_Manual-30.09.2018-track_Change.pdf | EGC area address bit layouts |

## Airframes ecosystem (ingest/compat targets)

| Source | Used for |
|---|---|
| docs.airframes.io feeding guide + decoder developer guidelines — https://github.com/airframesio/docs | Port/format table, station id conventions, metadata requirements |
| airframesio/aggregation-server — https://github.com/airframesio/aggregation-server | Legacy port plan, dormant gRPC proto (port 6001), asf-1.0 envelope |
| airframesio/airframes-client — https://github.com/airframesio/airframes-client | Prior protobuf art for asf-2.0 design |
| airframesio/stack (53labs) — https://github.com/airframesio/stack | Current Go/NATS ingest architecture asf-2.0 must integrate with |
| sdr-enthusiasts/acars_router — https://github.com/sdr-enthusiasts/acars_router | JSON normalization/dedup conventions, port-per-class scheme |
| sdr-enthusiasts/docker-acarshub — https://github.com/sdr-enthusiasts/docker-acarshub | Prometheus metric family conventions |

## Rust ecosystem decisions (researched 2026-06)

| Crate | Decision |
|---|---|
| soapysdr 0.4/0.5 (BSL-1.0) | SDR backend (mandatory for SDRplay); behind our IqSource trait |
| seify (Apache-2.0) | Deferred; avoid seify-rtlsdr (GPL-3 fork on crates.io) |
| rustfft/realfft, num-complex | DSP foundation |
| tonic 0.12 + prost 0.13 | gRPC; upgrade path to tonic 0.14 (grpc-rust org) noted |
| quinn 0.11 + rustls 0.23 (ring) | QUIC transport; gRPC-over-HTTP/3 (tonic-h3/h3) not yet stable |
| ratatui + crossterm | TUI (M8); sdrrat studied (no license — reference only) |
| crc 3.x | All CRC variants |
| Written in-house (no suitable crate) | PFB channelizer, DDC, RS errors-and-erasures, Viterbi (M6), all demodulators |
