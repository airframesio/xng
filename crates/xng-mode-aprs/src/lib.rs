//! Native APRS / AX.25 packet-radio decode core for xng.
//!
//! APRS is the Automatic Packet Reporting System: on VHF (144.39 MHz in
//! North America, 144.800 MHz in Europe) it is **Bell 202 AFSK** — 1200 Hz
//! "mark" / 2200 Hz "space" tones keyed at 1200 baud — carried in
//! **narrowband FM**, framed as **AX.25 v2.2 Unnumbered-Information (UI)**
//! packets whose information field is an **APRS Protocol Reference 1.0.1**
//! payload.
//!
//! This crate implements the full receive stack, bottom-up, with every
//! protocol fact anchored to an external reference (see PROVENANCE.md):
//!
//! - [`demod`] — FM discriminator -> Bell 202 AFSK1200 correlator -> bit
//!   timing recovery, emitting NRZI line symbols.
//! - [`hdlc`] — NRZI decode, HDLC bit de-stuffing, `0x7E` flag framing
//!   (AX.25 §3.6–§3.8).
//! - [`ax25`] — AX.25 v2.2 UI frame parsing: dest/source/digipeater address
//!   extraction (callsign ASCII<<1 + SSID octet, final-octet LSB=1), control
//!   `0x03`, PID `0xF0`, and the X.25 FCS check (§3.9–§3.14).
//! - [`aprs`] — APRS 1.0.1 payload dispatch on the data-type identifier:
//!   position (uncompressed DDMM.mm + Base-91 compressed, with course/speed,
//!   PHG/DFS/RNG data extensions and the compressed cs/T sub-field), weather,
//!   message, bulletin/announcement, status (incl. Maidenhead grid), object,
//!   item, general query, telemetry.
//! - [`mice`] — Mic-E (APRS 1.0.1 Chapter 10), the most common on-air format:
//!   the latitude/message-code/N-S-E-W/longitude-offset live in the AX.25
//!   destination address and the longitude/speed/course/symbol in the info
//!   field, so it is decoded at the [`decode_frame`] level across both fields.
//!
//! # IQ front end
//!
//! [`AprsChannelDecoder`] is the channelized IQ entry point, mirroring the
//! NAVTEX/AIS template: it owns an [`xng_dsp::Ddc`] that mixes a wideband
//! capture by `freq_offset_hz` and decimates to [`CHANNEL_RATE`], then runs
//! the [`demod::AfskDemod`] -> HDLC -> AX.25 -> APRS pipeline and emits an
//! [`AprsFrame`] per recovered packet. [`to_message`] normalizes a decoded
//! frame into the [`xng_types`] bus form.
//!
//! The DECODE/framing layers (AX.25 address rule, FCS, APRS payload formats)
//! stay oracle-anchored by their own spec-cited tests; the
//! modulate->AWGN->demod path used to validate the front end is
//! self-generated (see PROVENANCE.md and the `*_synth_iq` / BER tests).

pub mod aprs;
pub mod ax25;
pub mod demod;
pub mod hdlc;
pub mod mice;
pub mod modulate;

pub use aprs::{AprsKind, AprsPayload};
pub use ax25::{Address, Ax25Frame};

use chrono::Utc;
use num_complex::Complex;
use xng_dsp::Ddc;
use xng_types::{DecodeQuality, Message, MessageBody, Mode, Provenance, SignalQuality};

/// Internal demod sample rate: an integer multiple of 1200 Bd that comfortably
/// resolves the 2200 Hz space tone (Nyquist) and the FM swing. 38400 S/s = 32
/// samples/bit.
pub const CHANNEL_RATE: f64 = 38_400.0;

/// One-sided DDC passband. A 2.2 kHz top AFSK tone under narrowband FM
/// (≈±3 kHz deviation, ≈±5 kHz Carson bandwidth) fits well inside this; it
/// rejects the adjacent 25 kHz VHF channels.
pub const CHANNEL_PASSBAND_HZ: f64 = 7_000.0;

/// One fully decoded APRS packet: the AX.25 frame, the parsed APRS payload,
/// and the raw link-layer octets it was recovered from.
#[derive(Debug, Clone)]
pub struct AprsFrame {
    /// The decoded AX.25 UI frame (addresses, control, PID, info, FCS state).
    pub ax25: Ax25Frame,
    /// The parsed APRS payload (class + decoded fields).
    pub payload: AprsPayload,
    /// Raw deframed link-layer octets (address…control PID info FCS).
    pub raw: Vec<u8>,
}

/// Decode one APRS channel out of a wideband (or already-channelized) capture.
///
/// Mirrors the NAVTEX [`xng_mode_navtex::NavtexChannelDecoder`] contract:
/// owns an internal [`Ddc`] that mixes by `freq_offset_hz` and decimates the
/// capture to [`CHANNEL_RATE`], runs the AFSK/HDLC/AX.25/APRS pipeline, and
/// emits one [`AprsFrame`] per recovered UI packet.
pub struct AprsChannelDecoder {
    ddc: Option<Ddc>,
    demod: demod::AfskDemod,
    channel_buf: Vec<Complex<f32>>,
    /// Keep only AX.25 frames whose FCS validated (set false to also surface
    /// CRC-failed candidates).
    require_fcs: bool,
}

impl AprsChannelDecoder {
    /// `input_rate` is any capture rate ≥ [`CHANNEL_RATE`]; a non-integer
    /// multiple is resampled by the DDC. `freq_offset_hz` is the APRS channel
    /// center relative to the capture center (0 if already centered on the
    /// carrier).
    pub fn new(input_rate: f64, freq_offset_hz: f64) -> Result<Self, String> {
        let ddc = if (input_rate - CHANNEL_RATE).abs() < 1e-6 && freq_offset_hz.abs() < 1e-6 {
            None
        } else {
            Some(Ddc::new(
                input_rate,
                CHANNEL_RATE,
                freq_offset_hz,
                CHANNEL_PASSBAND_HZ,
            )?)
        };
        Ok(Self {
            ddc,
            demod: demod::AfskDemod::new(),
            channel_buf: Vec::new(),
            require_fcs: true,
        })
    }

    /// If `require` is false, frames whose FCS failed are still emitted (with
    /// `ax25.fcs_ok == false`). Default is true (only FCS-valid frames).
    pub fn set_require_fcs(&mut self, require: bool) {
        self.require_fcs = require;
    }

    /// Feed capture IQ; returns newly completed APRS frames.
    pub fn process(&mut self, input: &[Complex<f32>]) -> Vec<AprsFrame> {
        let channel: &[Complex<f32>] = match &mut self.ddc {
            Some(ddc) => {
                self.channel_buf.clear();
                ddc.process(input, &mut self.channel_buf);
                &self.channel_buf
            }
            None => input,
        };
        let raw_frames = self.demod.process(channel);
        let mut out = Vec::new();
        for raw in raw_frames {
            if let Some(frame) = decode_frame(&raw) {
                if frame.ax25.fcs_ok || !self.require_fcs {
                    out.push(frame);
                }
            }
        }
        out
    }

    /// Smoothed channel power level in dBFS.
    pub fn level_dbfs(&self) -> f32 {
        self.demod.level_dbfs()
    }
}

/// Decode a deframed AX.25 octet sequence (address…control PID info FCS) into
/// an [`AprsFrame`]: parse the AX.25 UI frame, then parse the APRS payload.
/// Returns `None` if it is not a parseable UI frame.
pub fn decode_frame(raw: &[u8]) -> Option<AprsFrame> {
    let ax25 = ax25::parse_frame(raw)?;
    // APRS rides UI/PID-0xF0; other UI frames still parse but payload is raw.
    let payload = if ax25.pid == 0xf0 {
        // Mic-E packs the latitude into the AX.25 destination address, so it
        // must be decoded with both the destination callsign and the info
        // field (APRS 1.0.1 Chapter 10). Detect it by the info field's Mic-E
        // data-type id (`` ` ``, `'`, 0x1c, 0x1d) and fall back to the
        // info-only dispatch for every other APRS data type.
        let info0 = ax25.info.first().copied();
        let is_mice = matches!(info0, Some(b'`') | Some(b'\'') | Some(0x1c) | Some(0x1d));
        if is_mice {
            match mice::parse(&ax25.dest.callsign, &ax25.info) {
                Some(m) => aprs::AprsPayload {
                    kind: aprs::AprsKind::MicE,
                    fields: m.fields,
                },
                None => aprs::parse(&ax25.info),
            }
        } else {
            aprs::parse(&ax25.info)
        }
    } else {
        aprs::AprsPayload {
            kind: aprs::AprsKind::Raw,
            fields: serde_json::json!({
                "info": String::from_utf8_lossy(&ax25.info),
                "pid": format!("0x{:02x}", ax25.pid),
            }),
        }
    };
    Some(AprsFrame {
        ax25,
        payload,
        raw: raw.to_vec(),
    })
}

/// Convert a decoded APRS frame into the normalized bus message.
///
/// `kind` is the APRS data class (`position`/`weather`/`message`/`status`/
/// `object`/`item`/`telemetry`/`mic-e`/`bulletin`/`query`/`raw`). `details` is
/// a JSON object carrying the AX.25
/// addressing (`source`, `dest`, `via`) merged with the decoded APRS fields
/// (`lat`, `lon`, `symbol`, `comment`, …). `decode.crc_ok` is the AX.25 FCS
/// result. `raw` carries the deframed link-layer octets.
/// Identify a space digipeater (ISS or an APRS satellite) from the AX.25 path.
/// Matches the well-known APRS-satellite digipeater callsigns in the `via`
/// list; the SSID is ignored. Returns the satellite's common name.
fn satellite_digipeater(ax25: &Ax25Frame) -> Option<&'static str> {
    for a in &ax25.via {
        let d = a.display();
        let base = d.split('-').next().unwrap_or("").to_ascii_uppercase();
        match base.as_str() {
            // ISS ARISS packet digipeater (and its historic aliases).
            "RS0ISS" | "ARISS" | "NA1SS" => return Some("ISS (ARISS)"),
            // US Naval Academy APRS sats (NO-84 / NO-104).
            "PSAT" | "PSAT2" => return Some("PSAT / PSAT-2"),
            // Generic APRS-satellite digipeater alias.
            "APRSAT" => return Some("APRS satellite"),
            _ => {}
        }
    }
    None
}

pub fn to_message(
    f: &AprsFrame,
    frequency_hz: u64,
    level_dbfs: f32,
    source: Provenance,
) -> Message {
    let kind = f.payload.kind.as_str().to_string();

    // Merge addressing into the payload fields.
    let mut details = match f.payload.fields.clone() {
        serde_json::Value::Object(m) => m,
        other => {
            let mut m = serde_json::Map::new();
            m.insert("info".into(), other);
            m
        }
    };
    details.insert("source".into(), serde_json::json!(f.ax25.source.display()));
    details.insert("dest".into(), serde_json::json!(f.ax25.dest.display()));
    let via: Vec<String> = f.ax25.via.iter().map(|a| a.display()).collect();
    details.insert("via".into(), serde_json::json!(via));

    // Space-based reception. 145.825 MHz is the international ISS / APRS-satellite
    // digipeat channel, so a frame heard there arrived via a spacecraft; tag it
    // so the source is unambiguous. The specific satellite is identified from the
    // digipeater callsign in the path when present (RS0ISS = ISS, etc.); the
    // station runtime layers TLE/overhead correlation on top when a receiver
    // position is configured (see `xng::aprs_sat`).
    const SAT_APRS_HZ: u64 = 145_825_000;
    let mut space = frequency_hz.abs_diff(SAT_APRS_HZ) <= 30_000;
    if let Some(sat) = satellite_digipeater(&f.ax25) {
        details.insert("satellite".into(), serde_json::json!(sat));
        space = true;
    }
    if space {
        details.insert("reception".into(), serde_json::json!("space"));
    }

    Message {
        mode: Mode::Aprs,
        timestamp: Utc::now(),
        frequency_hz,
        signal: SignalQuality {
            rssi_db: Some(level_dbfs),
            ..Default::default()
        },
        decode: DecodeQuality {
            crc_ok: f.ax25.fcs_ok,
            fec_corrected: None,
            errors: None,
        },
        body: MessageBody::Aprs {
            kind,
            details: serde_json::Value::Object(details),
        },
        raw: Some(f.raw.clone()),
        source,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channel_rate_is_integer_bit_multiple() {
        let samples_per_bit = CHANNEL_RATE / demod::BAUD;
        assert_eq!(
            samples_per_bit.fract(),
            0.0,
            "{samples_per_bit} samples/bit"
        );
        // Output rate must carry the two-sided passband (Nyquist).
        let min_rate = 2.0 * CHANNEL_PASSBAND_HZ;
        assert!(CHANNEL_RATE >= min_rate, "{CHANNEL_RATE} < {min_rate}");
        // And resolve the 2200 Hz space tone with margin.
        let tone_floor = 4.0 * demod::SPACE_HZ;
        assert!(
            CHANNEL_RATE >= tone_floor,
            "{CHANNEL_RATE} < {tone_floor} (space-tone resolution)"
        );
    }

    #[test]
    fn decode_frame_parses_ui_and_payload() {
        // Hand-built (spec-octet) frame from ax25 helpers, position payload.
        let raw = ax25::build_ui_frame(
            ("APRS", 0),
            ("N0CALL", 0),
            &[("WIDE1", 1)],
            b"!4903.50N/07201.75W-Test",
        );
        let f = decode_frame(&raw).expect("decode");
        assert!(f.ax25.fcs_ok);
        assert_eq!(f.payload.kind, aprs::AprsKind::Position);
        assert_eq!(f.ax25.source.callsign, "N0CALL");
    }

    /// SPEC GROUND TRUTH — APRS 1.0.1 Chapter 10, the information-field worked
    /// example (p.53), routed through a full AX.25 UI frame. Mic-E carries the
    /// latitude in the destination address, so this exercises the `decode_frame`
    /// wiring that joins the AX.25 destination callsign to the info field.
    ///
    /// The destination "T7P3SY" specifies western hemisphere + longitude offset
    /// +100 (bytes 5/6 = S/Y). The 9-byte info field is the literal spec bytes
    /// `` `(_fn"Oj/ `` which decode to longitude 112°07.74'W, speed 20 knots,
    /// course 251°, and the jeep symbol `/j` (p.53). `build_ui_frame` encodes
    /// the dest callsign by the AX.25 ASCII<<1 rule, exactly as a Mic-E TNC
    /// transmits it.
    #[test]
    fn mic_e_decodes_through_full_ax25_frame() {
        let raw =
            ax25::build_ui_frame(("T7P3SY", 0), ("N0CALL", 9), &[("WIDE1", 1)], b"`(_fn\"Oj/");
        let f = decode_frame(&raw).expect("decode Mic-E frame");
        assert!(f.ax25.fcs_ok);
        assert_eq!(f.payload.kind, aprs::AprsKind::MicE);
        let lon = f.payload.fields["lon"].as_f64().unwrap();
        assert!((lon - (-112.129)).abs() < 1e-3, "lon={lon}");
        assert_eq!(f.payload.fields["speed_knots"], 20);
        assert_eq!(f.payload.fields["course_deg"], 251);
        assert_eq!(f.payload.fields["symbol_code"], "j");
        assert_eq!(f.payload.fields["symbol_table"], "/");

        // And the normalized bus message carries kind "mic-e".
        let prov = Provenance {
            station: xng_types::StationIdentity::new("TEST-APRS"),
            app: xng_types::AppInfo::xng(),
            sdr: None,
            channel: None,
        };
        let msg = to_message(&f, 144_390_000, -20.0, prov);
        match &msg.body {
            MessageBody::Aprs { kind, details } => {
                assert_eq!(kind, "mic-e");
                // Addressing is still merged in.
                assert_eq!(details["source"], "N0CALL-9");
            }
            other => panic!("expected Aprs body, got {other:?}"),
        }
    }

    #[test]
    fn space_reception_tagged_and_satellite_identified() {
        // A frame digipeated by the ISS on 145.825 MHz: tagged space + ISS,
        // regardless of which 2m cluster channel it was tuned on.
        let raw = ax25::build_ui_frame(("APRS", 0), ("N0CALL", 9), &[("RS0ISS", 4)], b"!4903.50N/07201.75W-via ISS");
        let f = decode_frame(&raw).unwrap();
        let prov = Provenance {
            station: xng_types::StationIdentity::new("TEST-APRS"),
            app: xng_types::AppInfo::xng(),
            sdr: None,
            channel: None,
        };
        let msg = to_message(&f, 145_825_000, -20.0, prov.clone());
        match &msg.body {
            MessageBody::Aprs { details, .. } => {
                assert_eq!(details["reception"], "space");
                assert_eq!(details["satellite"], "ISS (ARISS)");
            }
            other => panic!("expected Aprs body, got {other:?}"),
        }
        // A terrestrial frame on 144.390 with no sat digipeater: no space tag.
        let raw2 = ax25::build_ui_frame(("APRS", 0), ("N0CALL", 7), &[("WIDE1", 1)], b"!4903.50N/07201.75W-terrestrial");
        let f2 = decode_frame(&raw2).unwrap();
        if let MessageBody::Aprs { details, .. } = &to_message(&f2, 144_390_000, -20.0, prov).body {
            assert!(details.get("reception").is_none(), "terrestrial must not be tagged space");
            assert!(details.get("satellite").is_none());
        }
    }

    #[test]
    fn to_message_merges_addressing_into_details() {
        let raw = ax25::build_ui_frame(
            ("APRS", 0),
            ("N0CALL", 7),
            &[("WIDE1", 1), ("WIDE2", 2)],
            b"!4903.50N/07201.75W-Hi",
        );
        let f = decode_frame(&raw).unwrap();
        let prov = Provenance {
            station: xng_types::StationIdentity::new("TEST-APRS"),
            app: xng_types::AppInfo::xng(),
            sdr: None,
            channel: None,
        };
        let msg = to_message(&f, 144_390_000, -20.0, prov);
        assert_eq!(msg.mode, Mode::Aprs);
        assert!(msg.decode.crc_ok);
        match &msg.body {
            MessageBody::Aprs { kind, details } => {
                assert_eq!(kind, "position");
                assert_eq!(details["source"], "N0CALL-7");
                assert_eq!(details["dest"], "APRS");
                assert_eq!(details["via"][0], "WIDE1-1");
                assert_eq!(details["via"][1], "WIDE2-2");
                let lat = details["lat"].as_f64().unwrap();
                assert!((lat - 49.058333).abs() < 1e-5);
            }
            other => panic!("expected Aprs body, got {other:?}"),
        }
    }
}
