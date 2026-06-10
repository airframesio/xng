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
use xng_dsp::{lowpass_taps, Fir};

pub const BIT_RATE: u32 = 10_500;
pub const SYMBOL_RATE: f64 = 5_250.0;
pub const CHANNEL_RATE_HR: f64 = 48_000.0;
const UW32: u32 = frame::UW;
/// Header + dummy section between the UW and the coded block.
pub const HR_SKIP_BITS: usize = 16 + 178;
pub const HR_CODED_BITS: usize = 64 * 78;

const PHASE_GAIN: f32 = 0.04;
const FREQ_GAIN: f32 = 0.0015;
const TIMING_GAIN: f64 = 0.02;
const AGC_ALPHA: f32 = 0.01;

/// Coherent OQPSK demod: NCO + lowpass, half-symbol strobes alternating
/// rails (I at even strobes, Q at odd), decision-directed cross-product
/// carrier loop, carrier-gated Gardner timing.
pub struct OqpskDemod {
    half: f64, // samples per half-symbol (rail-alternating strobe period)
    lpf: Fir,
    filtered: Vec<Complex<f32>>,
    nco_phase: f32,
    nco_freq: f32,
    timing: f64,
    history: [Complex<f32>; 32],
    hist_pos: usize,
    sample_idx: u64,
    rail_i: bool,
    prev_i: f32,
    prev_q: f32,
    agc: f32,
    carr_err: f32,
    level: f32,
}

impl OqpskDemod {
    pub fn new(channel_rate: f64) -> Self {
        Self {
            half: channel_rate / SYMBOL_RATE / 2.0,
            lpf: Fir::new(lowpass_taps(6_000.0 / channel_rate, 97)),
            filtered: Vec::new(),
            nco_phase: 0.0,
            nco_freq: 0.0,
            timing: 0.0,
            history: [Complex::new(0.0, 0.0); 32],
            hist_pos: 0,
            sample_idx: 0,
            rail_i: true,
            prev_i: 0.0,
            prev_q: 0.0,
            agc: 1e-3,
            carr_err: 1.0,
            level: 0.0,
        }
    }

    fn past(&self, delay: f64) -> Complex<f32> {
        let n = self.history.len();
        let i = delay.floor() as usize;
        let frac = (delay - i as f64) as f32;
        let a = self.history[(self.hist_pos + n - 1 - i) % n];
        let b = self.history[(self.hist_pos + n - 2 - i) % n];
        a * (1.0 - frac) + b * frac
    }

    /// Feed channel IQ; append (soft, hard) bits at 10 500 bps,
    /// alternating rails.
    pub fn process(&mut self, input: &[Complex<f32>], out: &mut Vec<(f32, u8)>) {
        for &raw in input {
            self.level += 0.001 * (raw.norm_sqr() - self.level);
            let mixed = raw * Complex::from_polar(1.0, self.nco_phase);
            self.nco_phase += self.nco_freq;
            if self.nco_phase.abs() > std::f32::consts::TAU {
                self.nco_phase %= std::f32::consts::TAU;
            }
            self.filtered.clear();
            self.lpf.process(&[mixed], &mut self.filtered);
            let Some(&y) = self.filtered.first() else { continue };
            self.history[self.hist_pos] = y;
            self.hist_pos = (self.hist_pos + 1) % self.history.len();
            self.sample_idx += 1;
            if self.sample_idx < self.history.len() as u64 {
                continue;
            }

            self.timing += 1.0;
            if self.timing < self.half {
                continue;
            }
            self.timing -= self.half;
            let now = self.past(self.timing);
            let mid = self.past(self.timing + self.half / 2.0);

            let (val, prev, other) = if self.rail_i {
                (now.re, self.prev_i, now.im)
            } else {
                (now.im, self.prev_q, now.re)
            };
            self.agc += AGC_ALPHA * (val.abs() - self.agc);
            let sym = (val / self.agc.max(1e-9)).clamp(-2.0, 2.0);

            // OQPSK decision-directed carrier error: at each rail strobe,
            // the *other* component should be mid-transition (small after
            // lock); its correlation with the decision gives the phase
            // error sign.
            let perr = (other / self.agc.max(1e-9)).clamp(-2.0, 2.0) * sym.signum() * 0.5;
            self.nco_phase -= PHASE_GAIN * perr;
            self.nco_freq -= FREQ_GAIN * perr / (2.0 * self.half) as f32;
            self.carr_err += 0.02 * (perr.abs() - self.carr_err);

            if self.carr_err < 0.4 {
                let m = if self.rail_i { mid.re } else { mid.im };
                let terr = ((sym - prev) * (m / self.agc.max(1e-9))) as f64;
                self.timing += (TIMING_GAIN * terr).clamp(-0.08, 0.08);
            }
            if self.rail_i {
                self.prev_i = sym;
            } else {
                self.prev_q = sym;
            }
            self.rail_i = !self.rail_i;
            out.push((sym, (sym > 0.0) as u8));
        }
    }

    pub fn level_dbfs(&self) -> f32 {
        10.0 * self.level.max(1e-12).log10()
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

/// OQPSK modulator (testing): bits alternate rails; each rail NRZ at
/// 5250 baud with the Q rail offset half a symbol; Nyquist-shaped.
pub fn modulate_oqpsk(
    bits: &[u8],
    sample_rate: f64,
    freq_offset_hz: f64,
    amplitude: f32,
) -> Vec<Complex<f32>> {
    let half = sample_rate / SYMBOL_RATE / 2.0;
    let total = ((bits.len() + 2) as f64 * half) as usize;
    // Build rail waveforms at sample resolution (rect), then shape.
    let mut i_rail = vec![0.0f32; total];
    let mut q_rail = vec![0.0f32; total];
    for (k, &b) in bits.iter().enumerate() {
        let v = if b == 1 { 1.0 } else { -1.0 };
        // Bit k occupies one half-symbol strobe slot; its rail holds the
        // value for a full symbol (two slots) centered on its strobe.
        let start = (k as f64 * half) as usize;
        let end = (((k + 2) as f64) * half) as usize;
        let rail = if k % 2 == 0 { &mut i_rail } else { &mut q_rail };
        for s in rail[start..end.min(total)].iter_mut() {
            *s = v;
        }
    }
    let mut shape_i = Fir::new(lowpass_taps(0.5 * SYMBOL_RATE / sample_rate, 129));
    let mut shape_q = Fir::new(lowpass_taps(0.5 * SYMBOL_RATE / sample_rate, 129));
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
