//! ADS-L 868 MHz physical-layer demodulator (2-FSK → frame bytes).
//!
//! ADS-L on SRD860 is 2-FSK at **100 kbit/s** with **±50 kHz** deviation
//! (SoftRF `adsl_proto_desc`: `RF_MODULATION_TYPE_2FSK`,
//! `RF_BITRATE_100KBPS`, `RF_FREQUENCY_DEVIATION_50KHZ`). The line code is
//! **IEEE Manchester** (`RF_WHITENING_MANCHESTER`): every data bit is sent
//! as two chips — data `0` → chips `1,0`; data `1` → chips `0,1` (the
//! SoftRF `ManchesterEncode` table: nibble `0000`→`10101010`,
//! `1111`→`01010101`). The whole on-air payload is **FSK-inverted**
//! (`RF_PAYLOAD_INVERTED`), so a transmitted "high tone" chip is logical 0.
//!
//! On-air structure (SoftRF `ADSL.h`):
//!
//! ```text
//! preamble 0x55  | 8-byte sync word | Version + 20-byte payload + 3 CRC
//!  (alt. chips)  |  (Manchester of   (Manchester-coded data bytes,
//!                |   F5 72 4B 18)     the whole stream FSK-inverted)
//! ```
//!
//! The 8 sync bytes `55 99 95 A6 9A 65 A9 6A` are the *chip* pattern that
//! Manchester-encodes the 4 data bytes `F5 72 4B 18`; we correlate against
//! the chip pattern directly (it is unaffected by data inversion because we
//! search both polarities).
//!
//! Chain: per-sample frequency discriminator → slow DC tracker (carrier
//! offset) → chip-rate integrate-and-dump with zero-crossing timing
//! recovery → chip stream → sync-word correlation (both polarities) →
//! Manchester chip-pair → data-bit decode → wire bytes (MSB-first) →
//! [`crate::Frame::parse`].
//!
//! Self-generated modulate→demod path: see PROVENANCE.md. The *decode* core
//! (`Frame::parse` / `IConspicuity`) remains anchored by the independent
//! `decode_vectors` oracle; this module only adds the IQ front-end.

use crate::CHANNEL_RATE;
use num_complex::Complex;

/// Raw data bit rate (bits/s) — Manchester doubles this to the chip rate.
pub const BAUD: f64 = 100_000.0;
/// On-air Manchester chip rate (chips/s).
pub const CHIP_RATE: f64 = 2.0 * BAUD;
/// FSK frequency deviation (Hz), one-sided.
pub const DEVIATION_HZ: f64 = 50_000.0;
/// Samples per chip at [`CHANNEL_RATE`].
pub const SAMPLES_PER_CHIP: usize = (CHANNEL_RATE / CHIP_RATE) as usize;

/// 8-byte ADS-L sync word (the on-air chip pattern), MSB-first per byte.
pub const SYNC_CHIPS: [u8; 8] = [0x55, 0x99, 0x95, 0xA6, 0x9A, 0x65, 0xA9, 0x6A];

/// Number of wire bytes the framer hands to [`crate::Frame::parse`]:
/// Version(1) + payload(20) + CRC(3).
pub const FRAME_BYTES: usize = 1 + crate::PAYLOAD_BYTES + 3;

/// Timing-loop gain (fraction of phase error applied per zero crossing).
const TIMING_GAIN: f64 = 0.15;
/// Carrier-offset (discriminator DC) tracking factor.
const FREQ_ALPHA: f32 = 0.0005;
/// Channel-power smoothing for the level estimate.
const LEVEL_ALPHA: f32 = 0.005;

/// 2-FSK demodulator with Manchester + sync framing. Streams chips and
/// emits complete wire-byte frames as the sync word is found.
pub struct FskDemod {
    prev_sample: Complex<f32>,
    prev_disc: f32,
    /// Discriminator DC estimate (carrier frequency offset).
    freq_offset: f32,
    /// Chip-timing phase in samples; wraps at [`SAMPLES_PER_CHIP`].
    timing: f64,
    /// Discriminator integrator over the current chip window.
    acc: f32,
    prev_disc_sign: i8,
    /// Smoothed channel power.
    level: f32,
    /// Rolling chip history for sync-word correlation (64 chips = 8 bytes).
    chip_hist: u64,
    /// Chips accumulated since the last sync hit (drives byte capture).
    capturing: Option<CaptureState>,
}

struct CaptureState {
    /// True if the sync word matched the inverted chip polarity (so data
    /// chips must be inverted before Manchester decode).
    inverted: bool,
    /// Decoded data bits collected after the sync word.
    bits: Vec<u8>,
    /// Pending Manchester chip (Some when one chip of a pair is buffered).
    pending_chip: Option<u8>,
}

impl CaptureState {
    fn new(inverted: bool) -> Self {
        Self {
            inverted,
            bits: Vec::with_capacity(FRAME_BYTES * 8),
            pending_chip: None,
        }
    }
}

impl FskDemod {
    pub fn new() -> Self {
        // Integer samples-per-chip keeps the timing recovery exact.
        assert!(
            (CHANNEL_RATE / CHIP_RATE).fract().abs() < 1e-9,
            "CHANNEL_RATE must be an integer multiple of the chip rate"
        );
        Self {
            prev_sample: Complex::new(0.0, 0.0),
            prev_disc: 0.0,
            freq_offset: 0.0,
            timing: 0.0,
            acc: 0.0,
            prev_disc_sign: 1,
            level: 0.0,
            chip_hist: 0,
            capturing: None,
        }
    }

    /// Feed channel IQ; return any complete wire-byte frames (Version +
    /// payload + CRC, exactly [`FRAME_BYTES`] long) ready for
    /// [`crate::Frame::parse`].
    pub fn process(&mut self, input: &[Complex<f32>]) -> Vec<Vec<u8>> {
        let mut frames = Vec::new();
        for &x in input {
            self.level += LEVEL_ALPHA * (x.norm_sqr() - self.level);

            let raw = (x * self.prev_sample.conj()).arg();
            self.prev_sample = x;
            self.freq_offset += FREQ_ALPHA * (raw - self.freq_offset);
            let disc = raw - self.freq_offset;

            // Zero crossings of the discriminator mark chip boundaries.
            let sign = if disc < 0.0 { -1 } else { 1 };
            if sign != self.prev_disc_sign && disc != 0.0 && self.prev_disc != 0.0 {
                let spc = SAMPLES_PER_CHIP as f64;
                let err = self.timing - (self.timing / spc).round() * spc;
                self.timing -= TIMING_GAIN * err;
            }
            self.prev_disc_sign = sign;
            self.prev_disc = disc;

            self.acc += disc;
            self.timing += 1.0;
            if self.timing >= SAMPLES_PER_CHIP as f64 {
                self.timing -= SAMPLES_PER_CHIP as f64;
                // Positive frequency (high tone) → chip 1 before inversion.
                let chip: u8 = if self.acc >= 0.0 { 1 } else { 0 };
                self.acc = 0.0;
                self.on_chip(chip, &mut frames);
            }
        }
        frames
    }

    /// Process one demodulated chip: drive sync search and byte capture.
    fn on_chip(&mut self, chip: u8, frames: &mut Vec<Vec<u8>>) {
        if let Some(done) = self.feed_capture(chip) {
            if let Some(bytes) = done {
                frames.push(bytes);
            }
            return;
        }

        // Sync correlation: shift chip into the 64-chip history and test
        // both polarities against the 64-bit sync chip pattern.
        self.chip_hist = (self.chip_hist << 1) | (chip as u64);
        let sync = sync_word_u64();
        let direct = (self.chip_hist ^ sync).count_ones();
        let inverted = (self.chip_hist ^ !sync).count_ones();
        // Allow a few chip errors so a noisy sync still latches.
        const TOL: u32 = 4;
        if direct <= TOL {
            self.capturing = Some(CaptureState::new(false));
        } else if inverted <= TOL {
            self.capturing = Some(CaptureState::new(true));
        }
    }

    /// Returns `None` while not capturing; `Some(None)` while a capture is
    /// in progress; `Some(Some(bytes))` when a full frame is complete.
    fn feed_capture(&mut self, chip: u8) -> Option<Option<Vec<u8>>> {
        let st = self.capturing.as_mut()?;
        // The ADS-L payload is always FSK-inverted on air (RF_PAYLOAD_INVERTED);
        // additionally undo any carrier-polarity flip detected at sync.
        let chip = chip ^ 1 ^ (st.inverted as u8);
        match st.pending_chip.take() {
            None => st.pending_chip = Some(chip),
            Some(first) => {
                // IEEE Manchester: data 0 → chips (1,0); data 1 → chips (0,1).
                let bit = match (first, chip) {
                    (1, 0) => 0u8,
                    (0, 1) => 1u8,
                    // Illegal chip pair (no mid-chip transition): treat the
                    // majority as the data bit so a single glitch still
                    // decodes; framing CRC catches genuine corruption.
                    _ => first ^ 1,
                };
                st.bits.push(bit);
            }
        }

        if st.bits.len() == FRAME_BYTES * 8 {
            let bytes = pack_msb_first(&st.bits);
            self.capturing = None;
            return Some(Some(bytes));
        }
        Some(None)
    }

    /// Smoothed channel power in dBFS.
    pub fn level_dbfs(&self) -> f32 {
        10.0 * self.level.max(1e-12).log10()
    }
}

impl Default for FskDemod {
    fn default() -> Self {
        Self::new()
    }
}

/// Pack the 8-byte sync chip pattern (MSB-first per byte) into a 64-bit word.
fn sync_word_u64() -> u64 {
    let mut w = 0u64;
    for &b in &SYNC_CHIPS {
        w = (w << 8) | (b as u64);
    }
    w
}

/// Pack a bit slice (MSB-first within each byte) into bytes.
fn pack_msb_first(bits: &[u8]) -> Vec<u8> {
    bits.chunks_exact(8)
        .map(|c| {
            c.iter()
                .enumerate()
                .fold(0u8, |b, (i, &v)| b | (v << (7 - i)))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sync_chip_pattern_decodes_to_f5724b18() {
        // The 8 chip-bytes Manchester-encode the 4 data bytes F5 72 4B 18.
        // Decode each chip pair (MSB-first) back to data bits and repack.
        let mut bits = Vec::new();
        for &byte in &SYNC_CHIPS {
            // 8 chips per byte = 4 data bits.
            for pair in 0..4 {
                let hi = (byte >> (7 - pair * 2)) & 1;
                let lo = (byte >> (6 - pair * 2)) & 1;
                let bit = match (hi, lo) {
                    (1, 0) => 0u8,
                    (0, 1) => 1u8,
                    _ => panic!("sync chip pair not Manchester-legal"),
                };
                bits.push(bit);
            }
        }
        let data = pack_msb_first(&bits);
        assert_eq!(data, vec![0xF5, 0x72, 0x4B, 0x18]);
    }

    #[test]
    fn samples_per_chip_is_integer_and_positive() {
        // Recompute from the rate constants so the check is a real assertion,
        // not a const folded away at compile time.
        let spc = CHANNEL_RATE / CHIP_RATE;
        assert!(spc >= 2.0, "need ≥2 samples/chip for the discriminator");
        assert!(
            spc.fract().abs() < 1e-9,
            "rate must be an integer chip multiple"
        );
        assert_eq!(SAMPLES_PER_CHIP, spc as usize);
    }
}
