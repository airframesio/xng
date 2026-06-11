//! Coherent AIS burst demodulator: the weak-signal path.
//!
//! The streaming FM-discriminator demod needs ~14 dB SNR; coherent
//! MSK detection works far lower. This path power-gates candidate
//! bursts, anchors them with a complex template correlation over the
//! preamble tail + HDLC start flag (searched over a CFO grid), then
//! runs a 4-state phase-trellis Viterbi (MSK approximation of the
//! GMSK pulse) over the burst with the carrier phase from the
//! correlation. NRZI is decoded inside the trellis transitions; the
//! HDLC deframer and FCS downstream arbitrate as always.

use num_complex::Complex;
use std::collections::VecDeque;
use std::f32::consts::PI;

const SPB: usize = 5; // samples per bit at 48 kHz / 9600 bd
/// Template: last 16 preamble bits (0101…) + the 8-bit start flag.
const TMPL_BITS: usize = 24;
/// Max AIS burst: 256 bits slot − template, plus margin.
const MAX_PAYLOAD_BITS: usize = 280;
/// CFO search grid (Hz): ships ±400 Hz plus receiver ppm.
const CFO_STEP_HZ: f32 = 150.0;
const CFO_RANGE_HZ: f32 = 1_200.0;
/// Burst gate: power must exceed the tracked floor by this factor.
const GATE_FACTOR: f32 = 2.0;
/// Correlation acceptance (normalized 0..1).
const CORR_THRESHOLD: f32 = 0.72;
const NOISE_ALPHA: f32 = 5e-4;

/// Linearly interpolated sample at fractional position `pos`.
#[inline]
fn sample_frac(w: &[Complex<f32>], pos: f32) -> Complex<f32> {
    let i = pos.floor().max(0.0) as usize;
    if i + 1 >= w.len() {
        return *w.last().unwrap_or(&Complex::new(0.0, 0.0));
    }
    let f = pos - i as f32;
    w[i] * (1.0 - f) + w[i + 1] * f
}

/// NRZI level sequence (±1) for a bit pattern, starting from +1.
fn nrzi_levels(bits: &[u8]) -> Vec<f32> {
    let mut level = 1.0f32;
    bits.iter()
        .map(|&b| {
            if b == 0 {
                level = -level;
            }
            level
        })
        .collect()
}

/// MSK baseband for a level sequence: phase ramps ±π/2 per bit.
fn msk_waveform(levels: &[f32]) -> Vec<Complex<f32>> {
    let mut phase = 0.0f32;
    let mut out = Vec::with_capacity(levels.len() * SPB);
    for &l in levels {
        for _ in 0..SPB {
            phase += l * PI / 2.0 / SPB as f32;
            out.push(Complex::from_polar(1.0, phase));
        }
    }
    out
}

pub struct CoherentDemod {
    buf: VecDeque<Complex<f32>>,
    /// Absolute index of buf[0] in the stream.
    base: u64,
    cursor: u64,
    noise: f32,
    template: Vec<Complex<f32>>,
    /// Burst window currently being gathered: (start_abs, samples needed).
    pending: Option<u64>,
    fs: f32,
}

impl CoherentDemod {
    pub fn new(fs: f64) -> Self {
        // 0101… ends with …01; flag = 01111110.
        let mut tmpl_bits = Vec::new();
        for k in 0..16 {
            tmpl_bits.push((k % 2) as u8); // 0,1,0,1,…
        }
        tmpl_bits.extend([0, 1, 1, 1, 1, 1, 1, 0]);
        debug_assert_eq!(tmpl_bits.len(), TMPL_BITS);
        let template = msk_waveform(&nrzi_levels(&tmpl_bits));
        Self {
            buf: VecDeque::new(),
            base: 0,
            cursor: 0,
            noise: 1e-6,
            template,
            pending: None,
            fs: fs as f32,
        }
    }

    /// Feed samples; returns decoded post-flag bit vectors (NRZI already
    /// removed) for the HDLC deframer, paired with the template position
    /// so callers can dedup against the streaming path.
    pub fn process(&mut self, input: &[Complex<f32>]) -> Vec<Vec<u8>> {
        self.buf.extend(input.iter().copied());
        let mut out = Vec::new();

        let tmpl_len = self.template.len();
        let burst_len = tmpl_len + MAX_PAYLOAD_BITS * SPB;

        loop {
            if let Some(start) = self.pending {
                // Wait until the whole candidate burst is buffered.
                let have = self.base + self.buf.len() as u64;
                if have < start + burst_len as u64 {
                    break;
                }
                let s0 = (start - self.base) as usize;
                let window: Vec<Complex<f32>> =
                    self.buf.iter().skip(s0).take(burst_len).copied().collect();
                let decoded = self.try_decode(&window);
                if std::env::var("AIS_DEBUG").is_ok() {
                    eprintln!("try_decode at {}: {}", start, decoded.is_some());
                }
                if let Some(bits) = decoded {
                    out.push(bits);
                }
                self.pending = None;
                self.cursor = start + tmpl_len as u64; // resume past the template
                continue;
            }

            // Hunt: power gate on a one-bit window. The template search
            // below needs its whole window buffered before the cursor may
            // advance — otherwise a burst near the buffer edge is tested
            // truncated, not found, and permanently skipped.
            let rel = (self.cursor - self.base) as usize;
            if rel + SPB * 64 + self.template.len() + SPB >= self.buf.len() {
                break;
            }
            let p: f32 = self.buf.iter().skip(rel).take(SPB).map(|c| c.norm_sqr()).sum::<f32>()
                / SPB as f32;
            if p < self.noise * GATE_FACTOR {
                self.noise += NOISE_ALPHA * (p - self.noise);
                self.cursor += SPB as u64;
                continue;
            }
            // Candidate: search the template in the next ~2 slots.
            if let Some(offset) = self.hunt_template(rel) {
                if std::env::var("AIS_DEBUG").is_ok() {
                    eprintln!("anchor at {}", self.base + (rel + offset) as u64);
                }
                self.pending = Some(self.base + (rel + offset) as u64);
            } else {
                self.cursor += (SPB * 8) as u64; // skip ahead a byte
            }
        }

        // Trim consumed samples (keep one burst of history).
        let keep_from = self.cursor.saturating_sub(burst_len as u64);
        while self.base < keep_from && !self.buf.is_empty() {
            self.buf.pop_front();
            self.base += 1;
        }
        out
    }

    /// Correlate the template over a short window after `rel`; returns
    /// the offset of an accepted anchor.
    fn hunt_template(&self, rel: usize) -> Option<usize> {
        let tmpl_len = self.template.len();
        let span = SPB * 64; // search ~64 bits ahead of the gate
        let mut best = (0.0f32, 0usize);
        for off in 0..span {
            let s0 = rel + off;
            if s0 + tmpl_len >= self.buf.len() {
                break;
            }
            // Differential-coherent metric is CFO-immune enough for the
            // anchor: corr of (r·conj(template)) self-consistency via
            // per-quarter-template phase agreement.
            let mut acc = [Complex::new(0.0f32, 0.0); 4];
            let mut energy = 0.0f32;
            for (k, &t) in self.template.iter().enumerate() {
                let r = self.buf[s0 + k];
                acc[k * 4 / tmpl_len] += r * t.conj();
                energy += r.norm_sqr();
            }
            if energy < 1e-12 {
                continue;
            }
            // CFO rotates the four partial sums by a constant step;
            // the magnitude of the differential combination is
            // rotation-invariant.
            let m = (acc[0].norm() + acc[1].norm() + acc[2].norm() + acc[3].norm())
                / (energy * tmpl_len as f32).sqrt();
            if m > best.0 {
                best = (m, off);
            }
        }
        if std::env::var("AIS_DEBUG").is_ok() && best.0 > 0.3 {
            eprintln!("  hunt rel {rel}: best m={:.3} off={}", best.0, best.1);
        }
        // Only accept a peak that lies strictly inside the window: a
        // best at the trailing edge is the rising shoulder of a burst
        // still entering the span — anchoring there hands the Viterbi a
        // mis-timed window and skips the real one. The cursor advance
        // recenters it into the next hunt.
        (best.0 > CORR_THRESHOLD && best.1 + SPB * 8 < span).then_some(best.1)
    }

    /// Anchor accepted: estimate CFO + phase, Viterbi the payload.
    fn try_decode(&self, window: &[Complex<f32>]) -> Option<Vec<u8>> {
        let tmpl_len = self.template.len();
        // CFO estimate: maximize coherent template correlation on a grid.
        let mut best: Option<(f32, f32, Complex<f32>)> = None; // (|corr|, cfo, corr)
        let mut cfo = -CFO_RANGE_HZ;
        while cfo <= CFO_RANGE_HZ {
            let step = Complex::from_polar(1.0, -2.0 * PI * cfo / self.fs);
            let mut rot = Complex::new(1.0f32, 0.0);
            let mut corr = Complex::new(0.0f32, 0.0);
            for (k, &t) in self.template.iter().enumerate() {
                corr += window[k] * rot * t.conj();
                rot *= step;
            }
            if best.is_none() || corr.norm() > best.unwrap().0 {
                best = Some((corr.norm(), cfo, corr));
            }
            cfo += CFO_STEP_HZ;
        }
        let (_, cfo, _) = best?;
        // Fractional timing: the anchor is integer-sample, but at 5
        // samples/bit a ±0.5-sample offset already skews every bit
        // correlation by ±10 % of a bit. Evaluate the template at
        // sub-sample offsets (linear interpolation) and keep the best;
        // the whole window is then resampled at that offset.
        let mut best_frac = (0.0f32, 0.0f32); // (|corr|, frac)
        for k in -2i32..=2 {
            let frac = k as f32 * 0.25;
            let mut corr = Complex::new(0.0f32, 0.0);
            for (i, &t) in self.template.iter().enumerate() {
                let x = sample_frac(window, i as f32 + frac);
                corr += x * t.conj();
            }
            if corr.norm() > best_frac.0 {
                best_frac = (corr.norm(), frac);
            }
        }
        let window: Vec<Complex<f32>> = if best_frac.1 != 0.0 {
            (0..window.len())
                .map(|i| sample_frac(window, i as f32 + best_frac.1))
                .collect()
        } else {
            window.to_vec()
        };
        let window = &window[..];
        // Fine CFO: phase slope between the two template halves at the
        // grid winner (the coarse grid leaves up to ±75 Hz, which would
        // integrate to radians of drift across a 26 ms burst).
        let step = Complex::from_polar(1.0, -2.0 * PI * cfo / self.fs);
        let mut rot = Complex::new(1.0f32, 0.0);
        let mut c1 = Complex::new(0.0f32, 0.0);
        let mut c2 = Complex::new(0.0f32, 0.0);
        for (k, &t) in self.template.iter().enumerate() {
            let v = window[k] * rot * t.conj();
            if k < tmpl_len / 2 {
                c1 += v;
            } else {
                c2 += v;
            }
            rot *= step;
        }
        let dphi = (c2 * c1.conj()).arg();
        let cfo = cfo + dphi / (2.0 * PI * (tmpl_len as f32 / 2.0) / self.fs);
        // Re-correlate at the refined CFO for the carrier phase.
        let step = Complex::from_polar(1.0, -2.0 * PI * cfo / self.fs);
        let mut rot = Complex::new(1.0f32, 0.0);
        let mut corr = Complex::new(0.0f32, 0.0);
        for (k, &t) in self.template.iter().enumerate() {
            corr += window[k] * rot * t.conj();
            rot *= step;
        }
        let phase0 = corr.arg();

        // De-rotate the payload region by CFO and the carrier phase.
        let step = Complex::from_polar(1.0, -2.0 * PI * cfo / self.fs);
        let mut rot = Complex::from_polar(1.0, -phase0)
            * Complex::from_polar(1.0, -2.0 * PI * cfo / self.fs * tmpl_len as f32);
        // The template waveform ends at a known phase; remove it so the
        // trellis starts at quadrant 0.
        let tmpl_end_phase = self.template.last().unwrap().arg();
        rot *= Complex::from_polar(1.0, -tmpl_end_phase);

        let payload = &window[tmpl_len..];
        let nbits = payload.len() / SPB;

        // 4-state Viterbi over the phase quadrant; transitions are
        // level ±1 advancing the quadrant by ±1 (π/2). NRZI: the data
        // bit is 1 when the level repeats, 0 when it toggles — that
        // needs the previous level, so the state is (quadrant, level):
        // 8 states.
        const NS: usize = 8; // quadrant(4) × prev level(2)
        let mut metric = [f32::NEG_INFINITY; NS];
        // Start: quadrant 0; the template's last level is known: the
        // flag ends …1110 → last bit 0 toggles; template levels end at
        // a deterministic level. nrzi_levels start +1 over the 24 tmpl
        // bits: recompute to know the final level.
        let mut tmpl_bits = Vec::new();
        for k in 0..16 {
            tmpl_bits.push((k % 2) as u8);
        }
        tmpl_bits.extend([0, 1, 1, 1, 1, 1, 1, 0]);
        let last_level = *nrzi_levels(&tmpl_bits).last().unwrap();
        let l0 = if last_level > 0.0 { 1usize } else { 0 };
        metric[l0] = 0.0; // quadrant 0, known level
        // Also allow the opposite-sign anchor (template sign ambiguity
        // is absorbed by phase0, so quadrant 2 with flipped level):
        metric[2 * 2 + (1 - l0)] = 0.0;

        let mut paths: Vec<Vec<u8>> = vec![Vec::with_capacity(nbits); NS];
        let mut rot_k = rot;
        // Decision-directed phase tracking: the residual angle of each
        // bit's winning correlation drives a slow trim that absorbs the
        // remaining CFO error and the GMSK-vs-MSK pulse mismatch.
        let mut trim = Complex::new(1.0f32, 0.0);
        const PHASE_GAIN: f32 = 0.25;
        for bit in 0..nbits {
            // Precompute the two expected bit waveforms (level ±1) from
            // quadrant 0; other quadrants are i^q rotations.
            let s = &payload[bit * SPB..(bit + 1) * SPB];
            // Correlate against +level ramp and −level ramp.
            let mut c_up = Complex::new(0.0f32, 0.0);
            let mut c_dn = Complex::new(0.0f32, 0.0);
            let mut r = rot_k;
            for (k, &x) in s.iter().enumerate() {
                let ph = (k as f32 + 0.5) * PI / 2.0 / SPB as f32;
                let xr = x * r * trim;
                c_up += xr * Complex::from_polar(1.0, ph).conj();
                c_dn += xr * Complex::from_polar(1.0, -ph).conj();
                r *= step;
            }
            rot_k = r;

            let mut nm = [f32::NEG_INFINITY; NS];
            let mut np: Vec<Vec<u8>> = vec![Vec::new(); NS];
            for q in 0..4 {
                // Quadrant rotation: expected waveform × i^q ⇒ correlate
                // received × conj(i^q).
                let qrot = Complex::from_polar(1.0, -(q as f32) * PI / 2.0);
                let up = (c_up * qrot).re;
                let dn = (c_dn * qrot).re;
                for lev in 0..2 {
                    let st = q * 2 + lev;
                    if metric[st] == f32::NEG_INFINITY {
                        continue;
                    }
                    for (new_lev, gain, dq) in [(1usize, up, 1i32), (0usize, dn, -1)] {
                        let nq = ((q as i32 + dq).rem_euclid(4)) as usize;
                        let nst = nq * 2 + new_lev;
                        let m = metric[st] + gain;
                        if m > nm[nst] {
                            nm[nst] = m;
                            let mut p = paths[st].clone();
                            // NRZI: 1 = level unchanged, 0 = toggled.
                            p.push((new_lev == lev) as u8);
                            np[nst] = p;
                        }
                    }
                }
            }
            // Phase trim from the globally best branch this bit: its
            // correlation should be real-positive; rotate against the
            // residual.
            let mut best_resid: Option<(f32, Complex<f32>)> = None;
            for q in 0..4 {
                let qrot = Complex::from_polar(1.0, -(q as f32) * PI / 2.0);
                for c in [c_up * qrot, c_dn * qrot] {
                    if best_resid.is_none() || c.re > best_resid.unwrap().0 {
                        best_resid = Some((c.re, c));
                    }
                }
            }
            if let Some((_, c)) = best_resid {
                if c.norm() > 1e-9 {
                    trim *= Complex::from_polar(1.0, -PHASE_GAIN * c.arg());
                }
            }
            metric = nm;
            paths = np;
        }

        let bestst = (0..NS).max_by(|&a, &b| metric[a].partial_cmp(&metric[b]).unwrap())?;
        if metric[bestst] == f32::NEG_INFINITY {
            return None;
        }
        Some(paths[bestst].clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn template_matches_modulator_preamble() {
        // The MSK template correlates strongly against the GMSK
        // modulator's rendition of the same bits.
        let mut bits = Vec::new();
        for k in 0..16 {
            bits.push((k % 2) as u8);
        }
        bits.extend([0, 1, 1, 1, 1, 1, 1, 0]);
        let levels = nrzi_levels(&bits);
        let tmpl = msk_waveform(&levels);
        assert_eq!(tmpl.len(), TMPL_BITS * SPB);
        let last = tmpl.last().unwrap();
        assert!((last.norm() - 1.0).abs() < 1e-5);
    }
}
