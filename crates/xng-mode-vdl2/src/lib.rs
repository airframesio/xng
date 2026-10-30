//! Native VDL Mode 2 decode core.
//!
//! Pipeline per channel: wideband IQ → [`xng_dsp::Ddc`] → 50 kHz channel
//! IQ → [`demod::Vdl2Demod`] (D8PSK burst acquisition, header,
//! deinterleave + RS(255,249)) → [`avlc`] frame scan → ACARS-over-AVLC via
//! [`xng_acars::block`] → [`xng_types::Message`].
//! [`Vdl2MultiChannelDecoder`] replaces the per-channel DDCs with one shared
//! front end feeding the same per-channel back end.
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
use xng_dsp::{ChannelizedDdc, Ddc, Oversample, SharedDdc};
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
    /// EVM-derived per-burst SNR (dB) from the symbol decision residuals (VDL2-8).
    pub snr_db: f32,
}

/// Preferred channel rate (an exact 10 samples/symbol), used when the capture
/// rate is an integer multiple of it.
const CHANNEL_RATE_NATIVE: f64 = 105_000.0;

/// Pick the internal channel rate for a capture rate.
///
/// Prefer 105 kS/s (an exact 10 samples/symbol) when the input divides into
/// it: at 100 kS/s every symbol center falls at a fractional sample position
/// and the linear interpolator's error acts as decision noise; integer sps
/// removes it. Otherwise prefer the high channel rate when the capture divides
/// into it; 50 kS/s remains the floor (and the vendored-fixture rate).
fn channel_rate_for(input_rate: f64) -> f64 {
    if input_rate >= CHANNEL_RATE_NATIVE
        && (input_rate / CHANNEL_RATE_NATIVE).fract().abs() < 1e-9
    {
        CHANNEL_RATE_NATIVE
    } else if input_rate >= CHANNEL_RATE_HI
        && (input_rate / CHANNEL_RATE_HI).fract().abs() < 1e-9
    {
        CHANNEL_RATE_HI
    } else {
        CHANNEL_RATE
    }
}

/// Per-channel back end: everything downstream of the front-end
/// downconverter (channel filter, demod, AVLC scan, ACARS/ATN decode). Shared
/// verbatim by the single- and multi-channel decoders so the two paths cannot
/// drift.
struct ChannelBack {
    /// Channel-selectivity lowpass at the channel rate. The RC(α=0.6)
    /// TX pulse occupies ±(1+α)·Rs/2 = ±8.4 kHz; a flat lowpass there
    /// keeps the signal spectrum (and its Nyquist zero-ISI property)
    /// untouched while removing all out-of-band noise.
    selectivity: xng_dsp::fir::Fir,
    select_buf: Vec<Complex<f32>>,
    demod: demod::Vdl2Demod,
    rs: ReedSolomon,
    x25: atn::X25Reassembler,
    clnp: atn::ClnpReassembler,
    cotp: atn::CotpReassembler,
}

impl ChannelBack {
    fn new(channel_rate: f64) -> Self {
        // -6 dB point at the symbol rate so the filter is flat through the
        // RC band edge (8.4 kHz) and the windowed-sinc transition lives
        // entirely in the noise-only region. This runs after the front end
        // too: the DDC's final stage is sized as an anti-alias filter, not a
        // channel filter (≈−2 dB at 12 kHz for 2.4 MS/s → 100 kS/s), so
        // this FIR is the channel filter on every path. On the live path
        // it moved the 50%-decode SNR from 20.8 to 18.6 dB.
        let selectivity = xng_dsp::fir::Fir::new(xng_dsp::fir::lowpass_taps(
            demod::SYMBOL_RATE / channel_rate,
            101,
        ));
        Self {
            selectivity,
            select_buf: Vec::new(),
            demod: demod::Vdl2Demod::new(channel_rate),
            rs: interleave::vdl2_rs(),
            x25: atn::X25Reassembler::new(),
            clnp: atn::ClnpReassembler::new(),
            cotp: atn::CotpReassembler::new(),
        }
    }

    /// Feed channel-rate IQ; `now` is the wideband timebase (seconds of
    /// capture consumed) used by the ATN reassemblers' timeouts.
    fn process(&mut self, channel: &[Complex<f32>], now: f64) -> Vec<Vdl2Frame> {
        self.select_buf.clear();
        self.selectivity.process(channel, &mut self.select_buf);
        let mut out = Vec::new();
        for burst in self.demod.process(&self.select_buf, &self.rs) {
            for frame in burst.frames {
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
                    snr_db: burst.snr_db,
                });
            }
        }
        out
    }
}

pub struct Vdl2ChannelDecoder {
    ddc: Option<Ddc>,
    channel_buf: Vec<Complex<f32>>,
    back: ChannelBack,
    samples_seen: u64,
    input_rate: f64,
    channel_rate: f64,
}

impl Vdl2ChannelDecoder {
    pub fn new(input_rate: f64, freq_offset_hz: f64) -> Result<Self, String> {
        let channel_rate = channel_rate_for(input_rate);
        let ddc = if (input_rate - channel_rate).abs() < 1e-6 && freq_offset_hz.abs() < 1e-6 {
            None
        } else {
            Some(Ddc::new(input_rate, channel_rate, freq_offset_hz, CHANNEL_PASSBAND_HZ)?)
        };
        Ok(Self {
            ddc,
            channel_buf: Vec::new(),
            back: ChannelBack::new(channel_rate),
            samples_seen: 0,
            input_rate,
            channel_rate,
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
            None => input,
        };
        self.back.process(channel, now)
    }

    pub fn level_dbfs(&self) -> f32 {
        self.back.demod.level_dbfs()
    }

    /// Reject bursts whose carrier offset exceeds `ppm` (VDL2-7); `None`
    /// (default) accepts every CFO-fit candidate.
    pub fn set_max_ppm(&mut self, ppm: Option<f64>) {
        self.back.demod.set_max_ppm(ppm);
    }

    /// Internal channel rate the demod runs at (S/s).
    pub fn channel_rate(&self) -> f64 {
        self.channel_rate
    }
}

/// One-sided passband the channelizer front end must keep clear of its bin
/// edge. VDL2's channel filter is the per-channel selectivity FIR in the back
/// end, so the front end is only an anti-alias stage around the RC band edge
/// (±8.4 kHz); the extra 2.1 kHz keeps that edge off the bin edge. The
/// channelizer's residual-vs-bin-edge rule and its anti-alias stage are both
/// sized from it. Passing that rule does not make a critically sampled bin
/// flat there: a channel 37.5 kHz off a 100 kHz bin center still sits in the
/// prototype's roll-off; the 2× bank the default uses is flat there (see
/// [`Vdl2MultiChannelDecoder::new`]).
pub const CHANNELIZER_PASSBAND_HZ: f64 = 10_500.0;

/// One-sided passband for the shared-decimation front end at `channel_rate`.
///
/// `SharedDdc` realizes its passband with a per-channel "sharp" FIR at the
/// intermediate rate whose length scales with 1/passband; a narrow passband
/// there would duplicate the back end's selectivity FIR at far higher cost. A
/// wide 0.4·rate (40 kHz at 100 kS/s; `SharedDdc` needs rate ≥ 2·passband)
/// keeps it a short anti-alias stage and leaves the channel filtering to the
/// back end.
pub fn shared_front_passband_hz(channel_rate: f64) -> f64 {
    0.4 * channel_rate
}

/// Shared multi-channel downconverter front end (see
/// [`Vdl2MultiChannelDecoder`]).
enum Front {
    Channelized(ChannelizedDdc),
    Shared(SharedDdc),
}

impl Front {
    fn process(&mut self, input: &[Complex<f32>], out: &mut [Vec<Complex<f32>>]) {
        match self {
            Self::Channelized(c) => c.process(input, out),
            Self::Shared(s) => s.process(input, out),
        }
    }
}

/// Decodes SEVERAL VDL2 channels out of one wideband capture with a single
/// shared downconverter front end: the wideband-rate work is done once for all
/// channels, then each channel runs the same back end as
/// [`Vdl2ChannelDecoder`] (selectivity FIR, demod, AVLC, ACARS/ATN).
///
/// Two front ends with the same contract:
/// * a polyphase channelizer ([`xng_dsp::ChannelizedDdc`]): one FFT pass
///   produces every channel; the default, 2× oversampled (see [`Self::new`]).
/// * a shared decimation ([`xng_dsp::SharedDdc`]): one full-rate decimation
///   feeds cheap per-channel finishes; the fallback.
pub struct Vdl2MultiChannelDecoder {
    front: Front,
    backs: Vec<ChannelBack>,
    channel_bufs: Vec<Vec<Complex<f32>>>,
    samples_seen: u64,
    input_rate: f64,
    channel_rate: f64,
}

impl Vdl2MultiChannelDecoder {
    /// `input_rate` is the wideband capture rate; `offsets` are each channel's
    /// center relative to the capture center (Hz).
    ///
    /// Uses the 2× oversampled channelizer front end ([`Oversample::Twice`]),
    /// falling back to the shared decimation if the channelizer cannot be
    /// built for this rate/offset set.
    ///
    /// Not the critically sampled channelizer: its bins must be at least the
    /// 100 kS/s channel rate, so on the 25 kHz VDL2 raster (e.g. the 7-channel
    /// plan around 136.8125 MHz) four of seven channels sit 37.5 kHz off a
    /// 100 kHz bin center, in the prototype's roll-off, and it decoded 35 of
    /// the 44 off-air frames the shared decimation gets. The 2× bank is flat
    /// wherever a channel falls in its bin: on that plan it matched the shared
    /// decimation (44/44 off-air at every tuning offset, the same 50%-decode
    /// SNR) at about half its CPU (PERF-20).
    pub fn new(input_rate: f64, offsets: &[f64]) -> Result<Self, String> {
        let channel_rate = channel_rate_for(input_rate);
        let front = match ChannelizedDdc::with_oversample(
            input_rate,
            channel_rate,
            offsets,
            CHANNELIZER_PASSBAND_HZ,
            Oversample::Twice,
        ) {
            Ok(c) => Front::Channelized(c),
            // On double failure report BOTH errors so an unusual capture
            // config shows why the default channelizer was rejected too.
            Err(chan_err) => Front::Shared(
                SharedDdc::new(
                    input_rate,
                    channel_rate,
                    offsets,
                    shared_front_passband_hz(channel_rate),
                )
                .map_err(|shared_err| {
                    format!(
                        "channelizer rejected ({chan_err}); shared-decim also failed ({shared_err})"
                    )
                })?,
            ),
        };
        Ok(Self::with_front(front, input_rate, channel_rate, offsets.len()))
    }

    /// Build with the shared-decimation front end explicitly (no fallback).
    pub fn new_shared(input_rate: f64, offsets: &[f64]) -> Result<Self, String> {
        let channel_rate = channel_rate_for(input_rate);
        let front = Front::Shared(SharedDdc::new(
            input_rate,
            channel_rate,
            offsets,
            shared_front_passband_hz(channel_rate),
        )?);
        Ok(Self::with_front(front, input_rate, channel_rate, offsets.len()))
    }

    /// Build with the critically sampled channelizer front end explicitly
    /// (no fallback).
    pub fn new_channelized(input_rate: f64, offsets: &[f64]) -> Result<Self, String> {
        Self::new_channelized_with_oversample(input_rate, offsets, Oversample::Critical)
    }

    /// Build with the channelizer front end explicitly (no fallback), with
    /// the bank's oversampling chosen: [`Oversample::Twice`] serves every
    /// channel flat wherever it falls in its bin (no roll-off for channels
    /// off a bin center) at about twice the shared bank cost.
    pub fn new_channelized_with_oversample(
        input_rate: f64,
        offsets: &[f64],
        oversample: Oversample,
    ) -> Result<Self, String> {
        let channel_rate = channel_rate_for(input_rate);
        let front = Front::Channelized(ChannelizedDdc::with_oversample(
            input_rate,
            channel_rate,
            offsets,
            CHANNELIZER_PASSBAND_HZ,
            oversample,
        )?);
        Ok(Self::with_front(front, input_rate, channel_rate, offsets.len()))
    }

    fn with_front(front: Front, input_rate: f64, channel_rate: f64, channels: usize) -> Self {
        Self {
            front,
            backs: (0..channels).map(|_| ChannelBack::new(channel_rate)).collect(),
            channel_bufs: vec![Vec::new(); channels],
            samples_seen: 0,
            input_rate,
            channel_rate,
        }
    }

    /// Number of channels.
    pub fn num_channels(&self) -> usize {
        self.backs.len()
    }

    /// Which front end was built, for benches, logs and tests:
    /// `"channelized <bank geometry>"` or `"shared"`.
    pub fn describe_front(&self) -> String {
        match &self.front {
            Front::Channelized(c) => format!("channelized {}", c.describe()),
            Front::Shared(_) => "shared".to_string(),
        }
    }

    /// Internal channel rate the demods run at (S/s).
    pub fn channel_rate(&self) -> f64 {
        self.channel_rate
    }

    /// Feed wideband IQ; returns the decoded frames for channel `i` as
    /// `(i, frames)` for every channel that produced any, in ascending `i`.
    pub fn process(&mut self, input: &[Complex<f32>]) -> Vec<(usize, Vec<Vdl2Frame>)> {
        self.samples_seen += input.len() as u64;
        let now = self.samples_seen as f64 / self.input_rate;
        self.front.process(input, &mut self.channel_bufs);
        let mut out = Vec::new();
        for (i, (back, buf)) in self.backs.iter_mut().zip(&self.channel_bufs).enumerate() {
            let frames = back.process(buf, now);
            if !frames.is_empty() {
                out.push((i, frames));
            }
        }
        out
    }

    /// Demod level (dBFS) for channel `i`.
    pub fn level_dbfs(&self, i: usize) -> f32 {
        self.backs[i].demod.level_dbfs()
    }

    /// Reject bursts whose carrier offset exceeds `ppm` on every channel
    /// (VDL2-7); `None` (default) accepts every CFO-fit candidate.
    pub fn set_max_ppm(&mut self, ppm: Option<f64>) {
        for b in &mut self.backs {
            b.demod.set_max_ppm(ppm);
        }
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
        Some(b) => {
            // ACARS-over-AVLC: the body is the shared ACARS core, but the AVLC
            // link wrapper (src/dst/control) would otherwise be lost — the
            // dumpvdl2-JSON feed (FEED-2.1) needs it. Carry it on the
            // `#[serde(skip)]` transient `vdl2_link` field, NOT inside `app`:
            // `app` is serialized into every public output (JSONL/MQTT/asf-2.0)
            // and is overwritten wholesale by the reassembler on completion, so
            // stashing link data there both leaked internal plumbing and got
            // destroyed on multi-block reassembly.
            let mut core = b.core.clone();
            core.vdl2_link = Some(serde_json::json!({
                "src": f.avlc.src, "dst": f.avlc.dst, "control": f.avlc.control,
            }));
            (MessageBody::Acars(core), b.crc_ok, Some(b.parity_errors))
        }
        None => (avlc_body(&f.avlc, f.atn.as_ref()), true, None),
    };
    Message {
        mode: Mode::Vdl2,
        timestamp: Utc::now(),
        frequency_hz,
        signal: SignalQuality {
            rssi_db: Some(level_dbfs),
            snr_db: Some(f.snr_db),
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
