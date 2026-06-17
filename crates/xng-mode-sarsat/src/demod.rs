//! COSPAS-SARSAT First-Generation Beacon demodulator (IQ → biphase half-symbols).
//!
//! Input: complex channel IQ at [`crate::CHANNEL_RATE`] after the DDC has mixed
//! the beacon to baseband. The signal is biphase-L (Manchester) phase
//! modulation at ±1.1 rad, 400 bps (C/S T.001 §2).
//!
//! Chain (mirrors the discriminator/timing-recovery pattern of the AIS and
//! ACARS demods, adapted to phase-shift biphase-L):
//!
//!  1. **Carrier recovery.** Because the phase deviation is ±1.1 rad (not
//!     ±π/2), the modulated carrier keeps a non-zero mean component
//!     (`A·cos(1.1)·e^{jθ_carrier}`). A one-pole complex average of the input
//!     tracks that residual carrier (frequency offset + phase), exactly the
//!     role the 160 ms unmodulated carrier preamble plays in a real receiver.
//!  2. **Phase detect.** Derotate each sample by the carrier estimate; the
//!     residual phase `arg(s·conj(carrier))` is `±1.1·m(n)` — the biphase
//!     level.
//!  3. **Half-symbol + timing recovery.** Integrate the level over half-bit
//!     windows; biphase-L guarantees a transition at every bit centre, which a
//!     zero-crossing timing loop locks to (same gimbal as the AIS bit-edge
//!     loop). Each half-bit window yields one half-symbol (`1` for +1.1 rad,
//!     `0` for −1.1 rad).
//!
//! The Manchester (biphase-L) pairing and bit/frame sync run in `lib.rs`, which
//! tries both half-symbol phase alignments — more robust than committing to a
//! pairing here.

use crate::CHANNEL_RATE;
use num_complex::Complex;

/// Data rate (bits/sec).
const BAUD: f64 = 400.0;
/// Samples per half-bit (one biphase symbol).
const SAMPLES_PER_HALF: f64 = (CHANNEL_RATE / BAUD) / 2.0;
/// Carrier-recovery one-pole factor (tracks frequency/phase offset).
const CARRIER_ALPHA: f32 = 0.02;
/// Timing-loop gain applied at each mid-symbol zero crossing.
const TIMING_GAIN: f64 = 0.10;
/// Channel-power smoothing for the level estimate.
const LEVEL_ALPHA: f32 = 0.005;

/// Recovers the stream of biphase half-symbols from channel IQ.
pub struct BiphaseDemod {
    /// Tracked carrier vector (frequency offset + phase + residual amplitude).
    carrier: Complex<f32>,
    /// Half-bit timing phase, in samples; wraps at SAMPLES_PER_HALF.
    timing: f64,
    /// Integrated residual phase over the current half-bit window.
    acc: f32,
    /// Sign of the previous sample's residual phase (for zero-cross timing).
    prev_sign: f32,
    /// Smoothed channel power.
    level: f32,
}

impl BiphaseDemod {
    pub fn new() -> Self {
        Self {
            carrier: Complex::new(0.0, 0.0),
            timing: 0.0,
            acc: 0.0,
            prev_sign: 0.0,
            level: 0.0,
        }
    }

    /// Feed channel IQ; append biphase **half-symbols** (`1` = +1.1 rad,
    /// `0` = −1.1 rad) to `halves`.
    pub fn process(&mut self, input: &[Complex<f32>], halves: &mut Vec<u8>) {
        for &x in input {
            self.level += LEVEL_ALPHA * (x.norm_sqr() - self.level);

            // Track the residual carrier (modulation mean is non-zero at ±1.1).
            self.carrier += (x - self.carrier) * CARRIER_ALPHA;

            // Residual phase after derotation = ±1.1·level.
            let resid = x * self.carrier.conj();
            let phase = resid.arg();
            let sign = if phase < 0.0 { -1.0 } else { 1.0 };

            // Mid-symbol transitions cross zero; nudge timing toward them.
            if self.prev_sign != 0.0 && sign != self.prev_sign {
                let sph = SAMPLES_PER_HALF;
                let err = self.timing - (self.timing / sph).round() * sph;
                self.timing -= TIMING_GAIN * err;
            }
            self.prev_sign = sign;

            self.acc += phase;
            self.timing += 1.0;
            if self.timing >= SAMPLES_PER_HALF {
                self.timing -= SAMPLES_PER_HALF;
                halves.push((self.acc >= 0.0) as u8);
                self.acc = 0.0;
            }
        }
    }

    /// Smoothed channel power in dBFS.
    pub fn level_dbfs(&self) -> f32 {
        10.0 * self.level.max(1e-12).log10()
    }
}

impl Default for BiphaseDemod {
    fn default() -> Self {
        Self::new()
    }
}
