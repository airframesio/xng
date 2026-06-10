//! Native ACARS (VHF "plain old ACARS", ARINC 618) decode core.
//!
//! Pipeline per channel: wideband IQ → [`xng_dsp::Ddc`] → 24 kHz channel IQ →
//! [`demod::MskDemod`] (AM envelope → 1800 Hz discriminator → differential
//! bit decisions) → [`frame::Deframer`] (sync hunt, parity, CRC) →
//! [`frame::AcarsFrame`] → [`xng_types::Message`].
//!
//! See PROVENANCE.md for the clean-room sourcing of every protocol fact.

pub mod demod;
pub mod frame;
pub mod modulate;

use chrono::Utc;
use num_complex::Complex;
use xng_dsp::Ddc;
use xng_types::{
    AcarsCore, DecodeQuality, Message, MessageBody, Mode, Provenance, SignalQuality,
};

/// Internal demod sample rate: 10 samples per bit at 2400 bd.
pub const CHANNEL_RATE: f64 = 24_000.0;
/// One-sided channel passband (MSK audio sidebands on AM).
pub const CHANNEL_PASSBAND_HZ: f64 = 5_000.0;

/// Decodes one ACARS channel out of a wideband capture.
pub struct AcarsChannelDecoder {
    ddc: Option<Ddc>,
    demod: demod::MskDemod,
    deframer: frame::Deframer,
    channel_buf: Vec<Complex<f32>>,
    bit_buf: Vec<u8>,
}

impl AcarsChannelDecoder {
    /// `input_rate` must be an integer multiple of 24 kHz (e.g. 2.4 MS/s).
    /// `freq_offset_hz` is the channel center relative to the capture center.
    pub fn new(input_rate: f64, freq_offset_hz: f64) -> Result<Self, String> {
        let ddc = if (input_rate - CHANNEL_RATE).abs() < 1e-6 && freq_offset_hz.abs() < 1e-6 {
            None
        } else {
            Some(Ddc::new(input_rate, CHANNEL_RATE, freq_offset_hz, CHANNEL_PASSBAND_HZ)?)
        };
        Ok(Self {
            ddc,
            demod: demod::MskDemod::new(),
            deframer: frame::Deframer::new(),
            channel_buf: Vec::new(),
            bit_buf: Vec::new(),
        })
    }

    /// Feed wideband IQ; returns any completed frames.
    pub fn process(&mut self, input: &[Complex<f32>]) -> Vec<frame::AcarsFrame> {
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
        let mut frames = Vec::new();
        for &bit in &self.bit_buf {
            if let Some(f) = self.deframer.push_bit(bit) {
                frames.push(f);
            }
        }
        frames
    }

    /// Smoothed channel envelope level in dBFS (rough RSSI for metadata).
    pub fn level_dbfs(&self) -> f32 {
        self.demod.level_dbfs()
    }
}

/// Convert a decoded frame into the normalized message model.
pub fn to_message(
    f: &frame::AcarsFrame,
    frequency_hz: u64,
    level_dbfs: f32,
    source: Provenance,
) -> Message {
    Message {
        mode: Mode::AcarsPoa,
        timestamp: Utc::now(),
        frequency_hz,
        signal: SignalQuality { rssi_db: Some(level_dbfs), ..Default::default() },
        decode: DecodeQuality {
            crc_ok: f.crc_ok,
            fec_corrected: Some(f.fixed_bits),
            errors: Some(f.parity_errors),
        },
        body: MessageBody::Acars(AcarsCore {
            mode: f.mode,
            tail: f.tail.clone(),
            label: f.label.clone(),
            sublabel: None,
            block_id: f.block_id,
            ack: f.ack,
            flight: f.flight.clone(),
            msg_num: f.msg_num.clone(),
            text: f.text.clone(),
            more_to_come: f.more_to_come,
        }),
        raw: Some(f.raw.clone()),
        source,
    }
}
