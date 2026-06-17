# Iridium — implementation notes

xng-mode-iridium decodes Iridium L-band bursts end to end: PHY demod, the
layer-2 bitsparser port (access codes, BCH, deinterleave), full frame typing
(IRA/IBC/ITL/LCW + IMS pager + the duplex traffic classes), the SBD/IDA
transport with two-layer reassembly into ACARS, a wideband full-band hunter,
and beam-pattern reconstruction. Sources: **iridium-toolkit** (muccc, BSD-2 —
code portable with attribution; `bitsparser.py` / `bch.py` /
`reassembler.py` / `itl.py` are the layer-2/transport reference),
**gr-iridium** and **iridium-sniffer** (alphafox02; GPL-3 — **facts only**;
iridium-sniffer's `ARCHITECTURE.md` documents the whole pipeline with
parameters and is the best single PHY reference; `web_map.c` is the mt-position
reference). See PROVENANCE.md for the per-structure attribution.

## Pipeline

PHY demod (`demod.rs`, `wideband.rs`) → layer 2 framing/classify (`frame.rs`)
→ per-class decode (`ira.rs`, `itl.rs`, `ms.rs`, `voice.rs`, `iip.rs`,
`u3.rs`) → IDA/SBD reassembly + transport (`sbd.rs`) → application
(`gsm.rs`, `mtpos.rs`, ACARS via `xng-acars`) → normalized `Message`
(`lib::to_message`). `lib.rs` is the orchestrator: `decode_bits` (simplex
classes), `lcw_traffic_frame` + `decode_da_bits` (duplex classes), and
`handle_bits` (the shared single-channel/wideband dispatch + reassembler feed).

## PHY

- L-band 1616–1626.5 MHz. Duplex channels below ~1625.979 MHz (toolkit
  `f_duplex` incl. doppler guard), simplex above ~1626.104 (`f_simplex`).
  Ring-alert channel 1626.270833 MHz (`RING_ALERT_HZ`); quaternary messaging
  channels nearby.
- DQPSK, **25 000 symbols/s**, bursts. Channel rate 250 kHz (10 sps);
  one-sided passband 25 kHz; RRC matched filter.
- Burst anatomy: preamble tone (16 symbols normal, 64 long/simplex) →
  12-symbol UW → payload. Max burst 90 ms (2250 symbols); normal frames
  131–191 payload symbols, simplex 80–444.
- UW absolute QPSK symbols (units of π/2): DL `[0,2,2,2,2,0,0,0,2,0,0,2]`,
  UL `[2,2,0,0,0,2,0,0,2,0,2,2]` (gr-iridium); the toolkit's 24-bit "access
  codes" are the *differential decode* of these: DL
  `001100000011000011110011`, UL `110011000011110011111100`
  (`frame::ACCESS_DL` / `ACCESS_UL`).
- Demod: power-boxcar burst gate → tone-DFT coarse CFO → coherent UW fit
  (joint timing/phase/CFO weighted least squares over the 12 known UW
  symbols, the same machinery proven on VDL2/HFDL) → decision-directed phase
  trim (α=0.2 as in gr-iridium) → hard QPSK → differential decode
  `map[(s−prev) mod 4]` with `DQPSK_MAP = [0,2,3,1]` → 2 bits per mapped
  symbol MSB-first. The access-code tolerance gate sits at the random-match
  boundary (`ACCESS_TOL=12` of 24, env `XNG_IRIDIUM_ACCESS_TOL`); the 24-bit
  CRC is the real arbiter.

## Layer 2 (iridium-toolkit `bitsparser.py`, BSD — ported)

Bit stream starts with the 24-bit access code; `data` = the rest. Our demod
emits bits in gr-iridium "RAW" order; the BCH de-interleavers run on the
**symbol-reversed** stream (`frame::symbol_reverse`, two bits per pair
swapped). The all-`00`/`11` access code and the ITL/IMS headers are invariant
under the swap (so they classify without it); BCH-coded RA/IBC/LCW/IDA frames
are not.

Classification order (`frame::classify`, downlink heuristics):

1. **IMS messaging** if `data[0..32]` == `HEADER_MESSAGING`
   (`00110011111100110011001100110011 11110011`).
2. **ITL** ("TL" / ISY Time-Location) if the 96-bit header is `11` + 94 zeros
   (matched tolerantly, ≤3 deviating bits). Checked **before** IRA: the
   all-zero header is a valid (degenerate) BCH codeword that would otherwise
   mis-decode as a ring alert at sat 0 / position (0,0,0). The `10`-vs-`11`
   research-log note refers to the leading two header bits; only the `11`
   form is a real typed frame in either oracle (no distinct NXT/`10` frame
   exists — IRID-1).
3. **IBC** broadcast if BCH(7,3) poly 29 over `data[0..6]` == 0 and the 2-way
   deinterleave of the next 64 bits passes ringalert BCH.
4. **LCW** (duplex) via the 46-bit LCW permutation + BCH 29/465/41
   (zero-syndrome gate here; the decode path loosens it — see below).
5. **IRA** if the 3-way deinterleave of `data[0..96]` yields 3 BCH blocks,
   each at least correctable, with ≥1 clean and total correction ≤3.

- BCH polys (toolkit integer convention): ringalert/IBC **1207**, messaging
  **1897**, IBC/LCW header **29**, LCW parts 465/41, IDA/ACCH **3545**. RA/MS
  blocks are 32 bits = 31 BCH (21 data + 10 check) + 1 even-parity bit; repair
  searches 1–2 bit flips. `ecc_blocks` trusts a weight-1 BCH correction even
  when the separate even-parity bit is flipped (unambiguous on this d=5 code);
  only an `errs==2` correction with bad parity truncates.
- Deinterleave reads symbol pairs from the end backwards (toolkit
  `de_interleave`): 2-way 64→2×32, 3-way 96→3×32.
- FILL pattern removal (`strip_fill`, ≤2 bitdiff per 32-bit half):
  `FILL_A=1010001001110011 1011111101101101`, `FILL_B=0101010001000101
  1100001011100110`.

### IRA (ring alert) — `ira::parse_ra` (`kind:"ring-alert"`)

Concatenated 21-bit BCH data blocks:

| Bits | Field |
|---|---|
| 0–6 | satellite id |
| 7–12 | beam id |
| 13–24 | pos_x (sign + 11) |
| 25–36 | pos_y |
| 37–48 | pos_z |
| 49–55 | RA interval (90 ms units) |
| 56 | broadcast timeslot |
| 57 | EPI |
| 58–62 | BCH downlink sub-band |
| 63… | pages, 42 bits each: tmsi(32) + msc_id(bits 34–39); all-ones page = END |

lat = atan2(z, √(x²+y²)) (geocentric), lon = atan2(y, x), radius =
4·√(x²+y²+z²) km, alt = radius − 6378 + 23 km. The degenerate all-zero
header (sat 0, x=y=z=0) is rejected — no broadcasting satellite at Earth's
center. `pages_complete` flags whether the END page was seen;
`bch_corrected` carries the corrected-block count.

### IBC (broadcast) — `ira::parse_bc` (`kind:"broadcast"`)

BCH(7,3) header byte → `bc_type`; payload packed as 42-bit blocks. For
`bc_type==0`: a satellite/cell descriptor (sat, beam, slot, sv_blocking,
acq_classes, acq_sub_band, acq_channels) and a type-tagged info block —
`info_type` 0 = max uplink power, 1 = broadcast time (`iri_time` +
`iri_time_unix`), 2 = TMSI expiry (`tmsi_expiry` + `_unix`). Remaining blocks
are channel **assignments** (random_id, timeslot, uplink/downlink sub-band,
access, dtoa, dfoa), skipping the `111`+0 filler.

### ITL (Time-Location / ISY) — `itl::decode_itl` (`kind:"itl"`)

The 768-bit payload after the 96-bit header is read as 384 DQPSK symbols
(absolute symbols recovered by inverting `DQPSK_MAP` and integrating), split
into I/Q via the toolkit gray map. The I channel carries a 128-bit PRS
**version** header then a 256-bit PRS **plane** code; the Q channel carries
four 96-bit PRS **message** codes. Each field is matched to the nearest known
PRS sequence by Hamming distance (sequences are pseudo-random and far apart,
so off-air bit errors tolerate). `map_sat` resolves message-code 0 + version
to a satellite / message label (S## planes/positions, R## relays, M##/N##
message types, version 1 and 2 tables in `itl_tables.rs`). Version 0 (all-zero
PRS_HDR, idle) is rejected. Real captures are dominated by ITL bursts.

### IMS pager (messaging) — `ms::parse` (`kind:"msg"` / `"msg-complete"`)

Ported from `IridiumMSMessage` family (ref. US 5,596,315). 21-bit BCH data
blocks (messaging poly): block 0 is the header (super-frame block/frame
counters, length, group); the body interleaves a per-block "odd bit" with
20-bit slices carrying the **RIC** (LSB-first), **format**, **seq**, and
content. Formats decoded: **5** = 7-bit ASCII text (with the 1023-mod-1024
block checksum `csum_ok` and multi-part `ctr`/`ctr_max`), **3** = BCD digits;
anything else → raw hex. Multi-part ASCII pages reassemble per RIC
(`PagerReassembler`, 60 s timeout) and emit a `msg-complete` frame.

- **Acquisition group ("AQ", group "A", ms_type==1)** — `ms::MsAcq` (IRID-1):
  the header bits 19/20 (`unknown1`/`secondary`) and the 12-bit pre-message
  counter `ctr1` from the first pre-message block are exposed rather than
  discarded. Non-acquisition frames carry no `acq`.

### LCW + duplex traffic classes (`lib.rs`, `frame.rs`)

Every duplex burst carries a 46-bit **LCW (Link Control Word)**:
`frame::decode_lcw` applies the permutation + three BCH components (29/465/41;
lcw2 is transmitted one bit short, both completions tried). The decode path
deliberately does **not** require the strict zero-syndrome `classify()==Lw` —
real off-air LCWs carry a few bit errors; it accepts light correction (≤2
errs for CRC-less classes, ≤6 for DA which is CRC-protected downstream).
`lcw_descriptor` decodes the control word into structured JSON: **maint**
(sync/switch/geoloc/maint[1,2] with dtoa/dfoa/lqi/power), **acchl**, **hndof**
(handoff_cand / handoff_resp with cand/denied/slot/sband/access). The 3-bit
frame type (`ft`) selects the class:

| ft | kind | decode |
|---|---|---|
| 0 | `voice` | `voice::classify_voice` ladder |
| 1 | `ip-data` | `iip::parse_ip_payload` ladder |
| 2 | (→ `ida`) | DA frame → SBD reassembly (not a `lcw_traffic_frame`) |
| 3 | `u3` | `u3::parse_u3` (mission-control in-band signalling) |
| 6 | `u6` | generic, `frame_ft` recorded |
| 7 | `sync` | filler-deviation `sync_errors` / `sync_idle` |
| other | `lcw` | generic, so no duplex burst is silently dropped |

Idle/all-zero bursts are dropped up front (`handle_bits`): a payload <10%
ones BCH-corrects to the trivial all-zero codeword and would surface as a
phantom ring alert / empty voice frame.

#### Voice ladder — `voice::classify_voice`

iridium-toolkit ladder (oracle-validated): **VDA** (CRC24 over the bit-reversed
payload passes → an IIP frame riding the voice channel) → **VO6** (RS(52,42)
over GF(64), full Berlekamp-Massey/Chien/Forney decoder) → **VOD** (shortened
GF(256) RS, 31 data + 8 transmitted + 8 erased checks) → **VOZ**
(zero-padded, byte-sum ≡ 0) → **VOC** (AMBE voice — codec proprietary, bytes
surfaced as `ambe_hex` only; decoding is out of scope, as in iridium-toolkit).

#### IP ladder — `iip::parse_ip_payload`

`IridiumIPMessage` ladder: **IIP** (CRC24 passes → ARQ frame: type
[`ack-idle`/`data`], seq, ack, header checksum, data) → **IIR** (GF(256) RS
codeword whose one's-complement-16 checksum is 0) → **IIQ** (3 flag bits +
13-bit counter + data) → **IIU** (unknown). The IP channel is ~88%
unencrypted, so plaintext IIP-`data` and IIR payloads are scanned for
upper-layer credentials (IRID-2): **PPP PAP** Authenticate-Request
(peer-id + password, RFC 1334 §2.2, PPP protocol 0xC023) and **HTTP
Basic-Auth** (`Authorization: Basic <base64>` → user:pass, RFC 7617 / 4648).
Found credentials attach as a `credentials` array. Neither toolkit decodes
this layer (they stop at "IP via PPP"); the parser is verified against the
published RFC byte layouts and base64 test vectors, per-frame only
(cross-frame IP-session reassembly deferred).

#### U3 — `u3::parse_u3`

`IridiumLCW3Message`: GF(256) byte code → **I38** (16-bit checksum + odd byte)
else GF(64) 6-bit code → **I36** (first symbol = numeric sub-format; sub-formats
6 / 32 / 34 unpack 24-bit number groups) else raw → **IU3**.

## IDA → SBD transport → ACARS (`sbd.rs`, `frame.rs`)

ft==2 (DA) bursts carry SBD/ACARS, CRC-protected. `frame::decode_da` maps the
312-bit post-LCW payload: 124-bit chunks → 2-way deinterleave → BCH(31,20)
poly 3545 blocks in `[b4,b2,b3,b1]` order → 200 data bits, with the
continuation flag, 3-bit counter, length, 20 payload bytes, and a CRC-CCITT
(CRC-16/IBM-3740) check. Each CRC-OK DA is emitted as a `kind:"ida"` frame and
fed to the reassembler.

Reassembly is two layers (toolkit `ReassembleIDA` / `ReassembleIDASBD`):

- **Layer A** — DA bursts → one IDA packet, grouped by frequency (±2 kHz),
  direction, sequential 3-bit counter, and time (≤280 ms inter-fragment, 1 s
  buffer life). The concurrent in-flight list is essential in the wideband
  path where many channels are active at once.
- **Per-packet decoders** run first on the assembled IDA packet: **mt-position**
  (`mtpos::extract`, `kind:"mt-position"`) and **GSM signalling**
  (`gsm::decode`, `kind:"gsm"`).
- **SBD transport** (`sbd_parts`): routes by type — `0x0600` mobile-originated
  registration ("HELLO", 29-byte pre-header: IMEI via BCD on the 0x20
  sub-type, MOMSN, message count, registration timestamp via `iri_time_unix`),
  `0x76xx` transfer (`0x26` 7-byte pre-header → mtmsn/packets/backlog, or
  `0x20` 5-byte), the `0x10` len/seq header, and an optional uplink ack/nack
  prefix.
- **Layer B** — multiple IDA packets → one SBD message by `msgno`/`msgcnt`
  (packet count from the `0x26` pre-header byte 3). `msgno==0` /
  `msgcnt<=1` parse immediately; `msgcnt>1 && msgno==1` buffers a `MultiSbd`;
  `msgno>1` appends in sequence, completing at `msgno==msgcnt` and tagging the
  result `multi_packets`. Partials expire after 5 s. Long ACARS/SBD bodies
  split across packets reassemble here. In practice the live downlink is
  almost all single-packet control-plane traffic.
- **ACARS-over-SBD** (`parse_acars`): an SOH (0x01) payload (skipping the
  optional 0x03 8-byte header) is parsed by `xng_acars::block` and surfaces as
  a first-class ACARS message (`to_message` maps it to `MessageBody::Acars`,
  like the other ACARS carriers). Non-ACARS SBD (most of it — device
  telemetry/status) is rendered as `payload_text` when printable.

### GSM signalling — `gsm::decode` (`kind:"gsm"`)

Iridium tunnels a GSM-derived protocol over the reassembled IDA channel
(toolkit `ReassembleIDAPP`). The first byte is the protocol discriminator /
transaction major; labelled protocols: **CC** (0x03), **MM** (0x05), **RR**
(0x06, IRID-3), **GMM** (0x08, IRID-3), **SS** (0x0b, IRID-3), **SMS** (0x09),
plus the dest variants. The 16-bit transaction identifier is mapped to message
names:

- CC: Alerting, Call Proceeding, Progress, Setup, Connect Ack, Disconnect,
  Release / Release Complete (with decoded disconnect-cause IE).
- MM: Location Updating Request/Accept/Reject (with LAI = mcc/mnc/lac + mobile
  identity), Authentication Request/Response, Identity request/response (IMSI/
  IMEI/TMSI mobile-identity IE), TMSI Reallocation Command, CM Service
  Accept/Reject.
- RR: System Information 1–6 / 2bis/2ter/2quater/5bis/5ter, Paging Request
  1–3 / Response, Immediate Assignment (+Extended/Reject), Additional
  Assignment, Channel Release. Iridium-observed types (Imm-Assign-Reject 06.3a,
  Additional-Assignment 06.3b, SI-5bis 06.05, SI-2quater 06.07) come from
  toolkit `IDA-GSM.txt`; the rest follow GSM 04.08 / 3GPP TS 44.018 §10.4.
- GMM: Detach Request (0x0805). SS: Register (0x0b3b). SMS: CP-DATA/ACK/ERROR.

The raw L2 bytes + direction are carried as `raw_l2_hex`/`ul` for GSMTAP /
Wireshark export. `0x0600` (Register/SBD-uplink) and `0x76xx` are deferred to
the SBD/ACARS path, not stolen by the GSM labeller.

### mt-position — `mtpos::extract` (`kind:"mt-position"`)

Ported from iridium-sniffer `web_map.c mtpos_ida_cb`. Some GSM-paging
(`0x0605`), SBD-paging (`0x7605`), and uplink (`0x0600`) IDA messages embed
the mobile terminal's own ECEF position as three signed 12-bit values (4 km
units) — a position source distinct from the IRA satellite positions (the
terminal is an Iridium-equipped aircraft or vessel). Derives lat/lon/alt;
gated to a plausible Earth radius (5000–7000 km) so a false-pass can't plant a
phantom.

## Broadcast-time conversion (`ira::iri_time_unix`, IRID-8)

`fmt_iritime` (90 ms ticks, the two ERA2-window leap seconds 2015-06-30 /
2016-12-31) extended for the network's periodic **re-epoch** (L-Band Frame
Number reset), which the stock toolkit does not handle. The counter restarts
near zero at each re-epoch (ERA1 2007-03-08, ERA2 2014-05-11, ERA3
2025-02-14, ERA4 2026-01-14 18:08 UTC per the MetOcean bulletin / 2026
security analysis), so the era in force at the frame's wall-clock receive time
is selected. Without this, every post-2025 frame decoded ~11 years into the
past. The ERA2 path remains bit-identical to `fmt_iritime` (pinned in
`ira::time_tests` and the off-air `tmsi_expiry` oracle).

## Wideband pipeline (`wideband.rs`)

The full-band hunter detects bursts by FFT across the whole capture, downmixes
each to baseband, and feeds the per-burst demod — no channel list needed. It
runs multi-threaded over a real off-air capture (Airspy R2 / SAWbird+IR /
Maxtena PN100) at up to 10 MS/s. Design points the live decode depends on:

- **Channelize with the DDC, not a boxcar.** Each burst goes through
  `xng_dsp::Ddc` (the same two-stage windowed-sinc the single-channel path
  uses), one-sided passband **28 kHz**. A boxcar-of-decim averager folds ~8 dB
  of wideband noise into the channel (measured peak/noise 8.5 dB vs 16.6 dB
  through a real FIR on the same burst).
- **Seed the demod noise floor.** The demod's asymmetric noise EMA needs
  ~1400 quiet samples to converge; a wideband-extracted burst has only ~1000
  channel samples of pre-roll, so an unseeded floor freezes ~18 dB high and
  the acquisition gate sits above the signal. The front end estimates channel
  noise (20th-percentile power) and `seed_noise()`s the demod.
- **Don't over-reject in the BCH/classify gates** (see Layer 2).

## Acquisition and sensitivity

The acquisition chain is a faithful port of gr-iridium's, plus a few beyond-gr
refinements (all env-overridable; defaults given). On a shared 300 s off-air
capture (Airspy R2, 1622 MHz, 10 MS/s, KSMF) it runs ahead of gr-iridium:

| | xng | gr-iridium |
|---|---:|---:|
| CRC-OK IDA frames | **758** | 573 |
| total IDA frames | 1577 | 1214 |
| distinct-content CRC-OK | **587** | (573 raw) |

gr-iridium = `iridium-extractor -o -c 1622000000 -r 10000000 -f ci16_le FILE`
→ `iridium-parser.py -o line`; CRC-OK = lines with `CRC:OK`. xng =
`xng decode FILE -f cs16 -r 10000000 -c 1622.000M --channels 1622.000M
--mode iridium`; CRC-OK = IDA frames with `body.details.crc_ok`. Both
pass-rates match (~48 %), so the whole gap is IDA-frame **production** (weak
bursts reaching a valid frame at all), not decode quality.

Chain:

- **Detector** — gr `fft_burst_tagger`: 512-frame rolling-*mean* baseline,
  threshold `10^(dB/10)/ENBW` (ENBW 1.72, default **16 dB**), integer
  peak-bin centering, ±burst_width/2 mask, `(fs/burst_width)·0.8` max-bursts
  squelch, per-bin freeze. gr defaults to 7 dB, but 16 dB decodes more here:
  the extra weak/noise detections a lower threshold produces each claim a
  per-channel demod slot but rarely convert.
- **Channelization** — per-burst `Ddc` to 250 kHz, **28 kHz** one-sided
  passband (gr's gentle `input_fir` passes energy out to ~28 kHz too).
- **Fine CFO** — gr's squared-FFT estimate (square the preamble+UW → tone at
  2·CFO, Blackman window, 16× zero-padded FFT, quadratic interp, halve),
  re-estimated **per frame**; a ±640 Hz residual-CFO grid (`CFO_REFINE=2`) is
  searched jointly with timing in the sync correlation.
- **Sync** — full **28-symbol** (16 preamble + 12 UW) coherent correlation at
  full sample resolution, DL and UL, free initial phase; no magnitude gate.
- **Multi-frame** — decode **every** TDMA time-slot frame in one detector
  window (`handle_multiple_frames_per_burst`). 24 ms post-roll keeps detection
  alive across short inter-slot gaps so adjacent frames land in one window.
- **End-of-frame** — trim only after **3 consecutive** symbols below **peak/8**
  (−18 dB from the burst's own max), reading a full ≤191-symbol frame.
  Breaking on the first payload symbol below `noise×4` (absolute) truncates
  weak frames below the BCH/CRC length and loses them — the change that took
  CRC-OK IDA 516→758.
- **Validation** — differential access-code gate at the random-match boundary
  (≤12 of 24 bits; the 24-bit CRC, false-pass ≈ 6e-8, is the real arbiter) →
  BCH (matches iridium-toolkit exactly) → CRC.
- **Two-filter union** — every burst is demodulated both unfiltered and through
  the RRC matched filter; the two recover largely *disjoint* populations
  (strong vs weak: 182 vs 758 CRC-OK alone), so both are emitted and deduped
  by decoded content (the SBD reassembler is fed exactly once per burst,
  preferring the matched-filter alternate when it yields a valid frame).

**Tested and rejected** (standing warnings): lowering the detector threshold
floods with noise detections that starve the demod slots; a preamble
decision-directed PLL hurts (the 28-symbol batch correlation already extracts
the optimal phase). `MIN_BURST_SPAN=0` (extracting single-frame bursts, as gr
does) adds frames offline but **floods the per-channel decode queues on the
live station** and grows `chan_dropped`; the default (2,
`XNG_IRIDIUM_MIN_BURST_SPAN`) keeps the soak drop-free at 10 MS/s.

### Soft-decision weak-frame recovery (IRID-5, opt-in `XNG_IRIDIUM_MAX_EFFORT`)

A beyond-gr lever for the weak-burst tail, **off by default**. When enabled, the
demod attaches per-bit reliabilities (each DQPSK symbol's amplitude × decision-
boundary margin) parallel to the hard bits, and the decoder runs (1) a UW
access-code error-correction pre-classify that snaps a near-threshold
differential access code to its exact DL/UL word, and (2) **Chase-2 soft-decision
BCH** (`bch_repair_soft`: flip the `p` least-reliable bits over `2^p` test
patterns, hard-decode each via the existing `bch_repair`, keep the minimum
reliability-weighted distance) on the RA/IBC/MS blocks. With `soft = None` the
path is **bit-identical** to the default decode (`decode_bits_soft(bits, None) ==
decode_bits(bits)`, pinned by `chase_p0_equals_hard`), so enabling the flag can
only add frames, never drop them.

Substantiation is an AWGN Monte-Carlo over the shipped decoders: per-block
decode success rises **77.2 % → 95.8 % (+18.6 pts)** at σ=0.62. On the 300 s
benchmark capture above, however, max-effort yields **no net new CRC-OK IDA
frames** (1577 either way) — that capture is *acquisition*-limited, not
BCH-limited (xng's CRC pass-rate already matches gr's), so there are no
BCH-recoverable frames left to win. The lever pays off only where the SNR floor
produces genuine BCH bit-errors on otherwise-acquired frames. A
no-regression benchmark gate confirmed default and max-effort both hold at 1577.
Reuses the crate's existing `bch_repair`, deinterleavers, and demod machinery;
no new dependencies. (Verified against iridium-toolkit's published BCH generator
polynomials, not a self-encode loopback.)

## Beam-pattern reconstruction (`src/beam.rs`, app layer)

IRA ring-alert frames carry two position kinds at different altitudes: the
broadcasting satellite (~780 km) and a ground beam footprint (~0 km;
iridium-toolkit's "down" positions). `classify_altitude` splits them into
satellite-track updates and footprint observations, dropping anything outside
the physical bands so a BCH/CRC false-pass cannot plant a phantom.

Each footprint is de-rotated into the broadcasting satellite's own frame
(cross-track / along-track km), so a beam accumulates a stable mean regardless
of where the satellite is when heard. Direction (north/south) comes from the
geocentric-z trend of successive fixes; it is sticky across short gaps.

The drawn pattern is the canonical **48-beam, 4-tier** layout — 3 Main Mission
Antennas × 16 beams, tiers of 3 / 9 / 15 / 21 from nadir outward (MathWorks
Satellite Communications Toolbox Iridium model, FCC filings). Tier ground radii
are the off-nadir boresight angles (~11° / 24° / 42° / 59°) projected from
780 km onto the Earth sphere. The three inner tiers match the ~1480 km extent a
single station decodes; the **outer tier is stretched to the documented
~2250 km radius / ~4500 km footprint** (edge ~62°, just inside the 62.97°
horizon limb) — faint limb beams illuminate the ground but rarely decode here,
so they belong on the map as modelled coverage even when unheard.

Beams render in three confidence tiers: **active** (swept this station within
~30 s) in beam colour; merely **decoded** (≥2 low-scatter observations) as
muted grey at its *measured* position; not-yet-decoded **modelled** slots as a
faint dashed gap-fill at the canonical position. A polluted average (RMS
scatter > 600 km, i.e. direction-fold) falls back to the modelled slot.
Footprint polygons and the satellite ground track are **unwrapped across the
±180° antimeridian**: every vertex/trail point is shifted into the ±180° window
of the satellite's sub-point so Leaflet draws across the seam instead of the
long way round. The dashboard shows one satellite's pattern at a time (click to
pin); satellites expire 2 min after last contact.

## Validation / oracles

Layer 2 is **oracle-validated against iridium-toolkit** (`bitsparser.py`,
`iridium-parser.py`), and the PHY is **cross-validated against gr-iridium**.
Together both layers are pinned to their reference implementations.

- **PHY (`crossval.rs`)** — gr-iridium's own reference burst
  (`prbs15-2M-20dB`, ~3 dB full-band SNR) demodulates bit-perfectly: access
  code recognized, zero PRBS15 recurrence violations across the payload. The
  DDC'd burst is vendored as a 32 KB CI fixture
  (`demodulates_gr_iridium_test_burst`). The wideband path finds and decodes
  the same burst re-upconverted to an arbitrary offset in a 2 MHz band
  (`wideband.rs`).
- **RF loopback (`e2e.rs`)** — generated IRA / IMS-pager bursts (CFO + noise)
  decode through the full chain; `rejects_all_zero_ring_alert` guards the
  degenerate-frame rejection.
- **IRA / IBC / IDA / IMS / DA oracle vectors (`e2e.rs`, `sbd_acars.rs`)** —
  frames generated by the TX helpers decode field-identically in
  `bitsparser.py` (sat/beam/xyz/interval/flags/sub-band/TMSIs; IBC
  sat/beam/assignments/info; IDA cont/ctr/len/CRC; IMS ric/fmt/seq/text).
- **Real off-air vectors (`offair_oracle.rs`)** — live KSMF bursts pinned to
  `iridium-parser.py`: a ring alert (sat 044 beam 25 pos +40.11/−127.29
  tmsi 071ca54a), a CRC-OK IDA/DA handoff burst, two IBC frames
  (sat/beam/assignments + `tmsi_expiry` counter 32768), and a U3 LCW handoff.
  These also guard the symbol-reverse bit-order convention.
- **Voice / IP / U3 (`voice.rs`, `iip.rs`)** — one vector per ladder stage
  (VDA/VO6/VOD/VOZ/VOC, IIP/IIQ/IIR/IIU) generated with the toolkit's own
  rs/rs6/crcmod code; CRC24 check value `iip_crc24("123456789")==0xbde882`.
- **GSM (`gsm.rs`)** — RR/GMM/SS labelling against `IDA-GSM.txt` + GSM 04.08
  §10.4; the 0x0600/0x76 deferral to the SBD path is regression-tested.
- **Credentials (`iip.rs`)** — PPP-PAP and HTTP Basic-Auth parsers verified
  against RFC 1334 / 7617 byte layouts and RFC 4648 base64 vectors (no decoder
  oracle exists — neither toolkit decodes this layer).
- **Time (`ira::time_tests`)** — ERA2 path bit-identical to `fmt_iritime`;
  ERA3/ERA4 re-epoch selection tested at the boundary.

Not count-gated in CI: the 300 s capture is 11 GB, too large to vendor. The
demod core is fenced by the bit-exact and field-exact oracle tests; the IDA
production number is a documented benchmark (BENCHMARKS.md). The ACARS-over-SBD
path is exercised end to end in `sbd_acars.rs` (`sbd_acars_end_to_end`,
`reassembles_interleaved_channels`).

## Known limitations / intentional gaps

- **AMBE voice** (VOC) is classified and the candidate bytes surfaced, but the
  codec is proprietary — not decoded (as in iridium-toolkit, which defers to an
  external `ir77_ambe`).
- **IP-session reassembly** is per-frame plaintext only; cross-frame IP/PPP
  session reassembly is deferred.
- **Encrypted IP** (~12% of the channel) is not decrypted.
- No distinct `NXT`/`10`-prefix sync frame is decoded — no oracle reference
  exists for one (IRID-1).

## Key references

- iridium-toolkit (muccc, BSD-2): `bitsparser.py`, `bch.py`, `reassembler.py`
  ({ida,sbd}.py), `itl.py`, `util.fmt_iritime`, `IDA-GSM.txt` — the layer-2 /
  transport / GSM-label reference, ported with attribution.
- gr-iridium + iridium-sniffer (alphafox02, GPL-3): PHY facts, burst-detector
  parameters, the IDA/SBD→ACARS pipeline, `web_map.c` mt-position; reference
  test burst for PHY cross-validation. **Facts only.**
- RFC 1334 (PPP PAP), RFC 7617 (HTTP Basic), RFC 4648 (base64); GSM 04.08 /
  3GPP TS 44.018 §10.4 (RR/MM/CC message types); US 5,596,315 (pager format).
- MathWorks Satellite Communications Toolbox Iridium model + FCC filings
  (beam layout); MetOcean technical bulletin / 2026 security analysis (re-epoch
  instants).
