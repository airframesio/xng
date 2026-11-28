//! Shared low-rate signal paths: the 48 kHz coherent in-phase signal is
//! mixed and decimated to [`TONE_RATE`] for tone and subcarrier analysis, and
//! the ident tone is narrowed further for the Morse decoder.

use crate::morse::{IdentDecoder, IdentRule, ENV_RATE};
use crate::CHANNEL_RATE;
use num_complex::Complex;
use rustfft::FftPlanner;
use std::f64::consts::TAU;
use xng_dsp::{lowpass_taps, Fir};

/// Analysis rate (Hz): 80 samples per 30 Hz cycle, so every navaid tone
/// (30, 90, 150 Hz) has an integer number of cycles in any whole block.
pub const TONE_RATE: f64 = 2_400.0;
pub const DECIM: usize = (CHANNEL_RATE / TONE_RATE) as usize;
/// One-sided passband of the decimated paths (Hz): the 9960 Hz subcarrier's
/// FM sidebands reach ±510 Hz; 90/150 Hz tones and the ident sit inside.
const PATH_CUTOFF_HZ: f64 = 750.0;
const PATH_TAPS: usize = 301;

/// The taps every 48 kHz → 2400 Hz path uses. Sharing one design keeps the
/// group delay identical across paths, which the VOR phase comparison relies
/// on.
pub fn path_taps() -> Vec<f32> {
    lowpass_taps(PATH_CUTOFF_HZ / CHANNEL_RATE, PATH_TAPS)
}

/// Mix a real 48 kHz stream by `-freq_hz` and decimate to [`TONE_RATE`].
pub struct MixDecim {
    fir: Fir,
    phase: f64,
    step: f64,
    scratch: Vec<Complex<f32>>,
}

impl MixDecim {
    pub fn new(freq_hz: f64) -> Self {
        Self {
            fir: Fir::with_decimation(path_taps(), DECIM),
            phase: 0.0,
            step: TAU * freq_hz / CHANNEL_RATE,
            scratch: Vec::new(),
        }
    }

    pub fn process(&mut self, x: &[f32], out: &mut Vec<Complex<f32>>) {
        self.scratch.clear();
        if self.step == 0.0 {
            self.scratch.extend(x.iter().map(|&v| Complex::new(v, 0.0)));
        } else {
            let (ss, sc) = (-self.step).sin_cos();
            let rot = Complex::new(sc, ss);
            let mut r = Complex::new(self.phase.cos(), -self.phase.sin());
            for (i, &v) in x.iter().enumerate() {
                if i % 256 == 0 {
                    r = Complex::new(self.phase.cos(), -self.phase.sin());
                }
                self.scratch.push(Complex::new(
                    (v as f64 * r.re) as f32,
                    (v as f64 * r.im) as f32,
                ));
                r *= rot;
                self.phase = (self.phase + self.step) % TAU;
            }
        }
        self.fir.process(&self.scratch, out);
    }
}

/// The ident tone: located to within a few Hz, narrowed to ±25 Hz, envelope
/// at [`ENV_RATE`] into the Morse decoder.
pub struct IdentPath {
    mix: MixDecim,
    /// Search buffer at TONE_RATE for the tone's exact frequency.
    search: Vec<Complex<f32>>,
    /// Accumulated power spectrum of the search buffer.
    spec: Vec<f64>,
    spec_count: usize,
    tone_off_hz: Option<f64>,
    fine_phase: f64,
    fir: Fir,
    /// Second, keying-matched filter at ENV_RATE (±5 Hz around the tone).
    narrow: Fir,
    buf: Vec<Complex<f32>>,
    env: Vec<Complex<f32>>,
    env2: Vec<Complex<f32>>,
    pow: Vec<f32>,
    pub decoder: IdentDecoder,
    /// Recent mark-level tone amplitude relative to the carrier (depth).
    pub depth: f32,
}

const SEARCH_N: usize = 4096; // 1.7 s at 2400 Hz, 0.59 Hz bins
const SEARCH_SPAN_HZ: f64 = 75.0;

impl IdentPath {
    pub fn new(tone_hz: f64, rule: IdentRule) -> Self {
        Self {
            mix: MixDecim::new(tone_hz),
            search: Vec::new(),
            spec: vec![0.0; SEARCH_N],
            spec_count: 0,
            tone_off_hz: None,
            fine_phase: 0.0,
            fir: Fir::with_decimation(
                lowpass_taps(25.0 / TONE_RATE, 241),
                (TONE_RATE / ENV_RATE) as usize,
            ),
            // A 7 wpm dot is 171 ms: about ±5 Hz keeps the keying edges and
            // drops the noise bandwidth to a tenth of a plain ±50 Hz filter.
            narrow: Fir::new(lowpass_taps(5.0 / ENV_RATE, 121)),
            buf: Vec::new(),
            env: Vec::new(),
            env2: Vec::new(),
            pow: Vec::new(),
            decoder: IdentDecoder::with_rule(rule),
            depth: 0.0,
        }
    }

    /// Measured ident tone frequency offset from nominal (Hz), once found.
    pub fn tone_offset_hz(&self) -> Option<f64> {
        self.tone_off_hz
    }

    /// `x`: coherent in-phase samples; `amp`: carrier amplitude (for depth).
    pub fn process(&mut self, x: &[f32], amp: f64) {
        self.buf.clear();
        self.mix.process(x, &mut self.buf);
        // Keep refining the tone frequency from accumulated spectra.
        self.search.extend_from_slice(&self.buf);
        while self.search.len() >= SEARCH_N {
            let mut b: Vec<Complex<f64>> = self.search[..SEARCH_N]
                .iter()
                .enumerate()
                .map(|(i, c)| {
                    let w = 0.5 - 0.5 * (TAU * i as f64 / SEARCH_N as f64).cos();
                    Complex::new(c.re as f64 * w, c.im as f64 * w)
                })
                .collect();
            FftPlanner::new().plan_fft_forward(SEARCH_N).process(&mut b);
            // Exponential average so the estimate follows a slow drift.
            let a = if self.spec_count < 8 {
                1.0 / (self.spec_count + 1) as f64
            } else {
                0.125
            };
            for (s, c) in self.spec.iter_mut().zip(&b) {
                *s += a * (c.norm_sqr() - *s);
            }
            self.spec_count += 1;
            self.search.drain(..SEARCH_N / 2);
            if self.spec_count >= 3 {
                self.update_tone();
            }
        }
        let Some(off) = self.tone_off_hz else { return };
        // Fine mix to the measured tone, narrow, envelope.
        let step = TAU * off / TONE_RATE;
        for c in self.buf.iter_mut() {
            let (s, co) = self.fine_phase.sin_cos();
            *c *= Complex::new(co as f32, -s as f32);
            self.fine_phase = (self.fine_phase + step) % TAU;
        }
        self.env.clear();
        self.fir.process(&self.buf, &mut self.env);
        self.pow.clear();
        let norm = if amp > 0.0 {
            1.0 / (amp * amp) as f32
        } else {
            1.0
        };
        self.env2.clear();
        self.narrow.process(&self.env, &mut self.env2);
        self.pow
            .extend(self.env2.iter().map(|c| c.norm_sqr() * norm));
        self.decoder.push(&self.pow);
        // Keyed tone level (normalized to the carrier) → modulation depth.
        if let Some(db) = self.decoder.tone_level_db() {
            self.depth = 2.0 * 10f32.powf(db / 20.0);
        }
    }

    fn update_tone(&mut self) {
        let bin = TONE_RATE / SEARCH_N as f64;
        let span = (SEARCH_SPAN_HZ / bin) as isize;
        let idx = |k: isize| ((k + SEARCH_N as isize) % SEARCH_N as isize) as usize;
        let mut best = (0isize, 0.0f64);
        for k in -span..=span {
            let p = self.spec[idx(k)];
            if p > best.1 {
                best = (k, p);
            }
        }
        // Noise from the bins outside the search window.
        let mut floor: Vec<f64> = (span + 20..span + 220)
            .map(|k| self.spec[idx(k)])
            .chain((span + 20..span + 220).map(|k| self.spec[idx(-k)]))
            .collect();
        floor.sort_by(|a, b| a.total_cmp(b));
        let floor = floor[floor.len() / 2].max(1e-300);
        if 10.0 * (best.1 / floor).log10() < 6.0 {
            return; // no ident tone yet
        }
        let (l, c, r) = (
            self.spec[idx(best.0 - 1)],
            best.1,
            self.spec[idx(best.0 + 1)],
        );
        let den = l - 2.0 * c + r;
        let d = if den.abs() > 0.0 {
            0.5 * (l - r) / den
        } else {
            0.0
        };
        self.tone_off_hz = Some((best.0 as f64 + d.clamp(-0.5, 0.5)) * bin);
    }
}
