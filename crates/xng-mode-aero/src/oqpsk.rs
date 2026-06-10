//! 10.5 kbps A-QPSK (OQPSK) P-channel support (ported from JAERO
//! `oqpskdemodulator.cpp` / `aerol.cpp` OQPSK framing).
//!
//! OQPSK at 5250 symbols/s: I and Q rails each carry 5250 bps with the Q
//! rail offset half a symbol; the combined bit stream is 10500 bps with
//! bits alternating rails. Frames: 64-bit unique word (the 32-bit UW
//! 0xE15AE893 carried on *each* rail, bits interleaved), 16-bit header,
//! 178 dummy bits, then 4992 coded bits (one 64×78 interleaver block).
//!
//! Rail conventions (which rail is "even", per-rail polarity) are
//! resolved by the UW search over all hypotheses, so transmitter
//! conventions self-correct.

use crate::frame::{self, FrameDecoder};
use crate::su;
use num_complex::Complex;
use xng_dsp::Fir;

pub const BIT_RATE: u32 = 10_500;
pub const SYMBOL_RATE: f64 = 5_250.0;
pub const CHANNEL_RATE_HR: f64 = 48_000.0;
const UW32: u32 = frame::UW;
/// Header + dummy section between the UW and the coded block.
pub const HR_SKIP_BITS: usize = 16 + 178;
pub const HR_CODED_BITS: usize = 64 * 78;

/// Strobe point within the 10.5 kHz timing-oscillator cycle (JAERO `ee`).
const STROBE_POINT: f64 = 0.65;
/// MSE below this counts as carrier lock (JAERO `signalthreshold`).
const LOCK_MSE: f32 = 0.5;
/// FFT length for coarse-CFO acquisition while unlocked (JAERO's coarse
/// estimator also works on a 2^14 baseband spectrum).
const ACQ_FFT: usize = 16_384;

/// Direct-form-II-transposed biquad (JAERO `IIR` with 3 a/b points).
struct Biquad {
    b: [f32; 3],
    a: [f32; 2],
    z: [f32; 2],
}

impl Biquad {
    fn new(b: [f32; 3], a: [f32; 2]) -> Self {
        Self { b, a, z: [0.0; 2] }
    }
    fn run(&mut self, x: f32) -> f32 {
        let y = self.b[0] * x + self.z[0];
        self.z[0] = self.b[1] * x - self.a[0] * y + self.z[1];
        self.z[1] = self.b[2] * x - self.a[1] * y;
        y
    }
    fn reset(&mut self) {
        self.z = [0.0; 2];
    }
}

/// Fixed fractional delay with linear interpolation (JAERO `Delay`).
struct FracDelay {
    buf: Vec<f32>,
    pos: usize,
    delay: f64,
}

impl FracDelay {
    fn new(delay: f64) -> Self {
        Self { buf: vec![0.0; delay.ceil() as usize + 2], pos: 0, delay }
    }
    fn run(&mut self, x: f32) -> f32 {
        let n = self.buf.len();
        self.buf[self.pos] = x;
        let i = self.delay.floor() as usize;
        let f = (self.delay - i as f64) as f32;
        let a = self.buf[(self.pos + n - i) % n];
        let b = self.buf[(self.pos + n - i - 1) % n];
        self.pos = (self.pos + 1) % n;
        a * (1.0 - f) + b * f
    }
}

/// Windowed moving average (JAERO `MovingAverage`).
struct MovingAvg {
    buf: Vec<f32>,
    pos: usize,
    sum: f64,
}

impl MovingAvg {
    fn new(len: usize) -> Self {
        Self { buf: vec![0.0; len], pos: 0, sum: 0.0 }
    }
    fn run(&mut self, x: f32) -> f32 {
        self.sum += (x - self.buf[self.pos]) as f64;
        self.buf[self.pos] = x;
        self.pos = (self.pos + 1) % self.buf.len();
        (self.sum / self.buf.len() as f64) as f32
    }
    fn reset(&mut self) {
        self.buf.iter_mut().for_each(|v| *v = 0.0);
        self.sum = 0.0;
    }
}

/// Root-raised-cosine taps (unit energy), `sps` samples per symbol.
pub fn rrc_taps(sps: f64, num_taps: usize, beta: f64) -> Vec<f32> {
    let mid = (num_taps - 1) as f64 / 2.0;
    let mut taps: Vec<f64> = (0..num_taps)
        .map(|n| {
            let t = (n as f64 - mid) / sps; // in symbols
            if t.abs() < 1e-9 {
                1.0 - beta + 4.0 * beta / std::f64::consts::PI
            } else if (t.abs() - 1.0 / (4.0 * beta)).abs() < 1e-9 {
                (beta / std::f64::consts::SQRT_2)
                    * ((1.0 + 2.0 / std::f64::consts::PI)
                        * (std::f64::consts::PI / (4.0 * beta)).sin()
                        + (1.0 - 2.0 / std::f64::consts::PI)
                            * (std::f64::consts::PI / (4.0 * beta)).cos())
            } else {
                let pt = std::f64::consts::PI * t;
                ((pt * (1.0 - beta)).sin() + 4.0 * beta * t * (pt * (1.0 + beta)).cos())
                    / (pt * (1.0 - (4.0 * beta * t).powi(2)))
            }
        })
        .collect();
    let energy: f64 = taps.iter().map(|h| h * h).sum::<f64>().sqrt();
    taps.iter_mut().for_each(|h| *h /= energy);
    taps.into_iter().map(|h| h as f32).collect()
}

/// Coherent OQPSK demod, ported from JAERO `oqpskdemodulator.cpp` (MIT,
/// see PROVENANCE.md): RRC(β=1) matched filter; non-data-aided square-law
/// symbol timing (delay-difference detector → narrow 10.5 kHz resonator →
/// strobed timing oscillator), so the clock acquires independently of the
/// carrier; strobes alternate rails at 10 500/s and consecutive strobes
/// pair into de-offset QPSK points; carrier tracked by the tanh
/// cross-product discriminator `tanh(I_d)·Q_d − tanh(Q)·I` through a
/// 2nd-order loop filter; residual constellation bias removed by a slow
/// moving-average rotation. Coarse CFO (JAERO's FFT estimator) is replaced
/// by a delay-conjugate frequency centroid applied while unlocked.
///
/// The timing resonator and carrier loop-filter coefficients are JAERO's,
/// designed at 48 kHz; `HrChain` always feeds this demod at 48 kHz.
pub struct OqpskDemod {
    fs: f64,
    rrc: Fir,
    filtered: Vec<Complex<f32>>,
    // carrier NCO (estimated offset, Hz, plus a phase trim in radians)
    nco_freq_hz: f64,
    nco_phase: f64,
    phase_trim: f32,
    agc: f32,
    // square-law symbol timing
    d_pow: FracDelay,
    d_t41: FracDelay,
    d_t42: FracDelay,
    d_t8: FracDelay,
    resonator: Biquad,
    st_phase: f64,   // fraction of a 10.5 kHz cycle
    st_freq_hz: f64, // clamped to ±0.1 Hz around nominal
    sig_last: Complex<f32>,
    // strobe pairing + carrier loop
    pair_toggle: bool,
    pt_d: Complex<f32>,
    ct_filter: Biquad,
    bias: MovingAvg,
    mse: f32,
    /// EMA of the 4th power of the unit-normalized pair point: ≈ −1 when
    /// truly locked, ≈ 0 when the constellation is spinning (a spinning
    /// constellation still yields a deceptively low MSE).
    quad: Complex<f32>,
    strobe_point: f64,
    // coarse CFO acquisition (squared-signal two-tone spectrum)
    acq_buf: Vec<Complex<f32>>,
    acq_fft: std::sync::Arc<dyn rustfft::Fft<f32>>,
    acq_spec: Vec<f32>,
    acq_blocks: u32,
    level: f32,
}

impl OqpskDemod {
    // JAERO's filter coefficients are kept verbatim (f64 precision) for
    // traceability even though they truncate to f32.
    #[allow(clippy::excessive_precision)]
    pub fn new(channel_rate: f64) -> Self {
        debug_assert!(
            (channel_rate - CHANNEL_RATE_HR).abs() < 1e-6,
            "OQPSK IIR coefficients are designed for 48 kHz"
        );
        let t = channel_rate / SYMBOL_RATE; // samples per symbol
        Self {
            fs: channel_rate,
            rrc: Fir::new(rrc_taps(t, 55, 1.0)),
            filtered: Vec::new(),
            nco_freq_hz: 0.0,
            nco_phase: 0.0,
            phase_trim: 0.0,
            agc: 1.0,
            d_pow: FracDelay::new(1.0),
            d_t41: FracDelay::new(t / 4.0),
            d_t42: FracDelay::new(t / 4.0),
            d_t8: FracDelay::new(t / 8.0),
            // 10 500 Hz resonator at 48 kHz (JAERO st_iir_resonator).
            resonator: Biquad::new(
                [0.000_327_142_189_395_890_35, 0.0, 0.000_327_142_189_395_890_35],
                [-0.390_052_999_482_108_03, 0.999_345_715_621_208_22],
            ),
            st_phase: 0.0,
            st_freq_hz: BIT_RATE as f64,
            sig_last: Complex::new(0.0, 0.0),
            pair_toggle: false,
            pt_d: Complex::new(0.0, 0.0),
            // Carrier loop filter at 48 kHz (JAERO ct_iir_loopfilter).
            ct_filter: Biquad::new(
                [0.001_027_561_065_367_206_4, 0.002_055_122_130_734_412_8, 0.001_027_561_065_367_206_4],
                [-1.920_738_681_557_713_9, 0.925_092_473_103_063_31],
            ),
            bias: MovingAvg::new(800),
            mse: 100.0,
            quad: Complex::new(0.0, 0.0),
            strobe_point: STROBE_POINT,
            acq_buf: Vec::with_capacity(ACQ_FFT),
            acq_fft: rustfft::FftPlanner::new().plan_fft_forward(ACQ_FFT),
            acq_spec: vec![0.0; ACQ_FFT],
            acq_blocks: 0,
            level: 0.0,
        }
    }

    /// Feed channel IQ; append (soft, hard) bits at 10 500 bps,
    /// alternating rails (Q of the de-offset pair first, then I,
    /// matching JAERO's soft-bit order).
    pub fn process(&mut self, input: &[Complex<f32>], out: &mut Vec<(f32, u8)>) {
        for &raw in input {
            self.level += 0.001 * (raw.norm_sqr() - self.level);

            // Mix down by the tracked carrier estimate.
            let rot = Complex::from_polar(1.0, -self.nco_phase as f32 + self.phase_trim);
            self.nco_phase += std::f64::consts::TAU * self.nco_freq_hz / self.fs;
            if self.nco_phase > std::f64::consts::TAU {
                self.nco_phase -= std::f64::consts::TAU;
            }
            self.filtered.clear();
            self.rrc.process(&[raw * rot], &mut self.filtered);
            let Some(&y0) = self.filtered.first() else { continue };

            // Coarse CFO acquisition (JAERO CoarseFreqEstimate): squaring
            // OQPSK produces spectral lines at 2f0 ± the symbol rate; a
            // two-tone matched search over the smoothed squared-signal
            // spectrum locates 2f0 to bin resolution. Measured on the
            // signal mixed by the frequency estimate alone (the carrier
            // loop's phase-trim FM would smear the lines).
            let za = raw * Complex::from_polar(1.0, -self.nco_phase as f32);
            self.acq_buf.push(za);
            if self.acq_buf.len() >= ACQ_FFT {
                if self.locked() {
                    self.acq_blocks = 0;
                } else {
                    let mut sq: Vec<Complex<f32>> =
                        self.acq_buf.iter().map(|z| z * z).collect();
                    self.acq_fft.process(&mut sq);
                    for (s, v) in self.acq_spec.iter_mut().zip(&sq) {
                        *s = 0.5 * *s + 0.5 * v.norm_sqr();
                    }
                    self.acq_blocks += 1;
                    if self.acq_blocks >= 2 {
                        let res = self.fs / ACQ_FFT as f64;
                        let tone = (SYMBOL_RATE / res).round() as i64;
                        let range = (3_000.0 / res) as i64;
                        let idx = |k: i64| k.rem_euclid(ACQ_FFT as i64) as usize;
                        let mut best = (f32::MIN, 0i64);
                        for k in -range..=range {
                            let mut s = 0.0f32;
                            for j in -1..=1 {
                                s += self.acq_spec[idx(k - tone + j)]
                                    + self.acq_spec[idx(k + tone + j)];
                            }
                            if s > best.0 {
                                best = (s, k);
                            }
                        }
                        let df = best.1 as f64 * res / 2.0;
                        if df.abs() > 1.0 {
                            self.nco_freq_hz += df;
                            self.ct_filter.reset();
                            self.bias.reset();
                            self.phase_trim = 0.0;
                            self.acq_spec.iter_mut().for_each(|s| *s = 0.0);
                            self.acq_blocks = 0;
                        }
                    }
                }
                self.acq_buf.clear();
            }

            // AGC to ~unit magnitude, clip (JAERO 2.84).
            self.agc += 0.001 * (y0.norm() - self.agc);
            let mut y = y0 / self.agc.max(1e-9);
            let mag = y.norm();
            if mag > 2.84 {
                y *= 2.84 / mag;
            }

            // Square-law timing detector → resonator → timing oscillator.
            let p = y.norm_sqr();
            let diff = self.d_pow.run(p) - p;
            let d1 = self.d_t41.run(diff);
            let d2 = self.d_t42.run(d1);
            let eta = self.resonator.run((d2 - diff) * d1);
            let m1 = Complex::new(eta, -self.d_t8.run(eta));
            let st_rot = Complex::from_polar(
                1.0,
                (std::f64::consts::TAU * self.st_phase) as f32,
            );
            let st_err = (st_rot * m1).arg();
            self.st_freq_hz = (self.st_freq_hz - st_err as f64 * 1e-8)
                .clamp(BIT_RATE as f64 - 0.1, BIT_RATE as f64 + 0.1);
            let prev_phase = self.st_phase;
            self.st_phase += self.st_freq_hz / self.fs - st_err as f64 * 0.01 / 360.0;
            // Strobe when the oscillator passes STROBE_POINT (with wrap).
            let passed = {
                let a = prev_phase.rem_euclid(1.0);
                let b = self.st_phase.rem_euclid(1.0);
                if a <= b {
                    a < self.strobe_point && self.strobe_point <= b
                } else {
                    a < self.strobe_point || self.strobe_point <= b
                }
            };
            self.st_phase = self.st_phase.rem_euclid(1.0);
            if passed {
                // Interpolate back to the strobe instant.
                let step = self.st_freq_hz / self.fs;
                let frac = (((self.st_phase - self.strobe_point).rem_euclid(1.0)) / step)
                    .clamp(0.0, 1.0) as f32;
                let pt = y * (1.0 - frac) + self.sig_last * frac;
                self.pair_toggle = !self.pair_toggle;
                if self.pair_toggle {
                    self.pt_d = pt;
                } else {
                    // De-offset pair: current strobe's I with the previous
                    // strobe's Q.
                    let mut pt_qpsk = Complex::new(pt.re, self.pt_d.im);

                    // tanh cross-product carrier discriminator.
                    let ct = (self.pt_d.re.tanh() * self.pt_d.im
                        - pt.im.tanh() * pt.re)
                        .clamp(-std::f32::consts::PI, std::f32::consts::PI);
                    let ec = self
                        .ct_filter
                        .run(ct)
                        .clamp(-std::f32::consts::FRAC_PI_2, std::f32::consts::FRAC_PI_2);
                    // Averaged over the data, the discriminator slope is
                    // negative w.r.t. constellation rotation (the off-rail
                    // component at each strobe is transitional, not a
                    // symbol), so corrections apply with these signs.
                    self.phase_trim += ec.to_radians();
                    self.nco_freq_hz -= 0.01 * ec as f64;

                    // Slow bias removal + lock metric.
                    let b = self.bias.run(ec);
                    pt_qpsk *= Complex::from_polar(1.0, b);
                    let ideal = Complex::new(
                        std::f32::consts::FRAC_1_SQRT_2.copysign(pt_qpsk.re),
                        std::f32::consts::FRAC_1_SQRT_2.copysign(pt_qpsk.im),
                    );
                    self.mse += 0.0025 * ((pt_qpsk - ideal).norm_sqr() - self.mse);
                    let u = pt_qpsk / pt_qpsk.norm().max(1e-9);
                    let u4 = (u * u) * (u * u);
                    self.quad += 0.0025 * (u4 - self.quad);

                    out.push((pt_qpsk.im, (pt_qpsk.im > 0.0) as u8));
                    out.push((pt_qpsk.re, (pt_qpsk.re > 0.0) as u8));
                }
            }
            self.sig_last = y;
        }
    }

    pub fn level_dbfs(&self) -> f32 {
        10.0 * self.level.max(1e-12).log10()
    }

    /// Carrier-lock quality (mean squared constellation error; < 0.5 ≈ locked).
    pub fn mse(&self) -> f32 {
        self.mse
    }

    /// True carrier lock: low constellation error *and* a stationary
    /// (non-spinning) constellation per the 4th-power statistic.
    pub fn locked(&self) -> bool {
        self.mse < LOCK_MSE && self.quad.norm() > 0.4
    }
}

/// Dual-rail UW hunt + frame assembly for the 10.5k P channel.
pub struct HrFramer {
    decoder: FrameDecoder,
    /// Rolling 64-bit window of hard bits.
    shift: u64,
    /// When collecting: (soft bits after UW, rail inversion masks).
    collecting: Option<(Vec<f32>, [f32; 2])>,
    pub reasm: su::Reassembler,
}

/// Check a 64-bit window: even-position bits = one rail's 32-bit UW,
/// odd = the other's, each rail independently invertible. Returns the
/// per-rail inversion signs on match.
fn check_uw(window: u64) -> Option<[f32; 2]> {
    let mut even: u32 = 0;
    let mut odd: u32 = 0;
    for k in 0..32 {
        even |= (((window >> (63 - 2 * k)) & 1) as u32) << (31 - k);
        odd |= (((window >> (63 - (2 * k + 1))) & 1) as u32) << (31 - k);
    }
    let sign = |r: u32| -> Option<f32> {
        if (r ^ UW32).count_ones() <= 2 {
            Some(1.0)
        } else if (r ^ !UW32).count_ones() <= 2 {
            Some(-1.0)
        } else {
            None
        }
    };
    Some([sign(even)?, sign(odd)?])
}

impl HrFramer {
    pub fn new() -> Self {
        Self {
            decoder: FrameDecoder::new(BIT_RATE),
            shift: 0,
            collecting: None,
            reasm: su::Reassembler::new(),
        }
    }

    pub fn push(&mut self, soft: f32, hard: u8, out: &mut Vec<su::AeroUserData>) {
        if let Some((buf, inv)) = &mut self.collecting {
            let k = buf.len();
            buf.push(soft * inv[k % 2]);
            if buf.len() == HR_SKIP_BITS + HR_CODED_BITS {
                let coded = &buf[HR_SKIP_BITS..];
                let bytes = self.decoder.decode(coded);
                for su_bytes in bytes.chunks_exact(su::SU_LEN) {
                    if su::su_crc_ok(su_bytes) {
                        if let Some(u) = self.reasm.push(su_bytes) {
                            out.push(u);
                        }
                    }
                }
                self.collecting = None;
            }
        }
        self.shift = (self.shift << 1) | hard as u64;
        if self.collecting.is_none() {
            if let Some(inv) = check_uw(self.shift) {
                self.collecting =
                    Some((Vec::with_capacity(HR_SKIP_BITS + HR_CODED_BITS), inv));
            }
        }
    }
}

impl Default for HrFramer {
    fn default() -> Self {
        Self::new()
    }
}

/// Build the 10.5k frame bit stream (testing): dual-rail UW + header +
/// dummy + interleaved coded bits from a generalized FrameEncoder.
pub fn hr_frame_bits(enc: &mut frame::FrameEncoder, su_bytes: &[u8], counter: u8) -> Vec<u8> {
    let low = enc.encode(su_bytes, counter); // UW32 + header16 + coded
    let coded = &low[48..];
    let mut bits = Vec::with_capacity(64 + HR_SKIP_BITS + HR_CODED_BITS);
    // Dual-rail UW: interleave the same 32 bits onto both rails.
    for k in 0..32 {
        let b = ((UW32 >> (31 - k)) & 1) as u8;
        bits.push(b);
        bits.push(b);
    }
    // Header (16) + dummy (178): reuse the low-rate header bits, pad.
    bits.extend_from_slice(&low[32..48]);
    bits.extend(std::iter::repeat(0).take(HR_SKIP_BITS - 16));
    bits.extend_from_slice(coded);
    bits
}

/// OQPSK modulator (testing): bits alternate rails; each rail is an
/// impulse train at 5250 baud, the Q rail offset half a symbol, shaped
/// with RRC(β=1) — the receiver's matched filter.
pub fn modulate_oqpsk(
    bits: &[u8],
    sample_rate: f64,
    freq_offset_hz: f64,
    amplitude: f32,
) -> Vec<Complex<f32>> {
    let half = sample_rate / SYMBOL_RATE / 2.0;
    let total = ((bits.len() + 4) as f64 * half) as usize;
    let mut i_rail = vec![0.0f32; total];
    let mut q_rail = vec![0.0f32; total];
    for (k, &b) in bits.iter().enumerate() {
        let v = if b == 1 { 1.0 } else { -1.0 };
        // Bit k's symbol impulse sits at its strobe instant.
        let at = ((k + 1) as f64 * half) as usize;
        if at < total {
            if k % 2 == 0 {
                i_rail[at] = v;
            } else {
                q_rail[at] = v;
            }
        }
    }
    let taps = rrc_taps(sample_rate / SYMBOL_RATE, 129, 1.0);
    let mut shape_i = Fir::new(taps.clone());
    let mut shape_q = Fir::new(taps);
    let ic: Vec<Complex<f32>> = i_rail.into_iter().map(|v| Complex::new(v, 0.0)).collect();
    let qc: Vec<Complex<f32>> = q_rail.into_iter().map(|v| Complex::new(v, 0.0)).collect();
    let mut i_s = Vec::new();
    let mut q_s = Vec::new();
    shape_i.process(&ic, &mut i_s);
    shape_q.process(&qc, &mut q_s);

    (0..total)
        .map(|n| {
            let ph = std::f64::consts::TAU * freq_offset_hz * n as f64 / sample_rate;
            let bb = Complex::new(i_s[n].re, q_s[n].re);
            bb * Complex::from_polar(amplitude, ph as f32)
        })
        .collect()
}

#[cfg(test)]
mod demod_tests {
    use super::*;

    struct Lcg(u64);
    impl Lcg {
        fn bit(&mut self) -> u8 {
            self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            ((self.0 >> 33) & 1) as u8
        }
    }

    /// Per-rail BER over the back half of the stream, trying bit-stream
    /// lags with independent per-rail polarity — what HrFramer tolerates.
    fn rail_ber(tx: &[u8], rx: &[f32]) -> f64 {
        let mut best = 1.0f64;
        for lag in 0..256.min(rx.len()) {
            let n = tx.len().min(rx.len() - lag);
            if n < 2000 {
                break;
            }
            let mut errs = 0usize;
            let mut total = 0usize;
            for rail in 0..2 {
                let pairs: Vec<(f64, bool)> = (n / 2..n)
                    .filter(|k| k % 2 == rail)
                    .map(|k| (rx[lag + k] as f64, tx[k] == 1))
                    .collect();
                let corr: f64 = pairs.iter().map(|&(s, b)| if b { s } else { -s }).sum();
                let sign = corr.signum();
                errs += pairs.iter().filter(|&&(s, b)| ((s * sign) > 0.0) != b).count();
                total += pairs.len();
            }
            best = best.min(errs as f64 / total as f64);
        }
        best
    }

    #[test]
    fn locks_and_demodulates_with_cfo() {
        for &cfo in &[0.0_f64, 120.0, -250.0] {
            let mut rng = Lcg(42);
            let bits: Vec<u8> = (0..40_000).map(|_| rng.bit()).collect();
            let iq = modulate_oqpsk(&bits, CHANNEL_RATE_HR, cfo, 0.5);
            let mut demod = OqpskDemod::new(CHANNEL_RATE_HR);
            let mut out = Vec::new();
            demod.process(&iq, &mut out);
            assert!(demod.locked(), "cfo={cfo}: no carrier lock (mse {})", demod.mse());
            let rx: Vec<f32> = out.iter().map(|&(s, _)| s).collect();
            let ber = rail_ber(&bits, &rx);
            assert!(ber < 0.001, "cfo={cfo}: BER {ber}");
        }
    }
}
