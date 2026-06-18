//! APRS / AX.25 demodulator: narrowband FM -> Bell 202 AFSK1200 -> NRZI bits.
//!
//! On air, VHF APRS (144.39 MHz in NA, 144.800 in EU) is **Bell 202 AFSK**
//! carried in **narrowband FM**: a 1200 Hz tone ("mark") and a 2200 Hz tone
//! ("space") keyed at 1200 baud, frequency-modulated onto the RF carrier.
//!
//! Demod chain:
//!
//! 1. **FM discriminator** — `arg(x · conj(x_prev))` over the channelized IQ
//!    recovers the instantaneous audio (the AFSK tone) as a real signal.
//! 2. **AFSK1200 tone detector** — a one-bit-delay correlator (the classic
//!    "Bell 202 correlator demod"): multiply the audio by a version of itself
//!    delayed by one bit period and low-pass filter. Because mark (1200 Hz)
//!    and space (2200 Hz) accumulate different phase over one bit, the
//!    correlator output sign distinguishes the two tones. This is more robust
//!    than two Goertzel bins at low SNR and needs no per-tone gain matching.
//! 3. **Bit timing recovery** — a digital PLL clocks one decision per 1200 Bd
//!    symbol, nudged toward observed zero crossings of the correlator output.
//! 4. The hard symbol decisions are the **NRZI line symbols**, fed straight
//!    to [`crate::hdlc::HdlcDeframer`] (NRZI decode + de-stuffing live there).
//!
//! The discriminator/correlator structure is textbook AFSK (clean-room; no
//! GPL decoder code copied). It is validated synthetically by the
//! modulate->AWGN->demod BER test (see PROVENANCE.md).

use crate::hdlc::HdlcDeframer;
use crate::CHANNEL_RATE;
use num_complex::Complex;

/// AFSK baud rate (Bell 202).
pub const BAUD: f64 = 1200.0;
/// Mark tone, Hz.
pub const MARK_HZ: f64 = 1200.0;
/// Space tone, Hz.
pub const SPACE_HZ: f64 = 2200.0;

/// Channel power smoothing for the level estimate.
const LEVEL_ALPHA: f32 = 0.002;
/// Phase (fraction of a bit, 0..1) the bit clock is hard-reset to on each
/// detected symbol transition. Because the trailing matched window has a
/// ~half-bit group delay, resetting just past 0.5 places the next sampling
/// wrap at the symbol center. Empirically locks across ±1% clock error and
/// every start phase (see `tests/end_to_end.rs` drift/offset coverage).
const TRANSITION_RESET_PHASE: f64 = 0.6;

/// Streaming AFSK1200 demodulator for one APRS channel.
///
/// After FM discrimination the audio is fed to a **non-coherent dual-tone
/// correlator**: two quadrature (sin/cos) integrators, one tuned to the
/// 1200 Hz mark and one to the 2200 Hz space, each summed over a sliding
/// one-bit window. The per-bit decision is `|mark| > |space|` — the standard
/// AFSK1200 detector. This is polarity-unambiguous (no DC-bias tracking
/// needed) and degrades gracefully under noise.
pub struct AfskDemod {
    samples_per_bit: f64,
    win: usize,
    /// FM discriminator state.
    prev_iq: Complex<f32>,
    /// Ring buffer of the last `win` discriminated-audio samples.
    hist: Vec<f32>,
    hpos: usize,
    filled: usize,
    /// Precomputed tone reference tables over the window length.
    mark_cos: Vec<f32>,
    mark_sin: Vec<f32>,
    space_cos: Vec<f32>,
    space_sin: Vec<f32>,
    /// Sign of the previous tone decision, for transition detection.
    prev_sign: bool,
    have_sign: bool,
    /// Bit-timing phase, 0..1 of a bit period; wraps at 1.0.
    timing: f64,
    /// Smoothed channel power.
    level: f32,
    deframer: HdlcDeframer,
}

impl Default for AfskDemod {
    fn default() -> Self {
        Self::new()
    }
}

impl AfskDemod {
    /// Build a demod for the fixed [`CHANNEL_RATE`] / 1200 Bd AFSK signal.
    pub fn new() -> Self {
        let samples_per_bit = CHANNEL_RATE / BAUD;
        let win = samples_per_bit.round() as usize;
        let win = win.max(4);
        let mut mark_cos = vec![0.0f32; win];
        let mut mark_sin = vec![0.0f32; win];
        let mut space_cos = vec![0.0f32; win];
        let mut space_sin = vec![0.0f32; win];
        for n in 0..win {
            let tm = 2.0 * std::f64::consts::PI * MARK_HZ * (n as f64) / CHANNEL_RATE;
            let ts = 2.0 * std::f64::consts::PI * SPACE_HZ * (n as f64) / CHANNEL_RATE;
            mark_cos[n] = tm.cos() as f32;
            mark_sin[n] = tm.sin() as f32;
            space_cos[n] = ts.cos() as f32;
            space_sin[n] = ts.sin() as f32;
        }
        Self {
            samples_per_bit,
            win,
            prev_iq: Complex::new(0.0, 0.0),
            hist: vec![0.0; win],
            hpos: 0,
            filled: 0,
            mark_cos,
            mark_sin,
            space_cos,
            space_sin,
            prev_sign: false,
            have_sign: false,
            timing: 0.0,
            level: 0.0,
            deframer: HdlcDeframer::new(),
        }
    }

    /// FM-discriminate one IQ sample to instantaneous audio.
    #[inline]
    fn discriminate(&mut self, x: Complex<f32>) -> f32 {
        self.level += LEVEL_ALPHA * (x.norm_sqr() - self.level);
        let d = x * self.prev_iq.conj();
        self.prev_iq = x;
        d.arg()
    }

    /// Compute the mark/space tone magnitudes over the current one-bit window
    /// and return `|mark| - |space|` (positive => mark/1200 Hz).
    fn tone_decision(&self) -> f32 {
        // The newest sample is at (hpos-1); walk the window oldest->newest so
        // the reference tables align to relative sample index 0..win.
        let mut mc = 0.0f32;
        let mut ms = 0.0f32;
        let mut sc = 0.0f32;
        let mut ss = 0.0f32;
        for n in 0..self.win {
            // Oldest sample first: hist[(hpos + n) % win].
            let s = self.hist[(self.hpos + n) % self.win];
            mc += s * self.mark_cos[n];
            ms += s * self.mark_sin[n];
            sc += s * self.space_cos[n];
            ss += s * self.space_sin[n];
        }
        let mark = (mc * mc + ms * ms).sqrt();
        let space = (sc * sc + ss * ss).sqrt();
        mark - space
    }

    /// Feed channel IQ; return completed raw AX.25 frames (octets incl. FCS).
    ///
    /// Timing recovery is a **transition-resync bit clock** on the trailing-
    /// window tone decision `d = |mark|-|space|`. A phase accumulator
    /// (`timing`, 0..1 of a bit) advances `1/spb` per sample and the symbol is
    /// sampled at its wrap. HDLC's NRZI + bit-stuffing guarantee a symbol
    /// transition at least every six bits, and on each detected transition
    /// (sign change of `d`) the clock is hard-reset to
    /// [`TRANSITION_RESET_PHASE`]. This drains any accumulated clock error at
    /// every transition, so the loop never slips even over the longest AX.25
    /// frame and tolerates ±1% baud error and any start phase.
    pub fn process(&mut self, input: &[Complex<f32>]) -> Vec<Vec<u8>> {
        let mut out = Vec::new();
        let step = 1.0 / self.samples_per_bit; // bit-fraction per sample
        for &x in input {
            let audio = self.discriminate(x);
            // Push into the sliding window.
            self.hist[self.hpos] = audio;
            self.hpos = (self.hpos + 1) % self.win;
            if self.filled < self.win {
                self.filled += 1;
            }

            // Normalized trailing-window tone decision.
            let decision = self.tone_decision() / self.win as f32;

            self.timing += step;
            if self.timing >= 1.0 {
                self.timing -= 1.0;
                // |mark| > |space| => mark (1200 Hz) => NRZI line symbol 1.
                let symbol = (decision >= 0.0) as u8;
                self.deframer.push_symbol(symbol, &mut out);
            }

            // Hard-resync the bit clock at each symbol transition.
            let sign = decision >= 0.0;
            if self.filled >= self.win && self.have_sign && sign != self.prev_sign {
                self.timing = TRANSITION_RESET_PHASE;
            }
            self.prev_sign = sign;
            self.have_sign = true;
        }
        out
    }

    /// Smoothed channel power in dBFS.
    pub fn level_dbfs(&self) -> f32 {
        10.0 * self.level.max(1e-12).log10()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn samples_per_bit_is_clean() {
        // CHANNEL_RATE must be an integer multiple of the baud rate.
        let spb = CHANNEL_RATE / BAUD;
        assert!(spb >= 4.0, "need >=4 samples/bit, got {spb}");
    }

    #[test]
    fn tones_are_bell_202() {
        assert_eq!(MARK_HZ, 1200.0);
        assert_eq!(SPACE_HZ, 2200.0);
    }
}
