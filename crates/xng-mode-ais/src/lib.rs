//! Native AIS (ITU-R M.1371) decode core.
//!
//! Pipeline per channel: wideband IQ → [`xng_dsp::Ddc`] → 48 kHz channel IQ
//! → [`demod::GmskDemod`] (frequency discriminator with offset tracking,
//! timing recovery, NRZI decode) → [`frame::HdlcDeframer`] (flag hunt,
//! destuffing, FCS) → [`frame::AisFrame`] → NMEA AIVDM →
//! [`xng_types::Message`].
//!
//! See PROVENANCE.md for the clean-room sourcing of every protocol fact.

pub mod demod;
pub mod frame;
pub mod fields;
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
    demod: demod::GmskDemod,
    deframer: frame::HdlcDeframer,
    nmea: nmea::SentenceBuilder,
    channel_buf: Vec<Complex<f32>>,
    bit_buf: Vec<u8>,
    channel: char,
}

impl AisChannelDecoder {
    /// `input_rate` must be an integer multiple of 48 kHz (e.g. 2.4 MS/s).
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
            demod: demod::GmskDemod::new(),
            deframer: frame::HdlcDeframer::new(),
            nmea: nmea::SentenceBuilder::new(),
            channel_buf: Vec::new(),
            bit_buf: Vec::new(),
            channel: channel_letter(frequency_hz),
        })
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
        self.bit_buf.clear();
        self.demod.process(channel, &mut self.bit_buf);
        let mut out = Vec::new();
        for &bit in &self.bit_buf {
            if let Some(f) = self.deframer.push_bit(bit) {
                let sentences = self.nmea.encode(&f.message_bits, self.channel);
                out.push((f, sentences));
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
            details: fields::decode(f.msg_type, &f.message_bits),
        },
        raw: Some(f.wire_bytes.clone()),
        source,
    }
}
