//! Native VDL Mode 2 decode core.
//!
//! Pipeline per channel: wideband IQ → [`xng_dsp::Ddc`] → 50 kHz channel
//! IQ → [`demod::Vdl2Demod`] (D8PSK burst acquisition, header,
//! deinterleave + RS(255,249)) → [`avlc`] frame scan → ACARS-over-AVLC via
//! [`xng_acars::block`] → [`xng_types::Message`].
//!
//! See PROVENANCE.md for clean-room sourcing (no GPL decoder code used).

pub mod atn;
pub mod atn_cpdlc;
pub mod avlc;
pub mod demod;
pub mod header;
pub mod interleave;
pub mod modulate;
pub mod scramble;

use chrono::Utc;
use num_complex::Complex;
use xng_dsp::rs::ReedSolomon;
use xng_dsp::Ddc;
use xng_types::{DecodeQuality, Message, MessageBody, Mode, Provenance, SignalQuality};

/// Internal channel rate (≈4.76 samples/symbol).
pub const CHANNEL_RATE: f64 = 50_000.0;
/// Preferred channel rate (~9.5 samples/symbol): measurably better
/// off-air decode than the 50 kS/s floor; used whenever the capture
/// rate divides into it.
pub const CHANNEL_RATE_HI: f64 = 100_000.0;
/// One-sided passband: D8PSK 10.5 kBd, RC α=0.6 → ±8.4 kHz.
pub const CHANNEL_PASSBAND_HZ: f64 = 8_500.0;

/// One decoded AVLC frame plus its ACARS content when present.
pub struct Vdl2Frame {
    pub avlc: avlc::AvlcFrame,
    pub acars: Option<xng_acars::block::AcarsBlock>,
    /// Decoded ATN transport (X.25 packet, CLNP/COTP) for I-frames.
    pub atn: Option<serde_json::Value>,
    pub rs_corrected: usize,
    /// Carrier frequency offset (Hz) measured from the burst preamble (VDL2-7).
    pub freq_skew_hz: f32,
}

pub struct Vdl2ChannelDecoder {
    ddc: Option<Ddc>,
    /// Channel-selectivity lowpass for the no-DDC path. The RC(α=0.6)
    /// TX pulse occupies ±(1+α)·Rs/2 = ±8.4 kHz; a flat lowpass there
    /// keeps the signal spectrum (and its Nyquist zero-ISI property)
    /// untouched while removing all out-of-band noise. When a DDC runs
    /// its decimation filter already provides this.
    selectivity: Option<xng_dsp::fir::Fir>,
    select_buf: Vec<Complex<f32>>,
    demod: demod::Vdl2Demod,
    rs: ReedSolomon,
    channel_buf: Vec<Complex<f32>>,
    x25: atn::X25Reassembler,
    clnp: atn::ClnpReassembler,
    cotp: atn::CotpReassembler,
    samples_seen: u64,
    input_rate: f64,
}

impl Vdl2ChannelDecoder {
    pub fn new(input_rate: f64, freq_offset_hz: f64) -> Result<Self, String> {
        // Prefer the high channel rate when the capture divides into it;
        // 50 kS/s remains the floor (and the vendored-fixture rate).
        // Prefer 105 kS/s (an exact 10 samples/symbol) when the input
        // divides into it: at 100 kS/s every symbol center falls at a
        // fractional sample position and the linear interpolator's
        // error acts as decision noise; integer sps removes it.
        const CHANNEL_RATE_NATIVE: f64 = 105_000.0;
        let channel_rate = if input_rate >= CHANNEL_RATE_NATIVE
            && (input_rate / CHANNEL_RATE_NATIVE).fract().abs() < 1e-9
        {
            CHANNEL_RATE_NATIVE
        } else if input_rate >= CHANNEL_RATE_HI
            && (input_rate / CHANNEL_RATE_HI).fract().abs() < 1e-9
        {
            CHANNEL_RATE_HI
        } else {
            CHANNEL_RATE
        };
        let ddc = if (input_rate - channel_rate).abs() < 1e-6 && freq_offset_hz.abs() < 1e-6 {
            None
        } else {
            Some(Ddc::new(input_rate, channel_rate, freq_offset_hz, CHANNEL_PASSBAND_HZ)?)
        };
        let selectivity = if ddc.is_none() {
            // -6 dB point at the symbol rate so the filter is flat
            // through the RC band edge (8.4 kHz) and the windowed-sinc
            // transition lives entirely in the noise-only region.
            let taps = xng_dsp::fir::lowpass_taps(demod::SYMBOL_RATE / channel_rate, 101);
            Some(xng_dsp::fir::Fir::new(taps))
        } else {
            None
        };
        Ok(Self {
            ddc,
            selectivity,
            select_buf: Vec::new(),
            demod: demod::Vdl2Demod::new(channel_rate),
            rs: interleave::vdl2_rs(),
            channel_buf: Vec::new(),
            x25: atn::X25Reassembler::new(),
            clnp: atn::ClnpReassembler::new(),
            cotp: atn::CotpReassembler::new(),
            samples_seen: 0,
            input_rate,
        })
    }

    pub fn process(&mut self, input: &[Complex<f32>]) -> Vec<Vdl2Frame> {
        self.samples_seen += input.len() as u64;
        let now = self.samples_seen as f64 / self.input_rate;
        let channel: &[Complex<f32>] = match &mut self.ddc {
            Some(ddc) => {
                self.channel_buf.clear();
                ddc.process(input, &mut self.channel_buf);
                &self.channel_buf
            }
            None => match &mut self.selectivity {
                Some(fir) => {
                    self.select_buf.clear();
                    fir.process(input, &mut self.select_buf);
                    &self.select_buf
                }
                None => input,
            },
        };
        let mut out = Vec::new();
        for burst in self.demod.process(channel, &self.rs) {
            for frame in avlc::scan(&burst.bits) {
                let acars = match frame.payload {
                    avlc::Payload::Acars => {
                        let start = frame.info.iter().position(|&b| b != 0xFF).unwrap_or(0);
                        xng_acars::block::parse(&frame.info[start..])
                    }
                    _ => None,
                };
                // ATN transport rides in I-frame info fields: an ISO 8208
                // packet (most links) or bare CLNP/ES-IS/IDRP.
                let atn = if acars.is_none()
                    && matches!(frame.control, avlc::Control::Info { .. })
                {
                    decode_atn(&frame.info, &mut self.x25, &mut self.clnp, &mut self.cotp, now)
                } else {
                    None
                };
                out.push(Vdl2Frame {
                    avlc: frame,
                    acars,
                    atn,
                    rs_corrected: burst.rs_corrected,
                    freq_skew_hz: burst.freq_skew_hz,
                });
            }
        }
        out
    }

    pub fn level_dbfs(&self) -> f32 {
        self.demod.level_dbfs()
    }

    /// Reject bursts whose carrier offset exceeds `ppm` (VDL2-7); `None`
    /// (default) accepts every CFO-fit candidate.
    pub fn set_max_ppm(&mut self, ppm: Option<f64>) {
        self.demod.set_max_ppm(ppm);
    }
}

/// Decode an I-frame information field as ATN transport.
fn decode_atn(
    info: &[u8],
    x25: &mut atn::X25Reassembler,
    clnp: &mut atn::ClnpReassembler,
    cotp: &mut atn::CotpReassembler,
    now: f64,
) -> Option<serde_json::Value> {
    if let Some(pkt) = atn::parse_x25(info) {
        let mut v = serde_json::to_value(&pkt).unwrap_or_default();
        v["layer"] = serde_json::json!("x25");
        if pkt.kind == "data" {
            if let Some(full) = x25.push(&pkt, now) {
                if let Some(net) = decode_network(&full, clnp, cotp, now) {
                    v["network"] = net;
                }
            } else {
                v["reassembling"] = serde_json::json!(true);
            }
        } else if !pkt.payload.is_empty() {
            // Call user data names the network protocol.
            if let Some(net) = decode_network(&pkt.payload, clnp, cotp, now) {
                v["network"] = net;
            }
        }
        return Some(v);
    }
    decode_network(info, clnp, cotp, now)
}

/// Parse an ATN network-layer payload, reassembling segmented CLNP data
/// units (ISO/IEC 8473 §6.7) before the full CLNP/COTP walk. A CLNP segment
/// that does not complete a data unit is reported as `reassembling` (its own
/// per-fragment CLNP header is still surfaced for visibility). Complete CLNP
/// DT PDUs additionally feed the COTP TSDU reassembler (ISO/IEC 8073 §6.6):
/// a multi-DT TSDU is decoded as one ATN-B1 application only once its EOT DT
/// arrives.
fn decode_network(
    b: &[u8],
    clnp: &mut atn::ClnpReassembler,
    cotp: &mut atn::CotpReassembler,
    now: f64,
) -> Option<serde_json::Value> {
    // Only CLNP (NLPID 0x81) is segmentable here; other protocols pass
    // straight through.
    if b.first() == Some(&0x81) {
        match clnp.push(b, now) {
            Some(full) => {
                let mut v = atn::parse_network(&full)?;
                cotp_reassemble(&full, &mut v, cotp, now);
                Some(v)
            }
            None => {
                // Incomplete data unit: surface this fragment's header plus a
                // reassembling marker.
                let mut v = atn::parse_network(b)?;
                v["reassembling"] = serde_json::json!(true);
                Some(v)
            }
        }
    } else {
        atn::parse_network(b)
    }
}

/// Feed a complete CLNP DT PDU's COTP TPDU to the TSDU reassembler (ISO/IEC
/// 8073 §6.6) and, when a multi-segment TSDU completes, decode the ATN-B1
/// application on the assembled user data and splice it into the COTP node.
/// Single-segment TSDUs are already decoded inline by [`atn::parse_cotp`];
/// here we only act on the reassembled (multi-DT) case, and mark a DT that is
/// still awaiting further segments.
fn cotp_reassemble(
    full: &[u8],
    v: &mut serde_json::Value,
    cotp: &mut atn::CotpReassembler,
    now: f64,
) {
    let Some(tpdu) = atn::clnp_cotp_tpdu(full) else { return };
    // Only DT TPDUs carry a segmentable TSDU.
    let Some((_, eot, seq, _)) = atn::cotp_dt_segment(tpdu) else { return };
    // A lone complete TSDU (seq 0, EOT) is handled inline already.
    let multi_segment = !(seq == 0 && eot);
    if !multi_segment {
        return;
    }
    match cotp.push(tpdu, now) {
        Some(tsdu) => {
            if let Some(app) = atn::parse_cotp_user_app(&tsdu) {
                if let Some(c) = v.get_mut("cotp") {
                    c["app"] = app;
                    c["tsdu_reassembled"] = serde_json::json!(true);
                    c["tsdu_len"] = serde_json::json!(tsdu.len());
                }
            }
        }
        None => {
            if let Some(c) = v.get_mut("cotp") {
                c["tsdu_reassembling"] = serde_json::json!(true);
            }
        }
    }
}

/// Convert a decoded frame into the normalized message model.
pub fn to_message(f: &Vdl2Frame, frequency_hz: u64, level_dbfs: f32, source: Provenance) -> Message {
    let (body, crc_ok, errors) = match &f.acars {
        Some(b) => (
            MessageBody::Acars(b.core.clone()),
            b.crc_ok,
            Some(b.parity_errors),
        ),
        None => (avlc_body(&f.avlc, f.atn.as_ref()), true, None),
    };
    Message {
        mode: Mode::Vdl2,
        timestamp: Utc::now(),
        frequency_hz,
        signal: SignalQuality {
            rssi_db: Some(level_dbfs),
            freq_skew_hz: Some(f.freq_skew_hz),
            ..Default::default()
        },
        decode: DecodeQuality {
            crc_ok,
            fec_corrected: Some(f.rs_corrected as u32),
            errors,
        },
        body,
        raw: Some(f.avlc.raw.clone()),
        source,
    }
}

/// Structured body for non-ACARS AVLC frames: the link layer is always
/// fully parsed (addresses, control), XID parameters are decoded, and
/// ATN payloads are at least labeled by protocol.
fn avlc_body(frame: &avlc::AvlcFrame, atn: Option<&serde_json::Value>) -> MessageBody {
    use avlc::{Control, Payload};
    let kind = match (&frame.control, &frame.payload) {
        (Control::Unnumbered { kind: "XID", .. }, _) => "xid".to_string(),
        (Control::Unnumbered { kind, .. }, _) => format!("avlc-{}", kind.to_lowercase()),
        (Control::Supervisory { kind, .. }, _) => format!("avlc-{}", kind.to_lowercase()),
        (Control::Info { .. }, Payload::Atn { .. }) => "atn".to_string(),
        (Control::Info { .. }, _) => "avlc-i".to_string(),
    };
    let mut details = serde_json::json!({
        "dst": frame.dst,
        "src": frame.src,
        "control": frame.control,
    });
    if let Payload::Atn { ipi } = frame.payload {
        details["protocol"] = serde_json::json!(match ipi {
            0x81 => "CLNP",
            0x82 => "ES-IS",
            _ => "IDRP",
        });
    }
    if let Some(a) = atn {
        details["atn"] = a.clone();
    }
    if matches!(frame.control, Control::Unnumbered { kind: "XID", .. }) {
        if let Some(params) = avlc::parse_xid(&frame.info) {
            details["params"] = serde_json::json!(params);
        }
    }
    if matches!(frame.control, Control::Unnumbered { kind: "FRMR", .. }) {
        if let Some(frmr) = avlc::parse_frmr(&frame.info) {
            details["frmr"] = serde_json::json!(frmr);
        }
    }
    if !frame.info.is_empty() {
        let shown = &frame.info[..frame.info.len().min(64)];
        details["info_hex"] =
            serde_json::json!(shown.iter().map(|b| format!("{b:02x}")).collect::<String>());
        details["info_len"] = serde_json::json!(frame.info.len());
    }
    MessageBody::Vdl2 { kind, details }
}

#[cfg(test)]
mod cotp_pipeline_tests {
    use super::*;

    /// Wrap a COTP TPDU in an unsegmented CLNP DT PDU (dst NSAP 47 01, src
    /// 47 02). Header = 9 fixed + 3 + 3 = 15 octets.
    fn clnp_dt(cotp: &[u8]) -> Vec<u8> {
        let mut b = vec![0x81, 15, 1, 0x3F, 0x1C, 0x00, 0x00, 0x00, 0x00];
        b.extend_from_slice(&[2, 0x47, 0x01]);
        b.extend_from_slice(&[2, 0x47, 0x02]);
        b.extend_from_slice(cotp);
        b
    }

    /// Normal-format COTP DT: LI=4, code 0xF0, dst_ref, EOT|seq, user data.
    fn cotp_dt(dst_ref: u16, eot: bool, seq: u8, user: &[u8]) -> Vec<u8> {
        let mut b = vec![0x04, 0xF0];
        b.extend_from_slice(&dst_ref.to_be_bytes());
        b.push(if eot { 0x80 } else { 0x00 } | (seq & 0x7F));
        b.extend_from_slice(user);
        b
    }

    #[test]
    fn cpdlc_reassembled_across_two_cotp_dt_segments() {
        // A protected-mode downlink WILCO whose user data is split across two
        // COTP DT TPDUs (seq 0 EOT=0, seq 1 EOT=1) on one connection. The
        // first segment is reported as a TSDU fragment; the second completes
        // the TSDU, which decodes as a CPDLC WILCO.
        let apdu = atn_cpdlc::build_downlink_wilco_for_test();
        assert!(apdu.len() >= 2, "need at least two octets to split");
        let split = apdu.len() / 2;
        let s0 = cotp_dt(0x0001, false, 0, &apdu[..split]);
        let s1 = cotp_dt(0x0001, true, 1, &apdu[split..]);

        let mut clnp = atn::ClnpReassembler::new();
        let mut cotp = atn::CotpReassembler::new();

        // First CLNP DT (carrying COTP segment 0): TSDU still reassembling.
        let v0 = decode_network(&clnp_dt(&s0), &mut clnp, &mut cotp, 0.0).unwrap();
        assert_eq!(v0["cotp"]["tpdu"], "DT");
        assert_eq!(v0["cotp"]["tsdu_segment"], true);
        assert!(v0["cotp"].get("app").is_none());
        assert_eq!(v0["cotp"]["tsdu_reassembling"], true);

        // Second CLNP DT (COTP segment 1, EOT): TSDU completes → CPDLC app.
        let v1 = decode_network(&clnp_dt(&s1), &mut clnp, &mut cotp, 1.0).unwrap();
        assert_eq!(v1["cotp"]["tsdu_reassembled"], true);
        let app = &v1["cotp"]["app"];
        assert_eq!(app["application"], "CPDLC");
        assert_eq!(app["pdu"], "send");
        assert_eq!(app["message"]["elements"][0]["element"], "dM0NULL");
    }

    #[test]
    fn single_segment_cotp_still_decodes_inline() {
        // A single complete DT (EOT, seq 0) still decodes the app inline,
        // unchanged from the pre-reassembly behaviour.
        let apdu = atn_cpdlc::build_downlink_wilco_for_test();
        let dt = cotp_dt(0x0002, true, 0, &apdu);
        let mut clnp = atn::ClnpReassembler::new();
        let mut cotp = atn::CotpReassembler::new();
        let v = decode_network(&clnp_dt(&dt), &mut clnp, &mut cotp, 0.0).unwrap();
        assert_eq!(v["cotp"]["app"]["pdu"], "send");
        assert!(v["cotp"].get("tsdu_reassembled").is_none());
    }
}
