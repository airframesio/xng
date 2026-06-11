# Vendored ICAO ATN ASN.1 modules

`atn-cpdlc.asn`, `atn-cm.asn`, `atn-ulcs.asn` are the ICAO Doc
9880/9705 ASN.1 module definitions for the ATN air-ground applications
(protected-mode CPDLC, context management, upper layers). The module
text is ICAO's specification content; these copies were obtained via
the Wireshark project's transcription of the standard (module text
only — no Wireshark dissector code was consulted or ported).
`xng-mode-vdl2/src/atn_cpdlc_tables.rs` is generated from
`atn-cpdlc.asn` (element names, argument types, phraseology comments).
