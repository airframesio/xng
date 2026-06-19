//! Coherent A-BPSK (MSK-class) burst demodulator (AERO-6).
//!
//! The default burst path ([`crate::demod::MskDemod`]) is a frequency
//! discriminator: it forms the per-sample phase difference
//! `d[n] = arg(x[n]·conj(x[n-1]))` and averages those angles over each bit.
//! That is non-coherent — it throws away the carrier phase and pays the
//! well-known FM-detector noise penalty (~2–3 dB; documented in
//! PROVENANCE.md as the discriminator's sensitivity cost).
//!
//! A-BPSK here is CPFSK with modulation index 0.5 (MSK): each bit advances
//! the carrier phase by +90° (bit 1) or −90° (bit 0) over the bit, and the
//! phase is *continuous* (see [`crate::modulate`]). With a coherent phase
//! reference the optimal per-bit decision is a correlation against the two
//! possible phase ramps. `CoherentMskDemod` does exactly that:
//!
//! 1. It keeps a running absolute phase reference `θ` (carrier + the phase
//!    accumulated by all previously-decided bits). Continuous phase means
//!    `θ` at the start of bit k is known once the earlier bits are decided.
//! 2. For each bit it correlates the (matched-filtered) bit samples against
//!    the +90° ramp `exp(j(θ + ½π·t/Tb))` and the −90° ramp
//!    `exp(j(θ − ½π·t/Tb))`, integrating *coherently* over the whole bit,
//!    and picks the larger in-phase energy. Integrating the complex samples
//!    against a phase reference before any nonlinearity gives the detector
//!    the bit's full energy at the true coherent-detection bound.
//! 3. It advances `θ` by the decided ±90° and nudges it with a small
//!    decision-directed phase-error term (a Costas-style carrier loop), so
//!    the reference stays locked through residual CFO and phase drift.
//!
//! Symbol timing is recovered by the same zero-crossing loop the
//! discriminator uses (the per-sample phase advance crosses zero mid-bit at
//! data transitions). The burst gate removes the bulk CFO upstream, so this
//! module only adds the carrier-coherent detector.
//!
//! Verified by `coherent_beats_discriminator_ber_vs_snr` (this crate): a
//! genuine modulate → complex-AWGN → demod BER sweep shows the coherent
//! path reaches a given BER at a markedly lower Eb/N0 than the discriminator
//! path through the identical front end.

use num_complex::Complex;
use xng_dsp::{lowpass_taps, Fir};

const FREQ_ALPHA: f32 = 0.0004;
const TIMING_GAIN: f64 = 0.1;
/// Decision-directed carrier-loop gain (how hard the residual phase error
/// pulls the reference each bit). Small: average over many bits.
const CARRIER_GAIN: f32 = 0.05;
const MAG_ALPHA: f32 = 0.01;

/// Decision-directed coherent (Costas-style) MSK demod for the 600/1200 bps
/// burst path.
pub struct CoherentMskDemod {
    spb: f64,
    /// Rate-matched lowpass / matched filter ahead of the detector (same
    /// role as the discriminator path's LPF).
    lpf: Fir,
    filtered: Vec<Complex<f32>>,
    /// One bit's worth of matched-filtered samples, collected then correlated
    /// against the two phase-ramp hypotheses at the bit boundary.
    bit_samples: Vec<Complex<f32>>,
    /// Running absolute phase reference (carrier + accumulated data phase).
    theta: f32,
    /// Zero-crossing timing recovery state (mirrors the discriminator path).
    freq_offset: f32,
    prev_sample: Complex<f32>,
    prev_disc: f32,
    timing: f64,
    /// Running mean |decision margin| for soft-bit normalization.
    mag: f32,
    have_prev: bool,
}

impl CoherentMskDemod {
    pub fn new(channel_rate: f64, bit_rate: f64) -> Self {
        let cutoff = 0.6 * bit_rate / channel_rate;
        let spb = channel_rate / bit_rate;
        Self {
            spb,
            lpf: Fir::new(lowpass_taps(cutoff, 101)),
            filtered: Vec::new(),
            bit_samples: Vec::with_capacity(spb.ceil() as usize + 2),
            theta: 0.0,
            freq_offset: 0.0,
            prev_sample: Complex::new(0.0, 0.0),
            prev_disc: 0.0,
            timing: 0.0,
            mag: 1e-3,
            have_prev: false,
        }
    }

    /// Decide one bit from its collected samples by correlating against the
    /// +90° and −90° phase ramps anchored at the current reference `θ`, then
    /// advance `θ` by the decision and a decision-directed phase correction.
    fn decide_bit(&mut self) -> (f32, u8) {
        let half = std::f32::consts::FRAC_PI_2;
        let n = self.bit_samples.len().max(1);
        let mut cp = Complex::new(0.0f32, 0.0); // correlation with +ramp
        let mut cm = Complex::new(0.0f32, 0.0); // correlation with −ramp
        for (j, &s) in self.bit_samples.iter().enumerate() {
            let frac = (j as f32 + 0.5) / n as f32;
            cp += s * Complex::from_polar(1.0, self.theta + half * frac).conj();
            cm += s * Complex::from_polar(1.0, self.theta - half * frac).conj();
        }
        // In-phase energy against each hypothesis: the larger wins.
        let bit = (cp.re > cm.re) as u8;
        let dev = if bit == 1 { half } else { -half };
        let matched = if bit == 1 { cp } else { cm };
        // Decision-directed carrier loop: the matched correlator's residual
        // angle is the carrier phase error; pull the reference toward it.
        self.theta += dev + CARRIER_GAIN * matched.arg();
        if self.theta > std::f32::consts::TAU {
            self.theta -= std::f32::consts::TAU;
        } else if self.theta < -std::f32::consts::TAU {
            self.theta += std::f32::consts::TAU;
        }
        // Soft value: normalized decision margin (coherent in-phase energies).
        let margin = cp.re - cm.re;
        self.mag += MAG_ALPHA * (margin.abs() - self.mag);
        let soft = (margin / self.mag.max(1e-9)).clamp(-1.0, 1.0);
        self.bit_samples.clear();
        (soft, bit)
    }

    /// Feed CFO-removed channel IQ; append (soft −1..1, hard 0/1) bits.
    pub fn process(&mut self, input: &[Complex<f32>], out: &mut Vec<(f32, u8)>) {
        let mut filtered = std::mem::take(&mut self.filtered);
        filtered.clear();
        self.lpf.process(input, &mut filtered);
        for &x in &filtered {
            self.bit_samples.push(x);

            // Zero-crossing symbol-timing recovery (same loop as the
            // discriminator): the per-sample phase advance crosses zero at
            // mid-bit data transitions; nudge the bit clock toward them.
            if self.have_prev {
                let raw = (x * self.prev_sample.conj()).arg();
                self.freq_offset += FREQ_ALPHA * (raw - self.freq_offset);
                let disc = raw - self.freq_offset;
                if disc != 0.0
                    && self.prev_disc != 0.0
                    && (disc < 0.0) != (self.prev_disc < 0.0)
                {
                    let err = self.timing - (self.timing / self.spb).round() * self.spb;
                    self.timing -= TIMING_GAIN * err;
                }
                self.prev_disc = disc;
            }
            self.prev_sample = x;
            self.have_prev = true;

            self.timing += 1.0;
            if self.timing >= self.spb {
                self.timing -= self.spb;
                let (soft, hard) = self.decide_bit();
                out.push((soft, hard));
            }
        }
        self.filtered = filtered;
    }
}
