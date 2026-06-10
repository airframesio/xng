//! Native Inmarsat Classic Aero decode core (L-band P-channel, 600 and
//! 1200 bps), ported from MIT-licensed JAERO (see PROVENANCE.md).
//!
//! Pipeline per channel: wideband IQ → [`xng_dsp::Ddc`] → 24 kHz channel
//! IQ → [`demod::MskDemod`] (both rates run in parallel; whichever locks
//! wins) → UW framing → [`frame::FrameDecoder`] (deinterleave, Viterbi,
//! descramble) → 12-byte SUs → [`su::Reassembler`] → ACARS via
//! [`xng_acars::block`] → [`xng_types::Message`].

pub mod burst;
pub mod demod;
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
}

impl Framer {
    fn new(rate: u32) -> Self {
        Self {
            decoder: frame::FrameDecoder::new(rate),
            shift: 0,
            collecting: None,
            reasm: su::Reassembler::new(),
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
        if self.collecting.is_none() && self.shift == frame::UW {
            self.collecting = Some(Vec::with_capacity(frame::HEADER_BITS + frame::CODED_BITS));
        }
    }
}

pub struct AeroChannelDecoder {
    ddc: Option<Ddc>,
    chains: Vec<RateChain>,
    channel_buf: Vec<Complex<f32>>,
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
        Ok(Self { ddc, chains, channel_buf: Vec::new() })
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
                out.push(AeroEvent { user, acars, bit_rate: chain.rate });
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
                        out.push(AeroEvent { user, acars, bit_rate: rate });
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

/// Convert a decoded event into the normalized message model.
pub fn to_message(e: &AeroEvent, frequency_hz: u64, level_dbfs: f32, source: Provenance) -> Message {
    let (body, crc_ok, errors) = match &e.acars {
        Some(b) => (MessageBody::Acars(b.core.clone()), b.crc_ok, Some(b.parity_errors)),
        None => (MessageBody::Undecoded, true, None),
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
