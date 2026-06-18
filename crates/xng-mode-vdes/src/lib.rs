//! Native VDES ASM (VHF Data Exchange System — Application-Specific
//! Messages) decode core, per ITU-R M.2092-1.
//!
//! VDES (ITU-R M.2092-1) augments AIS with dedicated ASM and VDE channels.
//! This crate decodes the **ASM** (Application-Specific Message) channels —
//! ASM 1 = 161.950 MHz and ASM 2 = 162.000 MHz (the former AIS channels
//! 2027 / 2028) — which carry GMSK-modulated, HDLC-framed, NRZI bit-stuffed
//! messages, the same link family as AIS, but reserved for the
//! application-specific (DAC/FID binary) message traffic moved off the AIS
//! position channels.
//!
//! Pipeline per channel: wideband IQ → [`xng_dsp::Ddc`] → 48 kHz channel IQ
//! → [`demod::GmskDemod`] (frequency discriminator, offset tracking, timing
//! recovery, NRZI decode) → [`frame::HdlcDeframer`] (flag hunt, destuffing,
//! CRC-16/X-25 FCS) → [`frame::VdesFrame`] → [`asm::decode`] (source MMSI +
//! DAC/FID + application payload) → [`xng_types::Message`].
//!
//! SCOPE (per the VDES-ASM mandate; HONEST about VDES's sparse public spec):
//! decoded here is the ASM transport (AIS Message 6 addressed / Message 8
//! broadcast binary, reused verbatim by M.2092-1) and a couple of
//! spec-citable DAC=1 (IMO SN.1/Circ.289) application payloads. The VDE
//! (high-rate data exchange) links, the satellite VDES component, and the
//! full IALA ASM DAC/FID catalogue are NOT implemented — see PROVENANCE.md
//! "Deferred". The PHY demod is validated only by a synthetic
//! modulate→AWGN→demod BER test (no real off-air VDES IQ exists).

pub mod asm;
pub mod demod;
pub mod frame;
pub mod modulate;

use chrono::Utc;
use num_complex::Complex;
use xng_dsp::Ddc;
use xng_types::{DecodeQuality, Message, MessageBody, Mode, Provenance, SignalQuality};

/// Internal demod sample rate: 5 samples per bit at 9600 bd.
pub const CHANNEL_RATE: f64 = 48_000.0;
/// One-sided channel passband (GMSK BT=0.5 at 9600 bd in a 25 kHz channel).
pub const CHANNEL_PASSBAND_HZ: f64 = 8_000.0;

/// VDES ASM channel center frequencies (ITU-R M.2092-1 Annex 1): ASM 1 =
/// 161.950 MHz, ASM 2 = 162.000 MHz.
pub const ASM1_HZ: u64 = 161_950_000;
pub const ASM2_HZ: u64 = 162_000_000;

/// Decodes one VDES ASM channel out of a wideband capture.
pub struct VdesChannelDecoder {
    ddc: Option<Ddc>,
    demod: demod::GmskDemod,
    deframer: frame::HdlcDeframer,
    channel_buf: Vec<Complex<f32>>,
    bit_buf: Vec<u8>,
}

impl VdesChannelDecoder {
    /// `input_rate` is any capture rate ≥ the 48 kHz channel rate; a
    /// non-integer multiple is resampled by the DDC. `freq_offset_hz` is the
    /// ASM channel center relative to the capture center.
    pub fn new(input_rate: f64, freq_offset_hz: f64) -> Result<Self, String> {
        let ddc = if (input_rate - CHANNEL_RATE).abs() < 1e-6 && freq_offset_hz.abs() < 1e-6 {
            None
        } else {
            Some(Ddc::new(input_rate, CHANNEL_RATE, freq_offset_hz, CHANNEL_PASSBAND_HZ)?)
        };
        Ok(Self {
            ddc,
            demod: demod::GmskDemod::new(),
            deframer: frame::HdlcDeframer::new(),
            channel_buf: Vec::new(),
            bit_buf: Vec::new(),
        })
    }

    /// Feed wideband IQ; returns CRC-valid ASM frames.
    pub fn process(&mut self, input: &[Complex<f32>]) -> Vec<frame::VdesFrame> {
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
                out.push(f);
            }
        }
        out
    }

    /// Smoothed channel power level in dBFS.
    pub fn level_dbfs(&self) -> f32 {
        self.demod.level_dbfs()
    }
}

/// Convert a decoded ASM frame into the normalized bus message
/// (`MessageBody::Vdes`). `kind` distinguishes addressed vs broadcast ASM;
/// `details` carries the source MMSI, DAC/FID, and decoded application
/// fields. Returns `None` if the frame is not a recognised ASM transport
/// (message ID other than 6/8).
pub fn to_message(
    f: &frame::VdesFrame,
    frequency_hz: u64,
    level_dbfs: f32,
    source: Provenance,
) -> Option<Message> {
    let asm = asm::decode(&f.message_bits)?;
    Some(Message {
        mode: Mode::Vdes,
        timestamp: Utc::now(),
        frequency_hz,
        signal: SignalQuality { rssi_db: Some(level_dbfs), ..Default::default() },
        decode: DecodeQuality { crc_ok: true, fec_corrected: None, errors: None },
        body: MessageBody::Vdes { kind: asm.kind().to_string(), details: asm.details() },
        raw: Some(f.wire_bytes.clone()),
        source,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channel_rate_is_integer_bit_multiple() {
        let spb = CHANNEL_RATE / demod::BAUD;
        assert_eq!(spb.fract(), 0.0, "{spb} samples/bit");
        // Channel rate must clear the Nyquist of the one-sided passband.
        let min_rate = 2.0 * CHANNEL_PASSBAND_HZ;
        assert!(CHANNEL_RATE >= min_rate, "{CHANNEL_RATE} < {min_rate}");
    }

    #[test]
    fn asm_channel_frequencies_are_documented() {
        // ITU-R M.2092-1 Annex 1: ASM 1 / ASM 2.
        assert_eq!(ASM1_HZ, 161_950_000);
        assert_eq!(ASM2_HZ, 162_000_000);
    }
}
