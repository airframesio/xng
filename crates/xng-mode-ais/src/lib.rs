//! Native AIS (ITU-R M.1371) decode core.
//!
//! Pipeline per channel: wideband IQ → [`xng_dsp::Ddc`] → 48 kHz channel IQ
//! → [`demod::GmskDemod`] (frequency discriminator with offset tracking,
//! timing recovery, NRZI decode) → [`frame::HdlcDeframer`] (flag hunt,
//! destuffing, FCS) → [`frame::AisFrame`] → NMEA AIVDM →
//! [`xng_types::Message`].
//!
//! See PROVENANCE.md for the clean-room sourcing of every protocol fact.

pub mod coherent;
pub mod demod;
pub mod fields;
pub mod frame;
pub mod modulate;
pub mod nmea;

use chrono::Utc;
use num_complex::Complex;
use xng_dsp::Ddc;
use xng_types::{DecodeQuality, Message, MessageBody, Mode, Provenance, SignalQuality};

/// Internal demod sample rate: 5 samples per bit at 9600 bd.
pub const CHANNEL_RATE: f64 = 48_000.0;
/// One-sided channel passband (GMSK BT=0.4 at 9600 bd in a 25 kHz channel).
pub const CHANNEL_PASSBAND_HZ: f64 = 8_000.0;

/// AIS channel A/B designators by frequency.
pub fn channel_letter(frequency_hz: u64) -> char {
    match frequency_hz {
        161_975_000 => 'A',
        162_025_000 => 'B',
        _ => 'A',
    }
}

/// Decodes one AIS channel out of a wideband capture.
pub struct AisChannelDecoder {
    ddc: Option<Ddc>,
    coherent: coherent::CoherentDemod,
    /// Recently emitted frames (bits, channel-sample time) so the
    /// streaming and coherent paths don't double-report one burst —
    /// the coherent path buffers a whole burst and can emit a chunk
    /// or two later.
    recent: Vec<(Vec<u8>, u64)>,
    channel_samples: u64,
    demod: demod::GmskDemod,
    deframer: frame::HdlcDeframer,
    nmea: nmea::SentenceBuilder,
    channel_buf: Vec<Complex<f32>>,
    bit_buf: Vec<u8>,
    channel: char,
}

impl AisChannelDecoder {
    /// `input_rate` is any capture rate ≥ the 48 kHz channel rate; a
    /// non-integer multiple (e.g. an Airspy's 2.5 MS/s) is resampled by the
    /// DDC (an integer multiple like 2.4 MS/s skips the resampler).
    /// `freq_offset_hz` is the channel center relative to the capture
    /// center; `frequency_hz` the absolute channel frequency (for the
    /// NMEA channel designator).
    pub fn new(input_rate: f64, freq_offset_hz: f64, frequency_hz: u64) -> Result<Self, String> {
        let ddc = if (input_rate - CHANNEL_RATE).abs() < 1e-6 && freq_offset_hz.abs() < 1e-6 {
            None
        } else {
            Some(Ddc::new(input_rate, CHANNEL_RATE, freq_offset_hz, CHANNEL_PASSBAND_HZ)?)
        };
        Ok(Self {
            ddc,
            coherent: coherent::CoherentDemod::new(CHANNEL_RATE),
            recent: Vec::new(),
            channel_samples: 0,
            demod: demod::GmskDemod::new(),
            deframer: frame::HdlcDeframer::new(),
            nmea: nmea::SentenceBuilder::new(),
            channel_buf: Vec::new(),
            bit_buf: Vec::new(),
            channel: channel_letter(frequency_hz),
        })
    }

    /// Max effort: anchor down to the deep-weak detection floor
    /// (~3× hunt CPU; see coherent.rs).
    pub fn set_max_effort(&mut self, max: bool) {
        self.coherent.set_max_effort(max);
    }

    /// Feed wideband IQ; returns decoded frames with their NMEA sentences.
    pub fn process(&mut self, input: &[Complex<f32>]) -> Vec<(frame::AisFrame, Vec<String>)> {
        let channel: &[Complex<f32>] = match &mut self.ddc {
            Some(ddc) => {
                self.channel_buf.clear();
                ddc.process(input, &mut self.channel_buf);
                &self.channel_buf
            }
            None => input,
        };
        self.channel_samples += channel.len() as u64;
        let now = self.channel_samples;
        // One AIS slot is 256 bits ≈ 1280 channel samples; expire dedup
        // entries after a generous four slots.
        self.recent.retain(|(_, t)| now - t < 5_200);

        self.bit_buf.clear();
        self.demod.process(channel, &mut self.bit_buf);
        let mut out = Vec::new();
        for &bit in &self.bit_buf {
            if let Some(f) = self.deframer.push_bit(bit) {
                self.recent.push((f.message_bits.clone(), now));
                let sentences = self.nmea.encode(&f.message_bits, self.channel);
                out.push((f, sentences));
            }
        }
        // Coherent weak-signal path: independently hunted bursts run
        // through a fresh HDLC deframer (a synthetic opening flag re-arms
        // it, since the coherent decoder starts right after the flag).
        for bits in self.coherent.process(channel) {
            let mut df = frame::HdlcDeframer::new();
            for &b in &[0u8, 1, 1, 1, 1, 1, 1, 0] {
                df.push_bit(b);
            }
            for &b in &bits {
                if let Some(f) = df.push_bit(b) {
                    if !self.recent.iter().any(|(m, _)| *m == f.message_bits) {
                        self.recent.push((f.message_bits.clone(), now));
                        let sentences = self.nmea.encode(&f.message_bits, self.channel);
                        out.push((f, sentences));
                    }
                    break;
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

/// Convert a decoded frame into the normalized message model.
pub fn to_message(
    f: &frame::AisFrame,
    nmea: Vec<String>,
    frequency_hz: u64,
    level_dbfs: f32,
    source: Provenance,
) -> Message {
    Message {
        mode: Mode::Ais,
        timestamp: Utc::now(),
        frequency_hz,
        signal: SignalQuality { rssi_db: Some(level_dbfs), ..Default::default() },
        decode: DecodeQuality { crc_ok: true, fec_corrected: None, errors: None },
        body: MessageBody::Ais {
            nmea,
            msg_type: Some(f.msg_type),
            mmsi: Some(f.mmsi),
            details: ais_details(f),
        },
        raw: Some(f.wire_bytes.clone()),
        source,
    }
}

/// Field decode plus a `distress` tag for SART/MOB/EPIRB-AIS devices
/// (classified by MMSI prefix — see [`fields::distress_class`]).
fn ais_details(f: &frame::AisFrame) -> Option<serde_json::Value> {
    let mut details = fields::decode(f.msg_type, &f.message_bits);
    if let Some(device) = fields::distress_class(f.mmsi) {
        let d = details.get_or_insert_with(|| serde_json::json!({}));
        if let Some(obj) = d.as_object_mut() {
            obj.insert("distress".into(), serde_json::json!(device));
        }
    }
    details
}
