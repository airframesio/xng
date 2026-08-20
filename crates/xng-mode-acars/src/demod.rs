//! ACARS MSK demodulator.
//!
//! Input: complex channel IQ at 24 kHz (10 samples/bit at 2400 bd), AM
//! carrier with audio MSK tones at 1200/2400 Hz.
//!
//! Chain: AM envelope (|IQ|, immune to carrier frequency offset) → DC block
//! (EMA highpass removes the carrier level) → complex mix by −1800 Hz (the
//! tone midpoint) → 1300 Hz lowpass (wide enough for the MSK main lobe at
//! ±600 Hz ± 2400 bd transitions; rejects the −3000/−4200 Hz mixing images)
//! → per-sample frequency discriminator (1200 Hz → −600 Hz, 2400 Hz →
//! +600 Hz) → per-bit integrate-and-dump with zero-crossing timing recovery
//! → differential decode (ARINC 618 §4.4.2: 1200 Hz = bit change, 2400 Hz =
//! no change; all-ones pre-key radiates continuous 2400 Hz).
//!
//! The differential mapping makes the bit stream polarity-ambiguous at
//! start-up (the initial state is unknown when we tune in mid-burst), so
//! the deframer hunts for the sync pattern in both polarities.

use crate::CHANNEL_RATE;
use num_complex::Complex;
use xng_dsp::{lowpass_taps, Fir, Nco};

const BAUD: f64 = 2400.0;
const SAMPLES_PER_BIT: usize = 10;
const TONE_MID_HZ: f64 = 1800.0;
const AUDIO_LPF_CUTOFF: f64 = 1300.0;
/// Sizing here is set by **sensitivity**, not by image rejection: 51 taps
/// already puts the −3000 Hz mixing image 83 dB down, far below the capture's
/// noise floor, yet decode yield keeps improving well past that.
///
/// CRC-OK out of **400 bursts** per σ, `examples/sensitivity.rs` against the
/// real demod, only `AUDIO_LPF_TAPS` varied:
///
/// | taps | 0.18 | 0.20 | 0.22 |
/// |------|------|------|------|
/// |   51 |  336 |  262 |  133 |
/// |   81 |  353 |  269 |  150 |
/// |  101 |  356 |  282 |  172 |
/// |  121 |  361 |  295 |  175 |
///
/// Monotone in taps across all three discriminating columns, and 121 over 51
/// is +7%/+13%/+32% — far outside run-to-run scatter. σ = 0.15 and σ = 0.25
/// are omitted because they are saturated and floored respectively; they move
/// with the seed rather than with the tap count.
///
/// The trial count matters here. This table previously reported 60 trials,
/// where reruns of the *same* build span 52–56 / 41–49 / 20–29 — a spread as
/// large as the differences being attributed to tap count. The conclusion was
/// right but the evidence did not support it; 400 trials does.
///
/// This convolution used to be ~40% of pipeline CPU, which made trimming it
/// tempting; the contiguous-window `Fir` rewrite made it ~2× cheaper instead,
/// so restoring 101 → 121 taps costs ~0.2 percentage points of one core across
/// 16 channels (`examples/cpu_budget.rs`: the audio-LPF line is 1.21% at 121
/// taps and scales with tap count). Buying the top of that table for that
/// price is the right trade. Do not trim it without re-running the sweep
/// against the REAL demod — a reimplemented chain gives misleadingly flat
/// results.
const AUDIO_LPF_TAPS: usize = 121;
/// Envelope DC tracker: fc ≈ alpha·fs/2π ≈ 19 Hz, settles within the pre-key.
const DC_ALPHA: f32 = 0.005;
/// Timing loop gain (fraction of the phase error applied per zero crossing).
const TIMING_GAIN: f64 = 0.15;
/// Envelope power smoothing factor for the level estimate.
const LEVEL_ALPHA: f32 = 0.005;
/// Noise-floor EMA factor (slower than the level tracker).
const NOISE_ALPHA: f32 = 0.002;
/// A sample more than this multiple above the running floor is treated as
/// signal (a burst) and excluded from the noise EMA, so a long transmission
/// can't drag the floor up to the carrier level. The pure-noise tail above
/// this is negligible (~e^-8), so the silence estimate stays ~unbiased.
const NOISE_GATE: f32 = 8.0;
/// Per-(above-gate-)sample multiplicative up-creep that lets a too-low frozen
/// floor re-converge (see `process`). Deliberately tiny and decoupled from
/// `NOISE_ALPHA`: this fires on every in-burst sample at the 24 kHz channel
/// rate, so at 2e-5 a worst-case ~250 ms contiguous burst lifts the floor only
/// ~0.5 dB (negligible, reporting-only), while a genuinely stuck floor still
/// recovers over a few seconds of channel activity.
const NOISE_RECOVER: f32 = 2.0e-5;

pub struct MskDemod {
    mix: Nco,
    lpf: Fir,
    mixed: Vec<Complex<f32>>,
    filtered: Vec<Complex<f32>>,
    prev_sample: Complex<f32>,
    prev_disc: f32,
    /// Bit-timing phase in samples, advances by 1 per sample, wraps at
    /// SAMPLES_PER_BIT (the bit boundary).
    timing: f64,
    /// Discriminator integrator over the current bit window.
    acc: f32,
    /// Differential decode state (last emitted bit).
    prev_bit: u8,
    /// Smoothed envelope power for RSSI.
    level: f32,
    /// Envelope DC (carrier level) tracker.
    dc: f32,
    /// Noise-floor estimate: envelope power EMA over inter-burst silence
    /// (gated by `NOISE_GATE`). 0.0 until the first sample seeds it.
    noise: f32,
}

impl MskDemod {
    pub fn new() -> Self {
        assert_eq!(CHANNEL_RATE as usize, (BAUD as usize) * SAMPLES_PER_BIT);
        Self {
            mix: Nco::new(TONE_MID_HZ, CHANNEL_RATE),
            lpf: Fir::new(lowpass_taps(AUDIO_LPF_CUTOFF / CHANNEL_RATE, AUDIO_LPF_TAPS)),
            mixed: Vec::new(),
            filtered: Vec::new(),
            prev_sample: Complex::new(0.0, 0.0),
            prev_disc: 0.0,
            timing: 0.0,
            acc: 0.0,
            prev_bit: 1, // pre-key state is all ones
            level: 0.0,
            dc: 0.0,
            noise: 0.0,
        }
    }

    /// Feed channel IQ; append hard bit decisions to `bits`.
    pub fn process(&mut self, input: &[Complex<f32>], bits: &mut Vec<u8>) {
        // AM envelope → DC block → complex, mixed down by the tone midpoint.
        self.mixed.clear();
        for x in input {
            let env = x.norm();
            let p = env * env;
            self.level += LEVEL_ALPHA * (p - self.level);
            self.dc += DC_ALPHA * (env - self.dc);
            // Noise floor: seed on the first sample, then track only samples
            // near the floor (silence); freeze on bursts. A high seed (tuned
            // in mid-burst) self-corrects, since silence samples fall well
            // below the gate and pull it back down.
            if self.noise == 0.0 {
                self.noise = p;
            } else if p < self.noise * NOISE_GATE {
                self.noise += NOISE_ALPHA * (p - self.noise);
            } else {
                // Tiny up-creep on above-gate samples so a too-LOW floor can
                // re-converge — an anomalously low first sample (a deep fade)
                // would otherwise leave every later silence sample above the
                // gate and freeze the floor low for the whole session,
                // inflating snr_db. NOISE_RECOVER is sized for the per-sample
                // channel rate (see its doc); a correct floor is dominated by
                // the silence EMA above and only creeps <1 dB on long bursts.
                self.noise *= 1.0 + NOISE_RECOVER;
            }
            self.mixed.push(Complex::new(env - self.dc, 0.0));
        }
        self.mix.mix(&mut self.mixed);

        self.filtered.clear();
        self.lpf.process(&self.mixed, &mut self.filtered);

        for &y in &self.filtered {
            // Frequency discriminator: the imaginary part of y·conj(prev) is
            // |y||prev|·sin(Δφ) — the classic atan2-free discriminator. On a
            // clean signal the tones sit ±600 Hz off the 1800 Hz midpoint,
            // i.e. ±9°/sample, where sin(Δφ) tracks Δφ to within 0.4%.
            //
            // That 0.4% is the NOISELESS figure and it is not the whole story:
            // under noise Δφ ranges over the full circle, where sin saturates
            // and past ±90° actually decreases. Sign is preserved, so nothing
            // breaks — it behaves as a soft limiter, not as an equal. It is a
            // deliberate trade, and it is slightly lossy. 400 bursts per σ
            // (`examples/sensitivity.rs`), identical noise:
            //
            //   σ         0.15  0.18  0.20  0.22  0.25
            //   atan2      394   362   296   179    23
            //   this        393   361   295   175    20
            //
            // ~1-2% down at the cliff, for a discriminator measured at 4.1×
            // cheaper than atan2 on this machine (116.4 ms vs 28.6 ms per 20 M
            // samples; ~0.17 points of one core across 16 channels). Worth it,
            // but state it rather than imply equivalence — and note the ratio
            // is a few ×, not the orders of magnitude an earlier revision of
            // this comment claimed.
            //
            // The divide is load-bearing and NOT optional. The raw cross
            // product is amplitude-weighted where atan2 is not: under noise
            // |y| fluctuates sample to sample, so loud-but-noisy samples
            // dominate the bit accumulator and marginal frames are lost.
            // Deleting `/ mag` still passes every test in this repository
            // while giving 382 / 295 / 159 / 46 / 0 against the row above —
            // less than a third of the yield at σ = 0.22 and nothing at all at
            // σ = 0.25. `examples/sensitivity.rs` is the only thing that
            // catches that, which is why it is committed.
            //
            // `norm_sqr().sqrt()` rather than `norm()`: the latter is
            // `f32::hypot`, an out-of-line libm call whose overflow guarding
            // buys nothing here (|d| = |y||prev| sits far inside f32 range).
            // 1.5× faster, and bit-identical decode yield.
            let d = y * self.prev_sample.conj();
            let mag = d.norm_sqr().sqrt();
            let disc = if mag > 0.0 { d.im / mag } else { 0.0 };
            self.prev_sample = y;

            // Timing: tone transitions cross zero at bit boundaries
            // (timing == 0 mod SAMPLES_PER_BIT). Nudge the phase so
            // crossings align with the boundary.
            if disc != 0.0 && self.prev_disc != 0.0 && (disc < 0.0) != (self.prev_disc < 0.0) {
                let spb = SAMPLES_PER_BIT as f64;
                let err = self.timing - (self.timing / spb).round() * spb;
                self.timing -= TIMING_GAIN * err;
            }
            self.prev_disc = disc;

            self.acc += disc;
            self.timing += 1.0;
            if self.timing >= SAMPLES_PER_BIT as f64 {
                self.timing -= SAMPLES_PER_BIT as f64;
                // Mean frequency < 0 → 1200 Hz tone → bit change.
                let change = (self.acc < 0.0) as u8;
                self.acc = 0.0;
                self.prev_bit ^= change;
                bits.push(self.prev_bit);
            }
        }
    }

    /// Smoothed envelope level in dBFS.
    pub fn level_dbfs(&self) -> f32 {
        10.0 * self.level.max(1e-12).log10()
    }

    /// Noise-floor estimate in dBFS (envelope power over silence).
    pub fn noise_dbfs(&self) -> f32 {
        10.0 * self.noise.max(1e-12).log10()
    }
}

impl Default for MskDemod {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Deterministic Gaussian generator (Box-Muller over xorshift) — no rand dep.
    struct Gauss(u64);
    impl Gauss {
        fn u(&mut self) -> f64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            ((self.0 >> 11) as f64 + 1.0) / ((1u64 << 53) as f64 + 2.0)
        }
        fn z(&mut self) -> f32 {
            let (u1, u2) = (self.u(), self.u());
            ((-2.0 * u1.ln()).sqrt() * (std::f64::consts::TAU * u2).cos()) as f32
        }
    }

    // The noise-floor estimate over pure complex AWGN must converge to the
    // analytic envelope power 2·sigma^2 (independent ground truth — NOT a
    // demod loopback), and scale correctly in dB when sigma changes.
    #[test]
    fn noise_floor_tracks_known_awgn_power() {
        let floor_dbfs = |sigma: f32| -> f32 {
            let mut g = Gauss(0xC0FF_EE12_3456_789B);
            let input: Vec<Complex<f32>> =
                (0..80_000).map(|_| Complex::new(g.z() * sigma, g.z() * sigma)).collect();
            let mut d = MskDemod::new();
            let mut bits = Vec::new();
            d.process(&input, &mut bits);
            d.noise_dbfs()
        };

        let sigma = 0.03f32;
        let expected = 10.0 * (2.0 * sigma * sigma).log10();
        let got = floor_dbfs(sigma);
        assert!((got - expected).abs() < 1.0, "floor {got} dBFS vs analytic {expected} dBFS");

        // Doubling sigma raises the measured floor by 10·log10(4) ≈ 6.02 dB.
        let louder = floor_dbfs(sigma * 2.0);
        assert!((louder - got - 6.02).abs() < 1.0, "Δ {} dB vs 6.02", louder - got);
    }
}
