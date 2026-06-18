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
pub mod coherent;
pub mod demod;
pub mod oqpsk;
pub mod frame;
pub mod modulate;
pub mod satellite;
pub mod state;
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
    /// Coded-bit errors the Viterbi FEC corrected on the frame that carried
    /// this event (AERO-6), when known. Genuine count derived by re-encoding
    /// the decoded bits ([`frame::FrameDecoder::last_fec_corrected`]); `None`
    /// for paths where it is not tracked. Feeds `DecodeQuality::fec_corrected`.
    pub fec_corrected: Option<u32>,
    /// P-channel superframe-lock + DCD/AFC state at the time this event's
    /// frame was decoded (AERO-4; enrichment, see
    /// [`state::SuperframeLockStateMachine`]). Only the low-rate L-band
    /// P-channel framer (`AeroChannelDecoder`) sets this; the 10.5k OQPSK
    /// and C-band burst paths use a different framer and leave it `None`.
    pub lock: Option<serde_json::Value>,
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
    /// FEC-corrected coded-bit count of the most recently decoded frame
    /// (AERO-6), latched so events from that frame carry it.
    last_fec_corrected: Option<u32>,
    /// Self-configuring satellite/beam resolver, fed every structured SU it
    /// sees (AERO-2). Latches the most recent satellite from the system-
    /// table broadcasts.
    resolver: satellite::SatelliteResolver,
    /// P-channel superframe-lock / DCD / AFC state machine (AERO-4), fed the
    /// parsed header of every collected frame.
    lock: state::SuperframeLockStateMachine,
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
            last_fec_corrected: None,
            resolver: satellite::SatelliteResolver::new(),
            lock: state::SuperframeLockStateMachine::new(),
        }
    }

    /// Snapshot of the current superframe-lock / DCD / AFC state for
    /// enrichment (AERO-4).
    fn lock_json(&self) -> serde_json::Value {
        self.lock.details_json()
    }

    fn push(&mut self, soft: f32, hard: u8, out: &mut Vec<su::AeroUserData>) {
        if let Some(buf) = &mut self.collecting {
            buf.push(soft);
            if buf.len() == frame::HEADER_BITS + frame::CODED_BITS {
                // Parse the 16-bit frame header (AERO-4) before the coded
                // payload, advance the superframe-lock / DCD / AFC state
                // machine with it, then decode the SUs.
                let header = frame::FrameHeader::from_soft_bits(&buf[..frame::HEADER_BITS]);
                self.last_header = Some(header);
                self.lock.update(header);
                let coded = &buf[frame::HEADER_BITS..];
                let bytes = self.decoder.decode(coded);
                // Genuine FEC-correction count for this frame (AERO-6).
                self.last_fec_corrected = Some(self.decoder.last_fec_corrected());
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
            // The frame header (AERO-4), resolved satellite (AERO-2) and
            // superframe-lock state (AERO-4) are latched in the framer; tag
            // every event from this chain with the current state.
            let frame_header = chain.framer.last_header;
            let fec_corrected = chain.framer.last_fec_corrected;
            let satellite = chain.framer.resolver.details();
            let lock = chain.framer.lock_json();
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
                    fec_corrected,
                    lock: Some(lock.clone()),
                });
            }
            for a in chain.framer.su_events.drain(..) {
                let mut e = su_event_msg(a, chain.rate, Mode::AeroL, AeroChannel::PChannel);
                e.frame_header = frame_header;
                e.satellite = satellite.clone();
                e.fec_corrected = fec_corrected;
                e.lock = Some(lock.clone());
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
            let fec_corrected = hr.framer.last_fec_corrected;
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
                    fec_corrected,
                    // The 10.5k OQPSK chain uses a different framer; the
                    // superframe-lock machine is the low-rate P-channel layer
                    // (AERO-4 scope), so leave it unset here.
                    lock: None,
                });
            }
            for a in hr.framer.su_events.drain(..) {
                let mut e = su_event_msg(a, oqpsk::BIT_RATE, Mode::AeroL, AeroChannel::PChannel);
                e.frame_header = frame_header;
                e.satellite = satellite.clone();
                e.fec_corrected = fec_corrected;
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
                // Discriminator detector first (robust on the burst preamble),
                // then the coherent (decision-directed) detector as a fallback
                // for marginal bursts — it recovers ~1 dB lower (AERO-6,
                // tests/coherent_ber.rs). The fallback is safe: `process`
                // returns `None` only when no UW/CRC matched, in which case it
                // left the cross-burst reassemblers untouched, so the second
                // pass cannot double-feed them.
                let bits = burst::demod_burst(&b, CHANNEL_RATE, rate as f64);
                let result = self.packetizers[i].process(&bits).or_else(|| {
                    let coh = burst::demod_burst_coherent(&b, CHANNEL_RATE, rate as f64);
                    self.packetizers[i].process(&coh)
                });
                if let Some(result) = result {
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
                    let fec_corrected = Some(result.fec_corrected);
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
                            fec_corrected,
                            // C-band feeder bursts use the burst framer, not
                            // the P-channel superframe-lock layer (AERO-4).
                            lock: None,
                        });
                    }
                    // Named control/signalling SUs (R access-request /
                    // call-progress / telephony-ack / RQA / ACK, or T-burst
                    // P-style control SUs) carry the burst's bit rate and
                    // the resolved channel tag (AERO-3 / AERO-8.2).
                    for a in result.su_events {
                        let mut e = su_event_msg(a, rate, Mode::AeroC, aero_channel);
                        e.satellite = satellite.clone();
                        e.fec_corrected = fec_corrected;
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
        fec_corrected: None,
        // Filled in by the caller for P-channel events; left unset here.
        lock: None,
    }
}

/// Merge the AERO-2 resolved-satellite annotation, the AERO-4 parsed frame
/// header and the AERO-4 superframe-lock snapshot into a `details` object
/// (in place). Existing keys are never overwritten. The resolved-satellite
/// block (`resolved_satellite`, `beam`, …), the frame header
/// (`frame_header`) and the lock snapshot (`superframe_lock`) land as nested
/// objects.
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
        // AERO-4: the P-channel superframe-lock / DCD / AFC snapshot, when
        // this event came from the low-rate L-band P-channel framer.
        if let Some(lock) = &e.lock {
            map.entry("superframe_lock".to_string()).or_insert(lock.clone());
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
        // No structured SU but the low-rate L-band P-channel framer tracked
        // a superframe-lock snapshot for the frame that carried this event —
        // surface the channel framing state (carrier_state / dcd / afc_locked
        // / frame counters) on its own Aero body so the AERO-4 lock state is
        // observable even for an otherwise-undecoded P-channel event (AERO-4).
        // The frame header and any resolved satellite ride along.
        (None, None) if e.lock.is_some() => {
            let mut details = serde_json::json!({});
            enrich_details(&mut details, e);
            (MessageBody::Aero { kind: "p-channel-status".to_owned(), details }, true, None)
        }
        // No lock snapshot (e.g. the 10.5k OQPSK chain), but we have a
        // resolved satellite and/or a parsed frame header — surface them on
        // their own Aero body so the AERO-2 satellite tag and AERO-4 header
        // reach `details` even for an event that is otherwise just
        // reassembled user data (e.g. ACARS-less).
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
        decode: DecodeQuality { crc_ok, fec_corrected: e.fec_corrected, errors },
        body,
        raw: Some(e.user.data.clone()),
        source,
    }
}
