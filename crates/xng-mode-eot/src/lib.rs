//! Native rail End-of-Train / Head-of-Train (EOT/HOT) telemetry decode core
//! for xng.
//!
//! North-American rail uses a short two-way telemetry link between the
//! locomotive (HOT, Head-of-Train) and the rear-car device (EOT,
//! End-of-Train): the EOT periodically reports brake-pipe pressure, motion,
//! marker-light and battery status to the head end, and the head end can
//! command the EOT (e.g. emergency brake). On air it is narrowband 1200-baud
//! binary FSK with Manchester line coding:
//!
//! - EOT -> HOT (rear-to-front telemetry): 457.9375 MHz
//! - HOT -> EOT (front-to-rear command):   452.9375 MHz
//!
//! (frequencies + 1200-baud FSK per SIGIDWIKI "End of Train Device (EOTD)").
//!
//! This crate implements both layers, bottom-up:
//!
//! - [`demod`] — narrow-shift FSK discriminator + chip-clock recovery,
//!   producing a Manchester chip stream.
//! - [`frame`] — Manchester decode, frame-sync hunt, and the AAR S-9152
//!   reverse-engineered field map (anchored to the cited PyEOT / EOTDecode
//!   decoders, see `frame.rs`).
//! - [`bch`] — the 18-bit ciphered BCH(63,45) data-block check.
//! - [`modulate`] — waveform synthesis used ONLY by tests.
//!
//! VERIFICATION POSTURE: the DECODE/framing layer is verified against the
//! documented field map (a hand-built spec packet whose fields the decoder
//! recovers AND whose BCH check verifies). The DEMOD layer is validated by a
//! synthetic `modulate -> AWGN -> demod` frame-recovery / BER measurement
//! (see the `*_synth_iq` tests). No off-air IQ is available, so no real-RF
//! claim is made. This link is reverse-engineered with no public formal AAR
//! standard; see the notes in `frame.rs` for fields that could not be pinned.

pub mod bch;
pub mod demod;
pub mod frame;
pub mod modulate;

pub use frame::EotFrame;

use chrono::Utc;
use num_complex::Complex;
use xng_dsp::Ddc;
use xng_types::{DecodeQuality, Message, MessageBody, Mode, Provenance, SignalQuality};

/// Internal demod sample rate: 10 samples per Manchester chip at the 2400-chip
/// rate (1200 baud x 2 chips). A clean integer multiple of the chip rate that
/// resolves the FSK swing with margin for the timing loop.
pub const CHANNEL_RATE: f64 = 24_000.0;
/// One-sided DDC passband. Comfortably passes the ±FSK tones plus the
/// Manchester chip-rate sidebands and a realistic tuning offset, while staying
/// inside the ~8 kHz EOT channel.
pub const CHANNEL_PASSBAND_HZ: f64 = 4_000.0;

/// On-air RF facts (informational; SIGIDWIKI EOTD).
pub mod params {
    /// Data baud rate (Manchester line code, 2 chips per bit).
    pub const BAUD: f64 = 1200.0;
    /// EOT -> HOT (rear-to-front telemetry) center frequency, Hz.
    pub const FREQ_EOT_TO_HOT: u64 = 457_937_500;
    /// HOT -> EOT (front-to-rear command) center frequency, Hz.
    pub const FREQ_HOT_TO_EOT: u64 = 452_937_500;
}

/// The 17-bit on-air hunt run: `101010` (bit-sync tail) + the 11-bit frame
/// sync word, per the cited decoders' `10101011100010010` search.
fn hunt_pattern() -> Vec<u8> {
    let mut p = frame::BIT_SYNC_TAIL.to_vec();
    p.extend_from_slice(&frame::FRAME_SYNC);
    p
}

/// Scan a logical bit stream for the hunt pattern and decode every aligned
/// 74-bit packet that follows. Returns the parsed frames.
///
/// The hunt pattern's last 11 bits are the frame sync, which becomes
/// `packet[0:11]`, so a match at index `i` means the packet starts at
/// `i + BIT_SYNC_TAIL.len()`.
pub fn scan_bits(bits: &[u8]) -> Vec<EotFrame> {
    let pat = hunt_pattern();
    let sync_off = frame::BIT_SYNC_TAIL.len();
    let mut out = Vec::new();
    if bits.len() < pat.len() {
        return out;
    }
    let mut i = 0;
    while i + pat.len() <= bits.len() {
        if bits[i..i + pat.len()] == pat[..] {
            let start = i + sync_off;
            if start + frame::PACKET_BITS <= bits.len() {
                if let Some(f) = frame::parse_packet(&bits[start..start + frame::PACKET_BITS]) {
                    out.push(f);
                    // Skip past this packet to avoid re-matching inside it.
                    i = start + frame::PACKET_BITS;
                    continue;
                }
            }
        }
        i += 1;
    }
    out
}

/// One fully decoded EOT/HOT frame plus the raw packet bits it came from.
#[derive(Debug, Clone)]
pub struct EotDecodedFrame {
    /// Parsed telemetry fields.
    pub frame: EotFrame,
    /// The 74 raw packet bits (sync + data + check), one byte per bit.
    pub packet_bits: Vec<u8>,
}

/// Decodes one EOT/HOT channel out of a wideband (or channel-rate) capture.
///
/// Mirrors the NAVTEX [`crate`]-template contract: owns an internal [`Ddc`]
/// that mixes by `freq_offset_hz` and decimates the capture to
/// [`CHANNEL_RATE`], runs the Manchester-FSK demod, and emits an
/// [`EotDecodedFrame`] per recovered packet.
pub struct EotChannelDecoder {
    ddc: Option<Ddc>,
    demod: demod::FskDemod,
    /// Manchester chip history (EOT bursts are short; we buffer and re-scan).
    chips: Vec<u8>,
    channel_buf: Vec<Complex<f32>>,
    /// Packets already reported (raw-bit identity), to avoid re-emitting.
    seen: Vec<Vec<u8>>,
}

impl EotChannelDecoder {
    /// `input_rate` is any capture rate >= [`CHANNEL_RATE`]; a non-integer
    /// multiple is resampled by the DDC. `freq_offset_hz` is the EOT channel
    /// center relative to the capture center (0 if already centered on the
    /// carrier).
    pub fn new(input_rate: f64, freq_offset_hz: f64) -> Result<Self, String> {
        let ddc = if (input_rate - CHANNEL_RATE).abs() < 1e-6 && freq_offset_hz.abs() < 1e-6 {
            None
        } else {
            Some(Ddc::new(
                input_rate,
                CHANNEL_RATE,
                freq_offset_hz,
                CHANNEL_PASSBAND_HZ,
            )?)
        };
        Ok(Self {
            ddc,
            demod: demod::FskDemod::new(),
            chips: Vec::new(),
            channel_buf: Vec::new(),
            seen: Vec::new(),
        })
    }

    /// Feed capture IQ; returns newly completed EOT/HOT frames.
    pub fn process(&mut self, input: &[Complex<f32>]) -> Vec<EotDecodedFrame> {
        let channel: &[Complex<f32>] = match &mut self.ddc {
            Some(ddc) => {
                self.channel_buf.clear();
                ddc.process(input, &mut self.channel_buf);
                &self.channel_buf
            }
            None => input,
        };
        self.demod.process(channel, &mut self.chips);

        // Try both Manchester pairing phases; the frame-sync hunt rejects the
        // wrong one (its 17-bit pattern won't appear under bad pairing).
        let mut out = Vec::new();
        for phase in 0..2usize {
            let bits = demod::manchester_decode(&self.chips, phase);
            for frame in scan_bits(&bits) {
                // Rebuild the raw packet bits for dedup + the `raw` field.
                let packet_bits = packet_bits_of(&frame);
                if !self.seen.contains(&packet_bits) {
                    self.seen.push(packet_bits.clone());
                    out.push(EotDecodedFrame { frame, packet_bits });
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

/// Reconstruct the 74 on-air packet bits from a parsed frame, for the `raw`
/// field and dedup. (The frame already fully determines them.)
fn packet_bits_of(f: &EotFrame) -> Vec<u8> {
    let mut p = vec![0u8; frame::PACKET_BITS];
    p[0..11].copy_from_slice(&frame::FRAME_SYNC);
    // chaining (MSB-first as parsed)
    p[11] = (f.chaining >> 1) & 1;
    p[12] = f.chaining & 1;
    set_rev(&mut p, 13, 15, f.battery_condition as u32);
    set_rev(&mut p, 15, 18, f.message_type as u32);
    set_rev(&mut p, 18, 35, f.unit_addr);
    set_rev(&mut p, 35, 42, f.pressure_psi as u32);
    // battery charge raw isn't stored; reconstruct nearest raw from pct.
    let charge_raw = ((f.battery_charge_pct as f32 / 100.0) * 127.0).round() as u32;
    set_rev(&mut p, 42, 49, charge_raw);
    p[49] = f.spare;
    p[50] = f.valve_circuit;
    p[51] = f.conf_indicator;
    p[52] = f.turbine;
    p[53] = f.motion;
    p[54] = f.marker_light_batt;
    p[55] = f.marker_light;
    // Recompute the ciphered check so raw round-trips through the BCH verify.
    let check = bch::ciphered_check(&p[frame::DATA_START..frame::DATA_END]);
    p[frame::DATA_END..frame::PACKET_BITS].copy_from_slice(&check);
    p
}

/// Write `value` into `bits[start..end]` LSB-first (inverse of the field_rev
/// read in `frame.rs`).
fn set_rev(bits: &mut [u8], start: usize, end: usize, value: u32) {
    for i in 0..(end - start) {
        bits[start + i] = ((value >> i) & 1) as u8;
    }
}

/// Convert a decoded EOT/HOT frame into the normalized bus message.
///
/// `kind` is `"eot"` (rear-to-front telemetry) or `"hot"` (front-to-rear
/// command), selected by `is_hot`. `details` is the [`EotFrame`] JSON.
/// `decode.crc_ok` is set from the BCH check. `raw` carries the packed packet
/// bits (one byte per bit).
pub fn to_message(
    f: &EotDecodedFrame,
    frequency_hz: u64,
    level_dbfs: f32,
    is_hot: bool,
    source: Provenance,
) -> Message {
    let kind = if is_hot { "hot" } else { "eot" }.to_string();
    let details = serde_json::to_value(&f.frame).unwrap_or(serde_json::Value::Null);
    Message {
        mode: Mode::Eot,
        timestamp: Utc::now(),
        frequency_hz,
        signal: SignalQuality {
            rssi_db: Some(level_dbfs),
            ..Default::default()
        },
        decode: DecodeQuality {
            crc_ok: f.frame.bch_ok,
            fec_corrected: None,
            errors: None,
        },
        body: MessageBody::Eot { kind, details },
        raw: Some(f.packet_bits.clone()),
        source,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn params_are_documented_values() {
        assert_eq!(params::BAUD, 1200.0);
        assert_eq!(params::FREQ_EOT_TO_HOT, 457_937_500);
        assert_eq!(params::FREQ_HOT_TO_EOT, 452_937_500);
    }

    #[test]
    fn channel_rate_is_integer_chip_multiple() {
        let samples_per_chip = CHANNEL_RATE / demod::CHIP_RATE;
        assert_eq!(
            samples_per_chip.fract(),
            0.0,
            "{samples_per_chip} samples/chip"
        );
        let min_rate = 2.0 * CHANNEL_PASSBAND_HZ;
        assert!(CHANNEL_RATE >= min_rate, "{CHANNEL_RATE} < {min_rate}");
    }

    #[test]
    fn hunt_pattern_is_cited_search_run() {
        // PyEOT/EOTDecode hunt for "10101011100010010".
        let expect: Vec<u8> = "10101011100010010".bytes().map(|b| b - b'0').collect();
        assert_eq!(hunt_pattern(), expect);
    }
}
