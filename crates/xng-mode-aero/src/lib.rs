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
pub mod su;

use chrono::Utc;
use num_complex::Complex;
use xng_dsp::Ddc;
use xng_types::{DecodeQuality, Message, MessageBody, Mode, Provenance, SignalQuality};

pub const CHANNEL_RATE: f64 = 24_000.0;
/// One-sided passband: MSK at ≤1200 bps occupies well under ±2 kHz.
pub const CHANNEL_PASSBAND_HZ: f64 = 2_500.0;

/// A decoded Aero event: reassembled user data, ACARS when parseable.
pub struct AeroEvent {
    pub user: su::AeroUserData,
    pub acars: Option<xng_acars::block::AcarsBlock>,
    pub bit_rate: u32,
    /// Set when this event is a C-channel assignment SU.
    pub assignment: Option<serde_json::Value>,
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
    /// C-channel assignment SUs decoded this push (drained per chunk).
    assignments: Vec<serde_json::Value>,
}

impl Framer {
    fn new(rate: u32) -> Self {
        Self {
            decoder: frame::FrameDecoder::new(rate),
            shift: 0,
            collecting: None,
            reasm: su::Reassembler::new(),
            assignments: Vec::new(),
        }
    }

    fn push(&mut self, soft: f32, hard: u8, out: &mut Vec<su::AeroUserData>) {
        if let Some(buf) = &mut self.collecting {
            buf.push(soft);
            if buf.len() == frame::HEADER_BITS + frame::CODED_BITS {
                let coded = &buf[frame::HEADER_BITS..];
                let bytes = self.decoder.decode(coded);
                for su_bytes in bytes.chunks_exact(su::SU_LEN) {
                    if su::su_crc_ok(su_bytes) {
                        if let Some(a) = su::parse_c_assignment(su_bytes) {
                            self.assignments.push(a);
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
            for user in users {
                let acars = su::parse_acars(&user.data);
                out.push(AeroEvent { user, acars, bit_rate: chain.rate, assignment: None });
            }
            for a in chain.framer.assignments.drain(..) {
                out.push(assignment_event(a, chain.rate));
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
            for user in hr_users {
                let acars = su::parse_acars(&user.data);
                out.push(AeroEvent { user, acars, bit_rate: oqpsk::BIT_RATE, assignment: None });
            }
            for a in hr.framer.assignments.drain(..) {
                out.push(assignment_event(a, oqpsk::BIT_RATE));
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
                    for user in result.users {
                        let acars = su::parse_acars(&user.data);
                        out.push(AeroEvent { user, acars, bit_rate: rate, assignment: None });
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

fn assignment_event(a: serde_json::Value, bit_rate: u32) -> AeroEvent {
    let user = su::AeroUserData {
        aes_id: a["aes_id"].as_str().unwrap_or("").to_owned(),
        ges_id: a["ges_id"].as_u64().unwrap_or(0) as u8,
        qno: 0,
        refno: 0,
        data: Vec::new(),
    };
    AeroEvent { user, acars: None, bit_rate, assignment: Some(a) }
}

/// Convert a decoded event into the normalized message model.
pub fn to_message(e: &AeroEvent, frequency_hz: u64, level_dbfs: f32, source: Provenance) -> Message {
    let (body, crc_ok, errors) = match (&e.acars, &e.assignment) {
        (Some(b), _) => (MessageBody::Acars(b.core.clone()), b.crc_ok, Some(b.parity_errors)),
        (None, Some(a)) => (
            MessageBody::Aero { kind: "c-channel-assignment".to_owned(), details: a.clone() },
            true,
            None,
        ),
        (None, None) => (MessageBody::Undecoded, true, None),
    };
    Message {
        mode: Mode::AeroL,
        timestamp: Utc::now(),
        frequency_hz,
        signal: SignalQuality { rssi_db: Some(level_dbfs), ..Default::default() },
        decode: DecodeQuality { crc_ok, fec_corrected: None, errors },
        body,
        raw: Some(e.user.data.clone()),
        source,
    }
}
