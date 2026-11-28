//! Navaid signal synthesis for tests and benchmarks.
//!
//! Builds VOR (conventional or Doppler), localizer and glideslope signals
//! from the Annex 10 definitions, with Morse ident, a voice-band stand-in,
//! carrier and subcarrier frequency errors, two-frequency systems and AWGN at
//! a stated carrier-to-noise density. Output is complex baseband at any
//! sample rate, streamed in blocks so long captures need no memory.
//!
//! VERIFICATION NOTE: a self-generated signal only shows that the receiver
//! inverts this model of the standard. It is also what the benchmarks feed
//! every decoder, so they are compared on identical input.

use crate::morse::code_for;
use num_complex::Complex;
use std::f64::consts::TAU;

/// Morse keying envelope for a repeated ident.
#[derive(Debug, Clone)]
pub struct IdentKey {
    marks: Vec<(f64, f64)>,
    period_s: f64,
    start_s: f64,
}

impl IdentKey {
    /// `wpm` sets the dot (1.2/wpm s); the ident repeats every `period_s`,
    /// first starting at `start_s`.
    pub fn new(ident: &str, wpm: f64, period_s: f64, start_s: f64) -> Self {
        let dot = 1.2 / wpm;
        let mut t = 0.0;
        let mut marks = Vec::new();
        for (i, c) in ident.chars().enumerate() {
            if i > 0 {
                t += 2.0 * dot; // 3 dots between letters (1 already added)
            }
            if let Some(code) = code_for(c) {
                for s in code.chars() {
                    let len = if s == '.' { dot } else { 3.0 * dot };
                    marks.push((t, t + len));
                    t += len + dot;
                }
            }
        }
        Self {
            marks,
            period_s,
            start_s,
        }
    }

    /// Keying level in [0, 1] with 5 ms raised-cosine edges.
    pub fn level(&self, t: f64) -> f64 {
        if t < self.start_s {
            return 0.0;
        }
        let u = (t - self.start_s) % self.period_s;
        const EDGE: f64 = 0.005;
        for &(a, b) in &self.marks {
            if u >= a && u < b {
                let r = ((u - a) / EDGE).min((b - u) / EDGE).min(1.0);
                return 0.5 - 0.5 * (std::f64::consts::PI * r).cos();
            }
            if u < a {
                break;
            }
        }
        0.0
    }
}

#[derive(Debug, Clone)]
pub struct IdentSpec {
    pub text: String,
    pub depth: f64,
    pub tone_hz: f64,
    pub wpm: f64,
    pub period_s: f64,
    pub start_s: f64,
}

impl IdentSpec {
    pub fn standard(text: &str) -> Self {
        Self {
            text: text.into(),
            depth: 0.10,
            tone_hz: 1_020.0,
            wpm: 7.0,
            period_s: 10.0,
            start_s: 0.5,
        }
    }
}

#[derive(Debug, Clone)]
pub struct VorParams {
    pub radial_deg: f64,
    pub mod30: f64,
    pub mod_sc: f64,
    pub deviation_hz: f64,
    pub subcarrier_hz: f64,
    pub f30_hz: f64,
    /// Doppler VOR sideband asymmetry (0 = conventional VOR).
    pub dvor_asymmetry: f64,
    pub voice_depth: f64,
    pub ident: Option<IdentSpec>,
}

impl Default for VorParams {
    fn default() -> Self {
        Self {
            radial_deg: 0.0,
            mod30: 0.30,
            mod_sc: 0.30,
            deviation_hz: 480.0,
            subcarrier_hz: 9_960.0,
            f30_hz: 30.0,
            dvor_asymmetry: 0.0,
            voice_depth: 0.0,
            ident: None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct IlsParams {
    /// m90 − m150.
    pub ddm: f64,
    /// m90 + m150 (0.40 localizer, 0.80 glideslope).
    pub sdm: f64,
    /// Extra phase on the 150 Hz tone (°), 0 = phase-locked.
    pub phase150_deg: f64,
    pub ident: Option<IdentSpec>,
}

impl IlsParams {
    pub fn localizer(ddm: f64) -> Self {
        Self {
            ddm,
            sdm: 0.40,
            phase150_deg: 0.0,
            ident: None,
        }
    }
    pub fn glideslope(ddm: f64) -> Self {
        Self {
            ddm,
            sdm: 0.80,
            phase150_deg: 0.0,
            ident: None,
        }
    }
}

#[derive(Debug, Clone)]
pub enum Signal {
    Vor(VorParams),
    Ils(IlsParams),
}

#[derive(Debug, Clone)]
pub struct CarrierSpec {
    pub offset_hz: f64,
    pub amplitude: f64,
    pub phase_rad: f64,
    pub signal: Signal,
}

/// A scene: one or more carriers plus noise.
#[derive(Debug, Clone)]
pub struct Scene {
    pub sample_rate: f64,
    pub carriers: Vec<CarrierSpec>,
    /// C/N0 of the first carrier (dB-Hz); `None` for no noise.
    pub cn0_dbhz: Option<f64>,
    pub seed: u64,
}

impl Scene {
    pub fn single(sample_rate: f64, offset_hz: f64, signal: Signal, cn0_dbhz: Option<f64>) -> Self {
        Self {
            sample_rate,
            carriers: vec![CarrierSpec {
                offset_hz,
                amplitude: 0.25,
                phase_rad: 0.7,
                signal,
            }],
            cn0_dbhz,
            seed: 1,
        }
    }
}

struct Rng(u64);

impl Rng {
    fn next_f64(&mut self) -> f64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        ((self.0 >> 11) as f64 + 0.5) / (1u64 << 53) as f64
    }
    fn gauss2(&mut self) -> (f64, f64) {
        let r = (-2.0 * self.next_f64().ln()).sqrt();
        let a = TAU * self.next_f64();
        (r * a.cos(), r * a.sin())
    }
}

/// Streaming signal generator.
pub struct Generator {
    scene: Scene,
    keys: Vec<Option<IdentKey>>,
    n: u64,
    sigma: f64,
    rng: Rng,
    /// Running FM phase of each VOR subcarrier.
    sc_phase: Vec<f64>,
}

const VOICE: [(f64, f64); 6] = [
    (420.0, 0.31),
    (730.0, 0.22),
    (1_150.0, 0.18),
    (1_600.0, 0.12),
    (2_150.0, 0.1),
    (2_700.0, 0.07),
];

impl Generator {
    pub fn new(scene: Scene) -> Self {
        let keys = scene
            .carriers
            .iter()
            .map(|c| {
                let id = match &c.signal {
                    Signal::Vor(v) => v.ident.as_ref(),
                    Signal::Ils(p) => p.ident.as_ref(),
                };
                id.map(|i| IdentKey::new(&i.text, i.wpm, i.period_s, i.start_s))
            })
            .collect();
        let sigma = match (scene.cn0_dbhz, scene.carriers.first()) {
            (Some(cn0), Some(c)) => {
                let n0 = c.amplitude * c.amplitude / 10f64.powf(cn0 / 10.0);
                (n0 * scene.sample_rate / 2.0).sqrt() // per real component
            }
            _ => 0.0,
        };
        let seed = scene.seed.max(1).wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
        let n = scene.carriers.len();
        Self {
            scene,
            keys,
            n: 0,
            sigma,
            rng: Rng(seed),
            sc_phase: vec![0.0; n],
        }
    }

    /// The next `count` samples.
    pub fn next_block(&mut self, count: usize) -> Vec<Complex<f32>> {
        let fs = self.scene.sample_rate;
        let mut out = Vec::with_capacity(count);
        for _ in 0..count {
            let t = self.n as f64 / fs;
            let mut acc = Complex::new(0.0, 0.0);
            for (ci, c) in self.scene.carriers.iter().enumerate() {
                let (i_mod, q_mod) = match &c.signal {
                    Signal::Vor(v) => {
                        let w30 = TAU * v.f30_hz * t;
                        let mut m = v.mod30 * (w30 - v.radial_deg.to_radians()).cos();
                        // Subcarrier phase: integrate frequency so a
                        // subcarrier offset is a true frequency error.
                        let inst = v.subcarrier_hz + v.deviation_hz * w30.cos();
                        self.sc_phase[ci] = (self.sc_phase[ci] + TAU * inst / fs) % TAU;
                        let psi = self.sc_phase[ci];
                        m += v.mod_sc * psi.cos();
                        let q = v.dvor_asymmetry * v.mod_sc * psi.sin();
                        if v.voice_depth > 0.0 {
                            let mut vv = 0.0;
                            for (k, (f, a)) in VOICE.iter().enumerate() {
                                vv += a
                                    * (TAU * f * t + k as f64 * 1.3 + (0.7 * t + k as f64).sin())
                                        .sin();
                            }
                            m += v.voice_depth * vv;
                        }
                        if let (Some(id), Some(k)) = (&v.ident, &self.keys[ci]) {
                            m += id.depth * k.level(t) * (TAU * id.tone_hz * t).cos();
                        }
                        (m, q)
                    }
                    Signal::Ils(p) => {
                        let w30 = TAU * 30.0 * t;
                        let m90 = 0.5 * (p.sdm + p.ddm);
                        let m150 = 0.5 * (p.sdm - p.ddm);
                        let mut m = m90 * (3.0 * w30).cos()
                            + m150 * (5.0 * w30 + p.phase150_deg.to_radians()).cos();
                        if let (Some(id), Some(k)) = (&p.ident, &self.keys[ci]) {
                            m += id.depth * k.level(t) * (TAU * id.tone_hz * t).cos();
                        }
                        (m, 0.0)
                    }
                };
                let ph = TAU * c.offset_hz * t + c.phase_rad;
                acc += Complex::new(1.0 + i_mod, q_mod) * Complex::from_polar(c.amplitude, ph);
            }
            if self.sigma > 0.0 {
                let (a, b) = self.rng.gauss2();
                acc += Complex::new(a * self.sigma, b * self.sigma);
            }
            out.push(Complex::new(acc.re as f32, acc.im as f32));
            self.n += 1;
        }
        out
    }
}

/// Quantize to RTL-SDR style unsigned 8-bit IQ (`scale` maps 1.0 to full scale).
pub fn to_cu8(x: &[Complex<f32>], scale: f32, out: &mut Vec<u8>) {
    for c in x {
        let q = |v: f32| (v * scale * 127.5 + 127.5).round().clamp(0.0, 255.0) as u8;
        out.push(q(c.re));
        out.push(q(c.im));
    }
}
