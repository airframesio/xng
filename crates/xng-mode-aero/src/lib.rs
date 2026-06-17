//! Native Inmarsat Classic Aero decode core (L-band P-channel, 600 and
//! 1200 bps), ported from MIT-licensed JAERO (see PROVENANCE.md).
//!
//! Pipeline per channel: wideband IQ → [`xng_dsp::Ddc`] → 24 kHz channel
//! IQ → [`demod::MskDemod`] (both rates run in parallel; whichever locks
//! wins) → UW framing → [`frame::FrameDecoder`] (deinterleave, Viterbi,
//! descramble) → 12-byte SUs → [`su::Reassembler`] → ACARS via
//! [`xng_acars::block`] → [`xng_types::Message`].

pub mod burst;
pub mod cchannel;
pub mod demod;
pub mod oqpsk;
pub mod frame;
pub mod modulate;
pub mod satellite;
pub mod su;

use chrono::Utc;
use num_complex::Complex;
use serde::Serialize;
use xng_dsp::Ddc;
use xng_types::{DecodeQuality, Message, MessageBody, Mode, Provenance, SignalQuality};

pub const CHANNEL_RATE: f64 = 24_000.0;
/// One-sided passband: MSK at ≤1200 bps occupies well under ±2 kHz.
pub const CHANNEL_PASSBAND_HZ: f64 = 2_500.0;

/// Which logical Inmarsat-Aero channel an event came from. JAERO models
/// these as distinct physical channels (`AeroL::ChannelType {PChannel,
/// RChannel, TChannel}`): P is the GES→AES TDM forward channel (L-band,
/// `AeroChannelDecoder`); R is the AES→GES random-access return channel
/// and T the AES→GES reserved/TDMA return channel (both reach the GES via
/// the C-band feeder bursts, `AeroBurstDecoder`). (AERO-8.2.)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum AeroChannel {
    PChannel,
    RChannel,
    TChannel,
}

impl AeroChannel {
    /// The channel tag emitted into `MessageBody::Aero` details.
    pub fn tag(self) -> &'static str {
        match self {
            AeroChannel::PChannel => "p-channel",
            AeroChannel::RChannel => "r-channel",
            AeroChannel::TChannel => "t-channel",
        }
    }
}

/// A decoded Aero event: reassembled user data, ACARS when parseable.
pub struct AeroEvent {
    pub user: su::AeroUserData,
    pub acars: Option<xng_acars::block::AcarsBlock>,
    pub bit_rate: u32,
    /// Set when this event is a structured (non-user-data) P-channel SU:
    /// a C-channel/T-channel assignment, a log-on/log-off control event, a
    /// call announcement, or an AES system-table broadcast. The value's
    /// `su_type` field names the SU (see [`su::parse_p_su`]).
    pub su_event: Option<serde_json::Value>,
    /// Physical channel this event came from. L-band P-channel events are
    /// `Mode::AeroL`; C-band feeder R/T bursts are `Mode::AeroC`. Carried
    /// per-event so [`to_message`] tags the message with the correct mode
    /// instead of hard-coding one (AERO-8.1).
    pub mode: Mode,
    /// Logical Aero channel (P/R/T) — surfaced alongside `bit_rate` in the
    /// emitted message so consumers can distinguish forward (P) from the
    /// random-access (R) and reserved (T) return channels (AERO-8.2).
    pub channel: AeroChannel,
    /// Parsed 16-bit P-channel frame header of the frame that carried this
    /// event, when available (AERO-4). Only the L-band P-channel framer
    /// (`AeroChannelDecoder`) sets this; burst/C-band paths leave it `None`.
    pub frame_header: Option<frame::FrameHeader>,
    /// Resolved satellite/beam annotation for this event, when a system-
    /// table broadcast has been observed on the channel (AERO-2). Already a
    /// JSON object ready to merge into the message `details`.
    pub satellite: Option<serde_json::Value>,
}

/// One demod + framing chain at a fixed bit rate.
struct RateChain {
    rate: u32,
    demod: demod::MskDemod,
    framer: Framer,
    bits: Vec<(f32, u8)>,
}

/// UW hunt + frame assembly state.
struct Framer {
    decoder: frame::FrameDecoder,
    shift: u32,
    /// When collecting: soft bits gathered after the UW (header + coded).
    collecting: Option<Vec<f32>>,
    reasm: su::Reassembler,
    /// Structured (non-user-data) P-channel SUs decoded this push, drained
    /// per chunk (assignments, log-on/log-off control, call announcements,
    /// system-table broadcasts — see [`su::parse_p_su`]).
    su_events: Vec<serde_json::Value>,
    /// Parsed header of the most recently assembled frame (AERO-4). Latched
    /// so events surfaced from this frame carry it.
    last_header: Option<frame::FrameHeader>,
    /// Self-configuring satellite/beam resolver, fed every structured SU it
    /// sees (AERO-2). Latches the most recent satellite from the system-
    /// table broadcasts.
    resolver: satellite::SatelliteResolver,
}

impl Framer {
    fn new(rate: u32) -> Self {
        Self {
            decoder: frame::FrameDecoder::new(rate),
            shift: 0,
            collecting: None,
            reasm: su::Reassembler::new(),
            su_events: Vec::new(),
            last_header: None,
            resolver: satellite::SatelliteResolver::new(),
        }
    }

    fn push(&mut self, soft: f32, hard: u8, out: &mut Vec<su::AeroUserData>) {
        if let Some(buf) = &mut self.collecting {
            buf.push(soft);
            if buf.len() == frame::HEADER_BITS + frame::CODED_BITS {
                // Parse the 16-bit frame header (AERO-4) before the coded
                // payload, then decode the SUs.
                self.last_header =
                    Some(frame::FrameHeader::from_soft_bits(&buf[..frame::HEADER_BITS]));
                let coded = &buf[frame::HEADER_BITS..];
                let bytes = self.decoder.decode(coded);
                for su_bytes in bytes.chunks_exact(su::SU_LEN) {
                    if su::su_crc_ok(su_bytes) {
                        if let Some(a) = su::parse_p_su(su_bytes) {
                            // Feed the resolver every structured SU so the
                            // system-table broadcasts (0x0C/0x07/0x05)
                            // keep the satellite/beam state current (AERO-2).
                            self.resolver.observe(&a);
                            self.su_events.push(a);
                        }
                        if let Some(u) = self.reasm.push(su_bytes) {
                            out.push(u);
                        }
                    }
                }
                self.collecting = None;
            }
            // Keep the shift register warm so back-to-back frames hunt
            // immediately (UW of the next frame follows directly).
        }
        self.shift = (self.shift << 1) | hard as u32;
        // Tolerate a couple of UW bit errors (off-air bits are not clean;
        // a false trigger costs one frame and dies at the SU CRCs).
        if self.collecting.is_none() && (self.shift ^ frame::UW).count_ones() <= 2 {
            self.collecting = Some(Vec::with_capacity(frame::HEADER_BITS + frame::CODED_BITS));
        }
    }
}

pub struct AeroChannelDecoder {
    ddc: Option<Ddc>,
    chains: Vec<RateChain>,
    channel_buf: Vec<Complex<f32>>,
    /// 10.5 kbps OQPSK chain on its own 48 kHz channel (only when the
    /// input rate can carry it).
    hr: Option<HrChain>,
}

struct HrChain {
    ddc: Option<Ddc>,
    demod: oqpsk::OqpskDemod,
    framer: oqpsk::HrFramer,
    buf: Vec<Complex<f32>>,
    bits: Vec<(f32, u8)>,
}

impl AeroChannelDecoder {
    pub fn new(input_rate: f64, freq_offset_hz: f64) -> Result<Self, String> {
        let ddc = if (input_rate - CHANNEL_RATE).abs() < 1e-6 && freq_offset_hz.abs() < 1e-6 {
            None
        } else {
            Some(Ddc::new(input_rate, CHANNEL_RATE, freq_offset_hz, CHANNEL_PASSBAND_HZ)?)
        };
        let chains = [600u32, 1200]
            .iter()
            .map(|&rate| RateChain {
                rate,
                demod: demod::MskDemod::new(CHANNEL_RATE, rate as f64),
                framer: Framer::new(rate),
                bits: Vec::new(),
            })
            .collect();
        let hr = if (input_rate - oqpsk::CHANNEL_RATE_HR).abs() < 1e-6
            && freq_offset_hz.abs() < 1e-6
        {
            Some(HrChain {
                ddc: None,
                demod: oqpsk::OqpskDemod::new(oqpsk::CHANNEL_RATE_HR),
                framer: oqpsk::HrFramer::new(),
                buf: Vec::new(),
                bits: Vec::new(),
            })
        } else if input_rate >= oqpsk::CHANNEL_RATE_HR {
            // Ignore an hr-incapable rate rather than failing the mode.
            Ddc::new(input_rate, oqpsk::CHANNEL_RATE_HR, freq_offset_hz, 6_500.0)
                .ok()
                .map(|ddc| HrChain {
                    ddc: Some(ddc),
                    demod: oqpsk::OqpskDemod::new(oqpsk::CHANNEL_RATE_HR),
                    framer: oqpsk::HrFramer::new(),
                    buf: Vec::new(),
                    bits: Vec::new(),
                })
        } else {
            None
        };
        Ok(Self { ddc, chains, channel_buf: Vec::new(), hr })
    }

    pub fn process(&mut self, input: &[Complex<f32>]) -> Vec<AeroEvent> {
        let channel: &[Complex<f32>] = match &mut self.ddc {
            Some(ddc) => {
                self.channel_buf.clear();
                ddc.process(input, &mut self.channel_buf);
                &self.channel_buf
            }
            None => input,
        };
        let mut out = Vec::new();
        for chain in &mut self.chains {
            chain.bits.clear();
            chain.demod.process(channel, &mut chain.bits);
            let mut users = Vec::new();
            for &(soft, hard) in &chain.bits {
                chain.framer.push(soft, hard, &mut users);
            }
            // The frame header (AERO-4) and resolved satellite (AERO-2) are
            // latched in the framer; tag every event from this chain with
            // the current state.
            let frame_header = chain.framer.last_header;
            let satellite = chain.framer.resolver.details();
            for user in users {
                let acars = su::parse_acars(&user.data);
                out.push(AeroEvent {
                    user,
                    acars,
                    bit_rate: chain.rate,
                    su_event: None,
                    mode: Mode::AeroL,
                    channel: AeroChannel::PChannel,
                    frame_header,
                    satellite: satellite.clone(),
                });
            }
            for a in chain.framer.su_events.drain(..) {
                let mut e = su_event_msg(a, chain.rate, Mode::AeroL, AeroChannel::PChannel);
                e.frame_header = frame_header;
                e.satellite = satellite.clone();
                out.push(e);
            }
        }

        // 10.5 kbps OQPSK chain.
        if let Some(hr) = &mut self.hr {
            let hr_channel: &[Complex<f32>] = match &mut hr.ddc {
                Some(ddc) => {
                    hr.buf.clear();
                    ddc.process(input, &mut hr.buf);
                    &hr.buf
                }
                None => input,
            };
            hr.bits.clear();
            hr.demod.process(hr_channel, &mut hr.bits);
            let mut hr_users = Vec::new();
            for &(soft, hard) in &hr.bits {
                hr.framer.push(soft, hard, &mut hr_users);
            }
            let frame_header = hr.framer.last_header;
            let satellite = hr.framer.resolver.details();
            for user in hr_users {
                let acars = su::parse_acars(&user.data);
                out.push(AeroEvent {
                    user,
                    acars,
                    bit_rate: oqpsk::BIT_RATE,
                    su_event: None,
                    mode: Mode::AeroL,
                    channel: AeroChannel::PChannel,
                    frame_header,
                    satellite: satellite.clone(),
                });
            }
            for a in hr.framer.su_events.drain(..) {
                let mut e = su_event_msg(a, oqpsk::BIT_RATE, Mode::AeroL, AeroChannel::PChannel);
                e.frame_header = frame_header;
                e.satellite = satellite.clone();
                out.push(e);
            }
        }
        out
    }

    pub fn level_dbfs(&self) -> f32 {
        self.chains[0].demod.level_dbfs()
    }
}

/// C-band R/T burst decoder (both rates tried per burst).
pub struct AeroBurstDecoder {
    ddc: Option<Ddc>,
    gate: burst::BurstGate,
    packetizers: [burst::BurstPacketizer; 2],
    channel_buf: Vec<Complex<f32>>,
    level: f32,
    /// Self-configuring satellite/beam resolver (AERO-2): T-burst P-style
    /// SUs can carry system-table broadcasts, so resolve from them too.
    resolver: satellite::SatelliteResolver,
}

impl AeroBurstDecoder {
    pub fn new(input_rate: f64, freq_offset_hz: f64) -> Result<Self, String> {
        let ddc = if (input_rate - CHANNEL_RATE).abs() < 1e-6 && freq_offset_hz.abs() < 1e-6 {
            None
        } else {
            Some(Ddc::new(input_rate, CHANNEL_RATE, freq_offset_hz, CHANNEL_PASSBAND_HZ)?)
        };
        Ok(Self {
            ddc,
            // Longest plausible burst: a few seconds at 24 kHz.
            gate: burst::BurstGate::new(4 * CHANNEL_RATE as usize),
            packetizers: [burst::BurstPacketizer::new(), burst::BurstPacketizer::new()],
            channel_buf: Vec::new(),
            level: 0.0,
            resolver: satellite::SatelliteResolver::new(),
        })
    }

    pub fn process(&mut self, input: &[Complex<f32>]) -> Vec<AeroEvent> {
        let channel: &[Complex<f32>] = match &mut self.ddc {
            Some(ddc) => {
                self.channel_buf.clear();
                ddc.process(input, &mut self.channel_buf);
                &self.channel_buf
            }
            None => input,
        };
        let mut out = Vec::new();
        for b in self.gate.process(channel) {
            self.level = b.iter().map(|x| x.norm_sqr()).sum::<f32>() / b.len() as f32;
            for (i, &rate) in [600u32, 1200].iter().enumerate() {
                let bits = burst::demod_burst(&b, CHANNEL_RATE, rate as f64);
                if let Some(result) = self.packetizers[i].process(&bits) {
                    // The C-band feeder burst is either a reserved/TDMA T
                    // burst or a random-access R burst (AERO-8.2).
                    let aero_channel = if result.is_t {
                        AeroChannel::TChannel
                    } else {
                        AeroChannel::RChannel
                    };
                    // T-burst P-style SUs can carry system-table broadcasts;
                    // feed them to the resolver before emitting (AERO-2).
                    for a in &result.su_events {
                        self.resolver.observe(a);
                    }
                    let satellite = self.resolver.details();
                    for user in result.users {
                        let acars = su::parse_acars(&user.data);
                        out.push(AeroEvent {
                            user,
                            acars,
                            bit_rate: rate,
                            su_event: None,
                            mode: Mode::AeroC,
                            channel: aero_channel,
                            frame_header: None,
                            satellite: satellite.clone(),
                        });
                    }
                    // Named control/signalling SUs (R access-request /
                    // call-progress / telephony-ack / RQA / ACK, or T-burst
                    // P-style control SUs) carry the burst's bit rate and
                    // the resolved channel tag (AERO-3 / AERO-8.2).
                    for a in result.su_events {
                        let mut e = su_event_msg(a, rate, Mode::AeroC, aero_channel);
                        e.satellite = satellite.clone();
                        out.push(e);
                    }
                    break; // one rate decoded this burst
                }
            }
        }
        out
    }

    pub fn level_dbfs(&self) -> f32 {
        10.0 * self.level.max(1e-12).log10()
    }
}

/// C-channel decoder: IQ (48 kHz, or DDC'd down from wideband) →
/// 8 400 bps OQPSK demod → deframer → voice frames and sub-band SUs.
/// C-channel circuits are call-assigned via P-channel setup, so the
/// frequency comes from the operator, not a scan plan.
pub struct CChannelDecoder {
    ddc: Option<Ddc>,
    demod: oqpsk::OqpskDemod,
    deframer: cchannel::CChannelDeframer,
    channel_buf: Vec<Complex<f32>>,
    soft: Vec<(f32, u8)>,
}

impl CChannelDecoder {
    pub fn new(input_rate: f64, freq_offset_hz: f64) -> Result<Self, String> {
        let ddc = if (input_rate - oqpsk::CHANNEL_RATE_HR).abs() < 1e-6
            && freq_offset_hz.abs() < 1e-6
        {
            None
        } else {
            Some(Ddc::new(
                input_rate,
                oqpsk::CHANNEL_RATE_HR,
                freq_offset_hz,
                8_400.0,
            )?)
        };
        Ok(Self {
            ddc,
            demod: oqpsk::OqpskDemod::new_c_channel(oqpsk::CHANNEL_RATE_HR),
            deframer: cchannel::CChannelDeframer::new(),
            channel_buf: Vec::new(),
            soft: Vec::new(),
        })
    }

    pub fn process(&mut self, input: &[Complex<f32>]) -> Vec<cchannel::CChannelEvent> {
        let channel: &[Complex<f32>] = match &mut self.ddc {
            Some(ddc) => {
                self.channel_buf.clear();
                ddc.process(input, &mut self.channel_buf);
                &self.channel_buf
            }
            None => input,
        };
        self.soft.clear();
        self.demod.process(channel, &mut self.soft);
        let mut out = Vec::new();
        for &(s, _) in &self.soft {
            out.extend(self.deframer.push(s));
        }
        out
    }

    pub fn level_dbfs(&self) -> f32 {
        self.demod.level_dbfs()
    }
}

fn su_event_msg(a: serde_json::Value, bit_rate: u32, mode: Mode, channel: AeroChannel) -> AeroEvent {
    let user = su::AeroUserData {
        aes_id: a["aes_id"].as_str().unwrap_or("").to_owned(),
        ges_id: a["ges_id"].as_u64().unwrap_or(0) as u8,
        qno: 0,
        refno: 0,
        data: Vec::new(),
    };
    AeroEvent {
        user,
        acars: None,
        bit_rate,
        su_event: Some(a),
        mode,
        channel,
        frame_header: None,
        satellite: None,
    }
}

/// Merge the AERO-2 resolved-satellite annotation and the AERO-4 parsed
/// frame header into a `details` object (in place). Existing keys are never
/// overwritten. The resolved-satellite block (`resolved_satellite`, `beam`,
/// …) and the frame header (`frame_header`) land as nested objects.
fn enrich_details(details: &mut serde_json::Value, e: &AeroEvent) {
    if let serde_json::Value::Object(map) = details {
        // `satellite` is already an object: { resolved_satellite, beam, … }.
        if let Some(serde_json::Value::Object(sat_map)) = &e.satellite {
            for (k, v) in sat_map {
                map.entry(k.clone()).or_insert(v.clone());
            }
        }
        if let Some(h) = e.frame_header {
            map.entry("frame_header".to_string()).or_insert(h.to_json());
        }
    }
}

/// Convert a decoded event into the normalized message model.
pub fn to_message(e: &AeroEvent, frequency_hz: u64, level_dbfs: f32, source: Provenance) -> Message {
    let (body, crc_ok, errors) = match (&e.acars, &e.su_event) {
        (Some(b), _) => (MessageBody::Acars(b.core.clone()), b.crc_ok, Some(b.parity_errors)),
        (None, Some(a)) => {
            // Enrich the structured-SU details with the channel tag and the
            // physical line/burst bit rate so consumers can distinguish
            // P/R/T and the line rate without re-deriving them (AERO-8.2).
            // The line rate uses a distinct key (`line_bit_rate`) so it does
            // not clobber a decoded protocol `bit_rate` field (e.g. the Pd
            // carrier rate in a P/R-control ISU 0x40).
            let mut details = a.clone();
            if let serde_json::Value::Object(map) = &mut details {
                map.insert("channel".into(), serde_json::json!(e.channel.tag()));
                map.insert("line_bit_rate".into(), serde_json::json!(e.bit_rate));
            }
            enrich_details(&mut details, e);
            (MessageBody::Aero { kind: su::p_su_kind(a), details }, true, None)
        }
        // No structured SU but we have a resolved satellite and/or a parsed
        // frame header — surface them on their own Aero body so the AERO-2
        // satellite tag and AERO-4 header reach `details` even for an event
        // that is otherwise just reassembled user data (e.g. ACARS-less).
        (None, None) if e.satellite.is_some() || e.frame_header.is_some() => {
            let mut details = serde_json::json!({});
            enrich_details(&mut details, e);
            (MessageBody::Aero { kind: "aero-frame".to_owned(), details }, true, None)
        }
        (None, None) => (MessageBody::Undecoded, true, None),
    };
    Message {
        mode: e.mode,
        timestamp: Utc::now(),
        frequency_hz,
        signal: SignalQuality { rssi_db: Some(level_dbfs), ..Default::default() },
        decode: DecodeQuality { crc_ok, fec_corrected: None, errors },
        body,
        raw: Some(e.user.data.clone()),
        source,
    }
}
