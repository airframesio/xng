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
    /// `input_rate` is any capture rate ≥ the 24 kHz channel rate; a
    /// non-integer multiple (e.g. an Airspy's 2.5 MS/s) is resampled by the DDC
    /// (an integer multiple like 2.4 MS/s skips the resampler).
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

/// Combine the structured application decode with any flat text-extracted
/// fields (OOOI gate/wheels times + airports, free-text position) into the
/// single `app` JSON value carried by the message body. Returns `None` when
/// there is nothing decoded. Flat fields appear at the top level (matching
/// acarsdec's flat JSON); the structured app object is nested under `app`
/// when both are present.
fn build_app_value(appdec: &xng_acars::AppDecode) -> Option<serde_json::Value> {
    let mut flat = serde_json::Map::new();

    if let Some(serde_json::Value::Object(oooi_map)) =
        appdec.oooi.as_ref().and_then(|o| serde_json::to_value(o).ok())
    {
        flat.extend(oooi_map);
    }
    if let Some(pos) = appdec.position.as_ref().and_then(|p| serde_json::to_value(p).ok()) {
        flat.insert("position".to_string(), pos);
    }

    let app_val = appdec.app.as_ref().map(|a| serde_json::to_value(a).unwrap_or_default());

    match (flat.is_empty(), app_val) {
        (true, None) => None,
        (true, Some(a)) => Some(a),
        (false, None) => Some(serde_json::Value::Object(flat)),
        (false, Some(a)) => {
            flat.insert("app".to_string(), a);
            Some(serde_json::Value::Object(flat))
        }
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
        body: {
            let appdec = xng_acars::decode(&f.label, &f.text, f.downlink);
            // Carry the structured application decode plus any OOOI
            // (OUT/OFF/ON/IN gate/wheels times + depa/dsta/eta) extracted
            // from the text into the body's `app` JSON value (the existing
            // structured field). OOOI fields are merged at the top of the
            // object so they serialize like acarsdec's flat JSON
            // (depa/dsta/eta/gtout/gtin/wloff/wlin).
            let app = build_app_value(&appdec);
            MessageBody::Acars(AcarsCore {
                mode: f.mode,
                tail: f.tail.clone(),
                label: f.label.clone(),
                sublabel: appdec.sublabel,
                mfi: appdec.mfi,
                block_id: f.block_id,
                ack: f.ack,
                flight: f.flight.clone(),
                msg_num: f.msg_num.clone(),
                text: f.text.clone(),
                more_to_come: f.more_to_come,
                reassembled: false,
                app,
            })
        },
        raw: Some(f.raw.clone()),
        source,
    }
}
