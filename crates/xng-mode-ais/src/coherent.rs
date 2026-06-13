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
/// Anchor acceptance at live effort: cheap, proven safe.
const CORR_THRESHOLD_LIVE: f32 = 0.72;
/// Max effort digs to the deep-weak detection floor (off-air miss
/// bursts peak at 0.56–0.60); the rescue acceptance policy absorbs
/// the extra false anchors, CPU pays ~3×.
const CORR_THRESHOLD_MAX: f32 = 0.55;
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

/// GMSK (BT = 0.4) integrated phase pulse q̃(t): 0 → 1 over ±2 bit
/// periods (same Gaussian math as the test modulator).
fn gmsk_qtilde(t: f32) -> f32 {
    // Cached lookup: the erf integration below is only run once to fill
    // a fine table (the function is called per waveform sample).
    static TABLE: std::sync::OnceLock<Vec<f32>> = std::sync::OnceLock::new();
    let table = TABLE.get_or_init(|| {
        (0..=1200).map(|i| gmsk_qtilde_exact(-3.0 + i as f32 * 0.005)).collect()
    });
    if t <= -3.0 {
        return 0.0;
    }
    if t >= 3.0 {
        return 1.0;
    }
    let idx = ((t + 3.0) / 0.005) as usize;
    table[idx.min(1200)]
}

fn gmsk_qtilde_exact(t: f32) -> f32 {
    // Integral of the Gaussian frequency pulse; erf approximation
    // (Abramowitz–Stegun 7.1.26).
    let erf = |x: f32| -> f32 {
        let s = if x < 0.0 { -1.0 } else { 1.0 };
        let x = x.abs();
        let t = 1.0 / (1.0 + 0.327_591_1 * x);
        let y = 1.0
            - (((((1.061_405_429 * t - 1.453_152_027) * t) + 1.421_413_741) * t
                - 0.284_496_736)
                * t
                + 0.254_829_592)
                * t
                * (-x * x).exp();
        s * y
    };
    const BT: f32 = 0.4;
    let a = (2.0 * PI / (2.0f32.ln()).sqrt()) * BT;
    // q̃(t) = ∫_{-∞}^{t} g; with g = (erf-difference)/2 form, integrate
    // numerically once over a fine grid (cached).
    // Cheap closed-enough form: integrate g(u) du with small steps.
    let mut acc = 0.0f32;
    let mut u = -3.0f32;
    while u < t.min(3.0) {
        let g = 0.5 * (erf(a * (u + 0.5) / std::f32::consts::SQRT_2)
            - erf(a * (u - 0.5) / std::f32::consts::SQRT_2));
        acc += g * 0.01;
        u += 0.01;
    }
    if t >= 3.0 { 1.0 } else { acc.clamp(0.0, 1.0) }
}

/// Per-(prev,cur,next) middle-bit waveforms: 5 samples of relative
/// phase (radians) above the completed-pulse reference, plus the
/// quadrant advance contributed when `prev` completes.
fn gmsk_bit_waveforms() -> [[f32; SPB]; 8] {
    let mut out = [[0.0f32; SPB]; 8];
    for (idx, w) in out.iter_mut().enumerate() {
        let lp = if idx & 4 != 0 { 1.0f32 } else { -1.0 };
        let lc = if idx & 2 != 0 { 1.0f32 } else { -1.0 };
        let ln = if idx & 1 != 0 { 1.0f32 } else { -1.0 };
        for (j, v) in w.iter_mut().enumerate() {
            let t = (j as f32 + 0.5) / SPB as f32; // within [0,1) of cur bit
            // φ_rel(t) = (π/2)[ lp·q̃(t+1) + lc·q̃(t) + ln·q̃(t−1) ]
            // minus lp's completed value at the window start reference
            // (the state's quadrant holds Σ levels through prev−1, and
            // prev's pulse is still partially in flight).
            *v = (PI / 2.0)
                * (lp * gmsk_qtilde(t + 1.0) + lc * gmsk_qtilde(t) + ln * gmsk_qtilde(t - 1.0));
        }
    }
    out
}

/// MSK (rectangular frequency) waveform table in the same
/// (prev,cur,next) format: the phase ramp depends on `cur` only.
/// Real transmitters vary in pulse shaping; the decoder runs both
/// hypotheses per burst and the HDLC FCS arbitrates.
fn msk_bit_waveforms() -> [[f32; SPB]; 8] {
    let mut out = [[0.0f32; SPB]; 8];
    for (idx, w) in out.iter_mut().enumerate() {
        let lc = if idx & 2 != 0 { 1.0f32 } else { -1.0 };
        let lp = if idx & 4 != 0 { 1.0f32 } else { -1.0 };
        for (j, v) in w.iter_mut().enumerate() {
            // Completed-pulse reference matches the GMSK convention:
            // prev contributes its full remaining half above the
            // reference (q̃ → step at 0.5 for MSK), i.e. lp·π/4 + ramp.
            *v = lp * PI / 4.0 + lc * PI / 2.0 * (j as f32 + 0.5) / SPB as f32;
        }
    }
    out
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

/// GMSK baseband for a level sequence (BT = 0.4 pulse), preceded and
/// followed by an implicit continuation of the edge levels.
fn gmsk_waveform(levels: &[f32]) -> Vec<Complex<f32>> {
    let n = levels.len();
    let mut out = Vec::with_capacity(n * SPB);
    let lvl = |i: i64| -> f32 {
        if i < 0 {
            levels[0]
        } else if i as usize >= n {
            levels[n - 1]
        } else {
            levels[i as usize]
        }
    };
    let mut completed = 0.0f32; // Σ levels of fully past pulses
    for k in 0..n as i64 {
        for j in 0..SPB {
            let t = (j as f32 + 0.5) / SPB as f32;
            let phase = (PI / 2.0)
                * (completed
                    + lvl(k - 1) * gmsk_qtilde(t + 1.0)
                    + lvl(k) * gmsk_qtilde(t)
                    + lvl(k + 1) * gmsk_qtilde(t - 1.0));
            out.push(Complex::from_polar(1.0, phase));
        }
        completed += lvl(k - 1);
    }
    out
}

/// Deframe one candidate (with a leading flag) to its FCS-valid AIS
/// frame, if any.
fn valid_frame(bits: &[u8]) -> Option<crate::frame::AisFrame> {
    let mut df = crate::frame::HdlcDeframer::new();
    for &b in &[0u8, 1, 1, 1, 1, 1, 1, 0] {
        df.push_bit(b);
    }
    bits.iter().find_map(|&b| df.push_bit(b))
}

/// First FCS-valid candidate: returns (bits consumed incl. closing
/// flag, the raw stuffed payload bits actually transmitted).
fn first_valid(candidates: &[Vec<u8>]) -> Option<(usize, Vec<u8>)> {
    for bits in candidates {
        let mut df = crate::frame::HdlcDeframer::new();
        for &b in &[0u8, 1, 1, 1, 1, 1, 1, 0] {
            df.push_bit(b);
        }
        for (k, &b) in bits.iter().enumerate() {
            if df.push_bit(b).is_some() {
                // k is the index of the closing flag's final 0.
                return Some((k + 1, bits[..k + 1].to_vec()));
            }
        }
    }
    None
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
    last_hunt_best: f32,
    corr_threshold: f32,
    /// MMSIs seen in any accepted frame (rescue confirmation).
    known_mmsis: std::collections::HashSet<u32>,
    /// Rescue-decoded frames from a not-yet-confirmed MMSI, held until
    /// a second frame from the same source confirms it.
    pending_rescue: std::collections::HashMap<u32, Vec<Vec<u8>>>,
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
        let template = gmsk_waveform(&nrzi_levels(&tmpl_bits));
        Self {
            buf: VecDeque::new(),
            base: 0,
            cursor: 0,
            noise: 1e-6,
            template,
            pending: None,
            fs: fs as f32,
            last_hunt_best: 0.0,
            corr_threshold: CORR_THRESHOLD_LIVE,
            known_mmsis: std::collections::HashSet::new(),
            pending_rescue: std::collections::HashMap::new(),
        }
    }

    /// Acceptance policy. Nominal decodes pass through (single
    /// hypothesis, negligible false-FCS odds). Rescue decodes come
    /// from a ~90-way hypothesis fan-out where random FCS-16 passes
    /// become a real rate, so they additionally need a sane message
    /// type and a confirmed source: an MMSI already seen, or a second
    /// held frame from the same MMSI (random passes never repeat a
    /// source — the same policy as the ADS-B ICAO confirmation).
    fn accept(&mut self, candidates: Vec<Vec<u8>>, rescued: bool) -> Vec<Vec<u8>> {
        let mut out = Vec::new();
        for cand in candidates {
            let Some(f) = valid_frame(&cand) else {
                if !rescued {
                    out.push(cand); // invalid candidates were always passed through
                }
                continue;
            };
            if !rescued {
                self.known_mmsis.insert(f.mmsi);
                out.push(cand);
                continue;
            }
            if !(1..=27).contains(&f.msg_type) {
                continue;
            }
            if self.known_mmsis.contains(&f.mmsi) {
                out.push(cand);
                continue;
            }
            let held = self.pending_rescue.entry(f.mmsi).or_default();
            if held.iter().any(|h| h == &cand) {
                continue; // same bits from another hypothesis: not a confirmation
            }
            held.push(cand);
            if held.len() >= 2 {
                out.append(held);
                self.known_mmsis.insert(f.mmsi);
            } else if self.pending_rescue.len() > 64 {
                let k = *self.pending_rescue.keys().next().unwrap();
                self.pending_rescue.remove(&k);
            }
        }
        out
    }

    /// Max effort lowers the anchor threshold to the deep-weak floor.
    pub fn set_max_effort(&mut self, max: bool) {
        self.corr_threshold =
            if max { CORR_THRESHOLD_MAX } else { CORR_THRESHOLD_LIVE };
    }

    /// Feed samples; returns decoded post-flag bit vectors (NRZI already
    /// removed) for the HDLC deframer, paired with the template position
    /// so callers can dedup against the streaming path.
    pub fn process(&mut self, input: &[Complex<f32>]) -> Vec<Vec<u8>> {
        self.buf.extend(input.iter().copied());
        self.buf.make_contiguous();
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
                let mut decoded = self.try_decode_nominal(&window);
                let mut rescued = false;
                if first_valid(&decoded).is_none() {
                    // Rescue: the full CFO × phase-gain hypothesis grid,
                    // then shifted windows — the stride-2 hunt can
                    // anchor a few samples off and the fractional
                    // refine only spans ±0.5 (weak bursts are acutely
                    // timing-sensitive). The FCS arbitrates; rescued
                    // frames face the stricter acceptance below.
                    decoded = self.try_decode(&window);
                    if first_valid(&decoded).is_none() && s0 >= 2 {
                        // Live caps the shift search (the retries, not
                        // the hypothesis grid, dominate rescue CPU).
                        let toffs: &[i64] = if self.corr_threshold <= CORR_THRESHOLD_MAX {
                            &[-1, 1, -2, 2, -3, 3, -4, 4]
                        } else {
                            &[-1, 1, -2, 2]
                        };
                        for &toff in toffs {
                            let sh = (s0 as i64 + toff) as usize;
                            let w: Vec<Complex<f32>> =
                                self.buf.iter().skip(sh).take(burst_len).copied().collect();
                            decoded = self.try_decode(&w);
                            if first_valid(&decoded).is_some() {
                                break;
                            }
                        }
                    }
                    rescued = true;
                }
                if std::env::var("AIS_DEBUG").is_ok() {
                    eprintln!("try_decode at {}: {} candidate(s)", start, decoded.len());
                }
                // Successive interference cancellation: if a candidate
                // is FCS-valid, reconstruct its exact burst (bits are
                // known, the synthesis is the modulator's) and subtract;
                // a colliding weaker burst then gets its own pass.
                if let Some((_bits_used, frame_bits)) = first_valid(&decoded) {
                    if let Some(residual) = self.subtract_burst(&window, &frame_bits) {
                        // The colliding burst's template can sit anywhere
                        // in the residual: scan for the best anchor.
                        let tmpl_len = self.template.len();
                        let mut best = (0.0f32, 0usize);
                        let mut off = 0usize;
                        while off + tmpl_len + SPB < residual.len() {
                            let m = Self::template_metric(
                                &self.template,
                                &residual[off..off + tmpl_len],
                            );
                            if m > best.0 {
                                best = (m, off);
                            }
                            off += 1;
                        }
                        if best.0 > self.corr_threshold {
                            let rescued = self.try_decode(&residual[best.1..]);
                            if std::env::var("AIS_DEBUG").is_ok() {
                                eprintln!(
                                    "  SIC anchor m={:.3} at {}: {} candidate(s)",
                                    best.0,
                                    best.1,
                                    rescued.len()
                                );
                            }
                            let sic = self.accept(rescued, true);
                            out.extend(sic);
                        }
                    }
                }
                out.extend(self.accept(decoded, rescued));
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
            let hunted = self.hunt_template(rel);
            if let Some(offset) = hunted {
                if std::env::var("AIS_DEBUG").is_ok() {
                    eprintln!("anchor at {}", self.base + (rel + offset) as u64);
                }
                self.pending = Some(self.base + (rel + offset) as u64);
            } else if self.last_hunt_best < 0.3 {
                // Nothing remotely template-like in the whole span:
                // skip most of it (huge CPU win on always-gated noise).
                self.cursor += (SPB * 48) as u64;
            } else {
                self.cursor += (SPB * 8) as u64; // recenter the shoulder
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

    /// Reconstruct a confirmed burst (template + raw payload bits) and
    /// subtract it from the window; returns the residual for a second
    /// decode pass, or None when the fit is implausible.
    fn subtract_burst(
        &self,
        window: &[Complex<f32>],
        payload_bits: &[u8],
    ) -> Option<Vec<Complex<f32>>> {
        // Synthesize the full transmitted waveform: template bits then
        // the raw payload bits, NRZI → levels → GMSK.
        let mut bits = Vec::with_capacity(TMPL_BITS + payload_bits.len());
        for k in 0..16 {
            bits.push((k % 2) as u8);
        }
        bits.extend([0, 1, 1, 1, 1, 1, 1, 0]);
        bits.extend_from_slice(payload_bits);
        let wave = gmsk_waveform(&nrzi_levels(&bits));
        let n = wave.len().min(window.len());

        // CFO estimate over the whole known waveform (phase slope of
        // halves), then complex least-squares amplitude.
        let mut c1 = Complex::new(0.0f32, 0.0);
        let mut c2 = Complex::new(0.0f32, 0.0);
        for k in 0..n {
            let v = window[k] * wave[k].conj();
            if k < n / 2 {
                c1 += v;
            } else {
                c2 += v;
            }
        }
        let dphi = (c2 * c1.conj()).arg();
        let cfo_step = Complex::from_polar(1.0, dphi / (n as f32 / 2.0));
        let mut rot = Complex::new(1.0f32, 0.0);
        let mut num = Complex::new(0.0f32, 0.0);
        let mut den = 0.0f32;
        for k in 0..n {
            let w = wave[k] * rot;
            num += window[k] * w.conj();
            den += w.norm_sqr();
            rot *= cfo_step;
        }
        let scale = num / den.max(1e-12);
        // Sanity: the fit must explain a meaningful share of the
        // window energy, else subtraction just injects the template.
        let fit_energy = scale.norm_sqr() * den;
        let win_energy: f32 = window[..n].iter().map(|c| c.norm_sqr()).sum();
        if fit_energy < 0.25 * win_energy {
            return None;
        }
        let mut residual = window.to_vec();
        let mut rot = Complex::new(1.0f32, 0.0);
        for k in 0..n {
            residual[k] -= scale * wave[k] * rot;
            rot *= cfo_step;
        }
        Some(residual)
    }

    /// Correlate the template over a short window after `rel`; returns
    /// the offset of an accepted anchor.
    /// Differential-coherent template metric at one position (CFO-
    /// immune: the four partial sums' magnitudes are rotation-
    /// invariant).
    fn template_metric(template: &[Complex<f32>], w: &[Complex<f32>]) -> f32 {
        let tmpl_len = template.len();
        let mut acc = [Complex::new(0.0f32, 0.0); 4];
        let mut energy = 0.0f32;
        for (k, &t) in template.iter().enumerate() {
            let r = w[k];
            acc[k * 4 / tmpl_len] += r * t.conj();
            energy += r.norm_sqr();
        }
        if energy < 1e-12 {
            return 0.0;
        }
        (acc[0].norm() + acc[1].norm() + acc[2].norm() + acc[3].norm())
            / (energy * tmpl_len as f32).sqrt()
    }

    /// Returns Some(offset) on an anchor; on a miss, the caller may use
    /// `last_hunt_best` to skip far ahead when nothing template-like
    /// was in the span at all.
    fn hunt_template(&mut self, rel: usize) -> Option<usize> {
        let tmpl_len = self.template.len();
        let span = SPB * 64; // search ~64 bits ahead of the gate
        let mut best = (0.0f32, 0usize);
        let buf = self.buf.as_slices().0; // contiguous after make_contiguous
        // Stride-2 coarse pass, then ±1 refinement around the winner:
        // the metric's peak is several samples wide and the fractional
        // refine absorbs ±1 anyway. Halves the hunt cost.
        let mut off = 0;
        while off < span {
            let s0 = rel + off;
            if s0 + tmpl_len >= buf.len() {
                break;
            }
            let m = Self::template_metric(&self.template, &buf[s0..s0 + tmpl_len]);
            if m > best.0 {
                best = (m, off);
            }
            off += 2;
        }
        for off in [best.1.saturating_sub(1), best.1 + 1] {
            let s0 = rel + off;
            if s0 + tmpl_len < buf.len() {
                let m = Self::template_metric(&self.template, &buf[s0..s0 + tmpl_len]);
                if m > best.0 {
                    best = (m, off);
                }
            }
        }
        self.last_hunt_best = best.0;
        if std::env::var("AIS_DEBUG").is_ok() && best.0 > 0.3 {
            eprintln!("  hunt abs {}: best m={:.3}", self.base + (rel + best.1) as u64, best.0);
        }
        // Only accept a peak that lies strictly inside the window: a
        // best at the trailing edge is the rising shoulder of a burst
        // still entering the span — anchoring there hands the Viterbi a
        // mis-timed window and skips the real one. The cursor advance
        // recenters it into the next hunt.
        (best.0 > self.corr_threshold && best.1 + SPB * 8 < span).then_some(best.1)
    }

    /// Anchor accepted: estimate CFO + phase, Viterbi the payload with
    /// both pulse-shape hypotheses (GMSK BT=0.4 and MSK).
    fn try_decode(&self, window: &[Complex<f32>]) -> Vec<Vec<u8>> {
        static TABLES: std::sync::OnceLock<[[[f32; SPB]; 8]; 2]> = std::sync::OnceLock::new();
        let tables = TABLES.get_or_init(|| [gmsk_bit_waveforms(), msk_bit_waveforms()]);
        // Decode hypotheses beyond the nominal estimate: at deep-weak
        // SNR the template CFO estimate is noisy and the decision-
        // directed phase tracking can lose lock (gain 0 = open loop).
        // The FCS arbitrates, so wrong hypotheses cost only CPU — and
        // these run only on anchored bursts. Live trims the CFO wings
        // (the off-air rescues came from gain/timing, not CFO).
        const HYP_MAX: &[(f32, f32)] = &[
            (0.0, 0.25), (0.0, 0.1), (0.0, 0.0),
            (-60.0, 0.25), (-60.0, 0.1), (-60.0, 0.0),
            (60.0, 0.25), (60.0, 0.1), (60.0, 0.0),
            (-120.0, 0.25), (-120.0, 0.1), (-120.0, 0.0),
            (120.0, 0.25), (120.0, 0.1), (120.0, 0.0),
        ];
        let hyp = HYP_MAX;
        let mut out = Vec::new();
        for &(cfo_off, gain) in hyp {
            for table in tables {
                if let Some(bits) = self.try_decode_with(window, table, cfo_off, gain) {
                    out.push(bits);
                }
            }
        }
        out
    }

    /// The single nominal hypothesis — today's behavior; everything in
    /// `try_decode` beyond it is a rescue and gets the stricter
    /// acceptance policy in `process`.
    fn try_decode_nominal(&self, window: &[Complex<f32>]) -> Vec<Vec<u8>> {
        static TABLES: std::sync::OnceLock<[[[f32; SPB]; 8]; 2]> = std::sync::OnceLock::new();
        let tables = TABLES.get_or_init(|| [gmsk_bit_waveforms(), msk_bit_waveforms()]);
        let mut out = Vec::new();
        for table in tables {
            if let Some(bits) = self.try_decode_with(window, table, 0.0, 0.25) {
                out.push(bits);
            }
        }
        out
    }

    fn try_decode_with(
        &self,
        window: &[Complex<f32>],
        wf: &[[f32; SPB]; 8],
        cfo_off: f32,
        phase_gain: f32,
    ) -> Option<Vec<u8>> {
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
        let cfo = cfo + dphi / (2.0 * PI * (tmpl_len as f32 / 2.0) / self.fs) + cfo_off;
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


        // Start one bit early, on the template's KNOWN last bit: it
        // anchors the state (quadrant + both in-flight levels) and the
        // window boundary; its emission is dropped below.
        let payload = &window[tmpl_len - SPB..];
        let nbits = payload.len() / SPB;

        // GMSK-exact MLSE: state = (quadrant of completed pulses,
        // l_prev, l_cur) — 16 states; branch chooses l_next. The branch
        // waveform for the current bit window is the true BT=0.4 pulse
        // shape for (l_prev, l_cur, l_next); the quadrant advances by
        // l_prev as its pulse completes. NRZI emits inside transitions.
        const NS: usize = 16;
        let mut metric = [f32::NEG_INFINITY; NS];
        // Template levels end deterministically; both sign hypotheses
        // (the carrier-phase estimate absorbs a π flip).
        let mut tmpl_bits = Vec::new();
        for k in 0..16 {
            tmpl_bits.push((k % 2) as u8);
        }
        tmpl_bits.extend([0, 1, 1, 1, 1, 1, 1, 0]);
        let lv = nrzi_levels(&tmpl_bits);
        let n_t = lv.len();
        // State for the template's last-bit window (fully known):
        // quadrant = completed pulses through template[n−3] (matching
        // gmsk_waveform's accumulation, which starts with the
        // continuation pseudo-pulse lvl(−1) = levels[0]).
        let completed: f32 = lv[0] + lv[..n_t - 2].iter().sum::<f32>();
        let q0 = (completed as i32).rem_euclid(4) as usize;
        let lp0 = (lv[n_t - 2] > 0.0) as usize;
        let lc0 = (lv[n_t - 1] > 0.0) as usize;
        metric[(q0 << 2) | (lp0 << 1) | lc0] = 0.0;
        // π-flip image (carrier-phase sign ambiguity).
        metric[(((q0 + 2) % 4) << 2) | ((1 - lp0) << 1) | (1 - lc0)] = 0.0;

        // Traceback matrix instead of per-branch path clones (the
        // clones were O(n²) and dominated the live CPU budget).
        let mut back: Vec<[u8; NS]> = Vec::with_capacity(nbits);
        let mut rot_k = rot;
        let mut trim = Complex::new(1.0f32, 0.0);
        for bit in 0..nbits {
            let sm = &payload[bit * SPB..(bit + 1) * SPB];
            // Pre-rotate the received samples once.
            let mut r = rot_k;
            let mut rx = [Complex::new(0.0f32, 0.0); SPB];
            for (j, &x) in sm.iter().enumerate() {
                rx[j] = x * r * trim;
                r *= step;
            }
            rot_k = r;

            // Correlations against the 8 context waveforms (quadrant 0).
            let mut cw = [Complex::new(0.0f32, 0.0); 8];
            for (w_idx, c) in cw.iter_mut().enumerate() {
                for j in 0..SPB {
                    *c += rx[j] * Complex::from_polar(1.0, wf[w_idx][j]).conj();
                }
            }

            let mut nm = [f32::NEG_INFINITY; NS];
            let mut prev_state = [0u8; NS];
            let mut best_resid: Option<(f32, Complex<f32>)> = None;
            for st in 0..NS {
                if metric[st] == f32::NEG_INFINITY {
                    continue;
                }
                let q = st >> 2;
                let lp = (st >> 1) & 1;
                let lc = st & 1;
                let qrot = Complex::from_polar(1.0, -(q as f32) * PI / 2.0);
                for ln in 0..2usize {
                    let w_idx = (lp << 2) | (lc << 1) | ln;
                    let c = cw[w_idx] * qrot;
                    let m = metric[st] + c.re;
                    // Next state: quadrant advances by l_prev (±1).
                    let nq = if lp == 1 { (q + 1) % 4 } else { (q + 3) % 4 };
                    let nst = (nq << 2) | (lc << 1) | ln;
                    if m > nm[nst] {
                        nm[nst] = m;
                        prev_state[nst] = st as u8;
                        if best_resid.is_none() || c.re > best_resid.unwrap().0 {
                            best_resid = Some((c.re, c));
                        }
                    }
                }
            }
            if let Some((_, c)) = best_resid {
                if c.norm() > 1e-9 && phase_gain > 0.0 {
                    trim *= Complex::from_polar(1.0, -phase_gain * c.arg());
                }
            }
            metric = nm;
            back.push(prev_state);
        }

        let bestst = (0..NS).max_by(|&a, &b| metric[a].partial_cmp(&metric[b]).unwrap())?;
        if metric[bestst] == f32::NEG_INFINITY {
            return None;
        }
        // Traceback: NRZI bit k = (l_k == l_{k−1}) read from the state
        // pair along the surviving path.
        let mut bits_rev = Vec::with_capacity(nbits);
        let mut st = bestst;
        for k in (0..nbits).rev() {
            let lp = (st >> 1) & 1;
            let lc = st & 1;
            let _ = lc;
            // The emitted bit at step k compared l_cur (then) with
            // l_prev (then): from the SOURCE state of this transition.
            let src = back[k][st] as usize;
            let s_lp = (src >> 1) & 1;
            let s_lc = src & 1;
            bits_rev.push((s_lc == s_lp) as u8);
            let _ = lp;
            st = src;
        }
        bits_rev.reverse();
        // First emission is the known template bit — drop it.
        Some(bits_rev[1..].to_vec())
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
        let tmpl = gmsk_waveform(&levels);
        assert_eq!(tmpl.len(), TMPL_BITS * SPB);
        let last = tmpl.last().unwrap();
        assert!((last.norm() - 1.0).abs() < 1e-5);
    }
}
