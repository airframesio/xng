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
//!   position (uncompressed DDMM.mm + Base-91 compressed), weather, message,
//!   status, object, telemetry.
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
        aprs::parse(&ax25.info)
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
/// `object`/`telemetry`/`raw`). `details` is a JSON object carrying the AX.25
/// addressing (`source`, `dest`, `via`) merged with the decoded APRS fields
/// (`lat`, `lon`, `symbol`, `comment`, …). `decode.crc_ok` is the AX.25 FCS
/// result. `raw` carries the deframed link-layer octets.
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
        assert_eq!(samples_per_bit.fract(), 0.0, "{samples_per_bit} samples/bit");
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
