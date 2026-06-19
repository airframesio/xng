//! Native HFDL decode core (see PROVENANCE.md and docs/notes/HFDL.md).

pub mod ac_cache;
pub mod demod;
pub mod fec;
pub mod modulate;
pub mod pdu;
pub mod systable;

use chrono::Utc;
use num_complex::Complex;
use xng_dsp::Ddc;
use xng_types::{DecodeQuality, Message, MessageBody, Mode, Provenance, SignalQuality};

/// Internal channel rate (≈6.67 samples/symbol at 1800 sym/s).
pub const CHANNEL_RATE: f64 = 12_000.0;
/// One-sided passband (2.8 kHz USB channel; signal ±~1.3 kHz around the
/// subcarrier after DDC).
pub const CHANNEL_PASSBAND_HZ: f64 = 1_500.0;
/// Audio subcarrier offset from the SSB carrier (channel) frequency.
pub const SUBCARRIER_OFFSET_HZ: f64 = 1_440.0;

pub struct HfdlChannelDecoder {
    ddc: Option<Ddc>,
    /// Channel-selectivity lowpass for the no-DDC path: the same
    /// ±1.5 kHz passband the DDC's decimation filter applies (off-air
    /// validated through that path). Without it a 12 kS/s direct input
    /// feeds the demod the full ±6 kHz of noise. The LMS equalizer
    /// downstream absorbs the filter's static in-band ISI, as it does
    /// on the DDC path.
    selectivity: Option<xng_dsp::Fir>,
    select_buf: Vec<Complex<f32>>,
    demod: demod::HfdlDemod,
    parser: pdu::PduParser,
    channel_buf: Vec<Complex<f32>>,
}

impl HfdlChannelDecoder {
    /// `freq_offset_hz` is the SSB carrier (channel) frequency relative
    /// to the capture center; the +1440 Hz subcarrier shift is applied
    /// here.
    pub fn new(input_rate: f64, freq_offset_hz: f64) -> Result<Self, String> {
        let sub = freq_offset_hz + SUBCARRIER_OFFSET_HZ;
        let ddc = if (input_rate - CHANNEL_RATE).abs() < 1e-6 && sub.abs() < 1e-6 {
            None
        } else {
            Some(Ddc::new(input_rate, CHANNEL_RATE, sub, CHANNEL_PASSBAND_HZ)?)
        };
        let selectivity = if ddc.is_none() {
            let taps =
                xng_dsp::lowpass_taps(CHANNEL_PASSBAND_HZ / CHANNEL_RATE, 101);
            Some(xng_dsp::Fir::new(taps))
        } else {
            None
        };
        Ok(Self {
            ddc,
            selectivity,
            select_buf: Vec::new(),
            demod: demod::HfdlDemod::new(CHANNEL_RATE),
            parser: pdu::PduParser::new(),
            channel_buf: Vec::new(),
        })
    }

    pub fn process(&mut self, input: &[Complex<f32>]) -> Vec<pdu::HfdlEvent> {
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
        for burst in self.demod.process(channel) {
            for mut e in self.parser.parse(&burst.payload, burst.bps) {
                // Stamp every event from this burst with the demod-measured
                // quality figures (HFDL-5): Viterbi corrected-symbol count,
                // carrier frequency offset (Hz), and EVM-derived SNR (dB).
                e.fec_corrected = Some(burst.fec_corrected);
                e.freq_skew_hz = Some(burst.freq_skew_hz);
                e.snr_db = burst.snr_db;
                out.push(e);
            }
        }
        out
    }

    pub fn level_dbfs(&self) -> f32 {
        self.demod.level_dbfs()
    }
}

/// Convert a decoded event into the normalized message model.
pub fn to_message(e: &pdu::HfdlEvent, frequency_hz: u64, level_dbfs: f32, source: Provenance) -> Message {
    let (body, crc_ok, errors) = match &e.acars {
        Some(b) => (
            MessageBody::Acars(b.core.clone()),
            b.crc_ok,
            Some(b.parity_errors),
        ),
        None => (
            MessageBody::Hfdl { kind: e.kind.clone(), details: e.details.clone() },
            true,
            None,
        ),
    };
    Message {
        mode: Mode::Hfdl,
        timestamp: Utc::now(),
        frequency_hz,
        // HFDL-5: surface the demod-measured SNR and carrier frequency
        // offset (only set for burst-derived events; byte-built events
        // leave them None — never fabricated).
        signal: SignalQuality {
            rssi_db: Some(level_dbfs),
            snr_db: e.snr_db,
            freq_skew_hz: e.freq_skew_hz,
            ..Default::default()
        },
        decode: DecodeQuality { crc_ok, fec_corrected: e.fec_corrected, errors },
        body,
        raw: Some(e.raw.clone()),
        source,
    }
}
