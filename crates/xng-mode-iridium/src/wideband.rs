//! Wideband Iridium front end: FFT burst detection across the whole
//! capture (gr-iridium's architecture — facts from iridium-sniffer's
//! ARCHITECTURE.md, GPL, facts only: sliding FFT with per-bin adaptive
//! noise floor, ~40 kHz burst grouping, per-burst downmix to the 250 kHz
//! channel rate) feeding the existing single-channel demodulator.

use crate::demod::IridiumDemod;
use crate::CHANNEL_RATE;
use num_complex::Complex;
use rayon::prelude::*;
use rustfft::Fft;
use std::sync::Arc;
use xng_dsp::Ddc;

/// Detection threshold over the per-bin noise floor (gr-iridium: 16 dB).
const THRESHOLD: f32 = 40.0; // linear ≈ 16 dB
/// Frames to average into the noise floor before detecting. Seeding the
/// floor from a single frame (its random per-bin magnitude) leaves many
/// bins initialized near a noise null, so for the EMA's whole settling
/// time those bins read tens of dB "hot" and fire spuriously — fatal at
/// the larger FFTs of higher capture rates. A short warmup mean fixes
/// the floor before any detection. 64 frames ≈ 33 ms, well before the
/// first real burst.
const WARMUP_FRAMES: u64 = 64;
/// Maximum burst duration in seconds.
const MAX_BURST_S: f64 = 0.092;
/// Time to keep capturing after the burst leaves the detector.
const POST_S: f64 = 0.012;
/// Pre-burst samples to include (preamble ramp).
const PRE_S: f64 = 0.004;
/// One-sided channel passband for the per-burst DDC. Wider than the
/// single-channel decoder's 25 kHz so the demod's ±30 kHz tone-CFO search
/// can still recover bursts whose detection centroid sits well off the
/// true channel center under spectral leakage; the extra noise this admits
/// is removed again by the demod's own matched processing.
const WIDEBAND_PASSBAND_HZ: f64 = 50_000.0;

/// Centered ("same") matched-filter convolution: output aligned to input (no
/// net group delay, symmetric taps), zero-padded at the edges.
fn matched_filter(x: &[Complex<f32>], taps: &[f32]) -> Vec<Complex<f32>> {
    let n = taps.len();
    let half = (n / 2) as isize;
    (0..x.len() as isize)
        .map(|i| {
            let mut acc = Complex::new(0.0f32, 0.0);
            for (k, &tap) in taps.iter().enumerate() {
                let idx = i + k as isize - half;
                if idx >= 0 && (idx as usize) < x.len() {
                    acc += x[idx as usize] * tap;
                }
            }
            acc
        })
        .collect()
}

struct ActiveBurst {
    /// Center bin of the detection.
    bin: usize,
    /// First detection frame index.
    start_frame: u64,
    /// Last frame the burst was seen in.
    last_frame: u64,
}

pub struct IridiumWideband {
    /// Recently extracted bursts (start_frame, last_frame, bin) for
    /// suppressing duplicate split detections.
    recent: Vec<(u64, u64, usize)>,
    input_rate: f64,
    fft: Arc<dyn Fft<f32>>,
    nfft: usize,
    window: Vec<f32>,
    /// Per-bin noise floor (asymmetric EMA).
    floor: Vec<f32>,
    /// Ring buffer of raw samples (absolute indexing).
    buf: Vec<Complex<f32>>,
    start_abs: u64,
    /// Next FFT frame index to process.
    next_frame: u64,
    /// Frames folded into the floor so far (for the warmup mean).
    floor_frames: u64,
    active: Vec<ActiveBurst>,
    /// Emit per-burst detection diagnostics (XNG_IRIDIUM_DEBUG set).
    debug: bool,
    /// Detection threshold over the per-bin noise floor (XNG_IRIDIUM_THRESHOLD).
    det_threshold: f32,
    /// Split-duplicate suppression width in bins (XNG_IRIDIUM_DEDUP_BINS).
    dedup_bins: i64,
    /// Raised-cosine matched-filter taps applied per burst (empty = disabled).
    mf_taps: Vec<f32>,
}

/// A demodulated burst: bit stream plus its frequency offset from the
/// capture center.
pub struct WidebandBurst {
    pub offset_hz: f64,
    pub bits: Vec<u8>,
}

impl IridiumWideband {
    /// `input_rate` must be an integer multiple of the 250 kHz channel
    /// rate (2.0/2.5/5/10 MHz captures qualify).
    pub fn new(input_rate: f64) -> Result<Self, String> {
        let decim = (input_rate / CHANNEL_RATE).round() as usize;
        if (input_rate - decim as f64 * CHANNEL_RATE).abs() > 1e-6 || decim == 0 {
            return Err(format!(
                "input rate {input_rate} is not an integer multiple of {CHANNEL_RATE}"
            ));
        }
        // FFT sized for ~1 kHz bins (gr-iridium: 2^round(log2(fs/1000))).
        let nfft = (input_rate / 1000.0).round() as usize;
        let nfft = 1usize << (usize::BITS - 1 - nfft.leading_zeros());
        let window: Vec<f32> = (0..nfft)
            .map(|n| {
                // Blackman.
                let x = std::f32::consts::TAU * n as f32 / (nfft - 1) as f32;
                0.42 - 0.5 * x.cos() + 0.08 * (2.0 * x).cos()
            })
            .collect();
        Ok(Self {
            input_rate,
            fft: rustfft::FftPlanner::new().plan_fft_forward(nfft),
            nfft,
            window,
            // Filled from the first frame, then a slow symmetric EMA
            // (gr-iridium averages a 512-frame history; an asymmetric
            // min-tracker would sit far below the mean of
            // exponentially-distributed noise bins and fire constantly).
            floor: Vec::new(),
            buf: Vec::new(),
            start_abs: 0,
            next_frame: 0,
            floor_frames: 0,
            active: Vec::new(),
            recent: Vec::new(),
            debug: std::env::var("XNG_IRIDIUM_DEBUG").is_ok(),
            det_threshold: crate::demod::env_f32("XNG_IRIDIUM_THRESHOLD", THRESHOLD),
            dedup_bins: crate::demod::env_f32("XNG_IRIDIUM_DEDUP_BINS", 120.0) as i64,
            // Root-raised-cosine matched filter (matched to Iridium's RRC
            // transmit pulse; gr-iridium uses RRC alpha 0.4, 51 taps). On real
            // captures it ~17x's CRC-OK IDA yield, but the strict UW-cost/timing
            // acquisition still drops some *clean* bursts post-filter, so it is
            // opt-in (XNG_IRIDIUM_RC_ALPHA=0.4) pending a demod re-tune. alpha
            // ≤ 0 disables it (default).
            mf_taps: {
                let alpha = crate::demod::env_f32("XNG_IRIDIUM_RC_ALPHA", 0.0);
                if alpha > 0.0 {
                    crate::modulate::rrc_taps(CHANNEL_RATE / crate::demod::SYMBOL_RATE, 51, alpha as f64)
                } else {
                    Vec::new()
                }
            },
        })
    }

    /// Feed wideband IQ; returns demodulated bursts (bit streams with
    /// their frequency offsets).
    pub fn process(&mut self, input: &[Complex<f32>]) -> Vec<WidebandBurst> {
        self.buf.extend_from_slice(input);
        // Bursts that finish this call; demodulated in parallel below. The
        // detection loop stays sequential (the noise-floor EMA is stateful),
        // but per-burst extract is independent and the heavy part (DDC +
        // demod over a whole burst at the input rate), so it parallelizes.
        let mut to_extract: Vec<ActiveBurst> = Vec::new();
        let frame_len = self.nfft as u64;

        // Each frame's windowed FFT magnitude spectrum is an independent
        // computation, and the FFTs dominate the per-frame cost at high
        // capture rates. Compute them all in parallel up front; the stateful
        // noise-floor + burst detection below then runs sequentially over
        // them in frame order, so the result is identical to the previous
        // inline single-threaded version.
        let buf_end = self.start_abs + self.buf.len() as u64;
        let frame_rels: Vec<usize> = {
            let mut rels = Vec::new();
            let mut nf = self.next_frame;
            while nf * frame_len + frame_len <= buf_end {
                rels.push((nf * frame_len - self.start_abs) as usize);
                nf += 1;
            }
            rels
        };
        let (fft, samples, window, nfft) = (&self.fft, &self.buf, &self.window, self.nfft);
        let mags: Vec<Vec<f32>> = frame_rels
            .par_iter()
            .map(|&rel| {
                let mut spec: Vec<Complex<f32>> = samples[rel..rel + nfft]
                    .iter()
                    .zip(window)
                    .map(|(s, &w)| s * w)
                    .collect();
                fft.process(&mut spec);
                spec.iter().map(|c| c.norm_sqr()).collect()
            })
            .collect();

        for mag in &mags {
            // Update the per-bin noise floor. Warm up with a running
            // mean over the first WARMUP_FRAMES (no detection yet), then
            // track with the slow EMA. Detecting before the floor is
            // settled produces band-wide false bursts.
            if self.floor.is_empty() {
                self.floor = vec![0.0; self.nfft];
            }
            self.floor_frames += 1;
            let warming = self.floor_frames <= WARMUP_FRAMES;
            let mut hot = vec![false; self.nfft];
            for (k, &m) in mag.iter().enumerate() {
                let f = &mut self.floor[k];
                if warming {
                    // Incremental mean of the frames seen so far.
                    *f += (m - *f) / self.floor_frames as f32;
                    continue;
                }
                hot[k] = m > *f * self.det_threshold;
                // Bursts are brief; the slow EMA dilutes them like
                // gr-iridium's 512-frame history.
                *f += (m - *f) / 512.0;
            }

            // Group contiguous hot bins (gaps ≤ 2 bridged) into burst
            // detections. No width cap: a strong burst's spectral
            // leakage can light a wide region, and the energy centroid
            // still lands on the true channel center.
            let mut k = 0usize;
            while k < self.nfft {
                if !hot[k] {
                    k += 1;
                    continue;
                }
                let start = k;
                let mut cold = 0usize;
                while k < self.nfft && cold <= 2 {
                    if hot[k] {
                        cold = 0;
                    } else {
                        cold += 1;
                    }
                    k += 1;
                }
                k -= cold; // do not consume trailing cold bins
                // Energy-weighted center bin.
                let (mut num, mut den) = (0.0f64, 0.0f64);
                for b in start..k {
                    num += mag[b] as f64 * b as f64;
                    den += mag[b] as f64;
                }
                let center = if den > 0.0 { (num / den).round() as usize } else { start };
                // Attach to an active burst within ±3 bins or start one.
                let mut attached = false;
                for a in &mut self.active {
                    if (a.bin as i64 - center as i64).unsigned_abs() <= 12
                        && a.last_frame + 2 >= self.next_frame
                    {
                        a.last_frame = self.next_frame;
                        attached = true;
                        break;
                    }
                }
                if !attached {
                    self.active.push(ActiveBurst {
                        bin: center,
                        start_frame: self.next_frame,
                        last_frame: self.next_frame,
                    });
                }
            }

            // Finish bursts that have gone quiet (or run too long).
            let post_frames = (POST_S * self.input_rate / frame_len as f64).ceil() as u64;
            let max_frames = (MAX_BURST_S * self.input_rate / frame_len as f64).ceil() as u64;
            let cur = self.next_frame;
            let mut finished: Vec<ActiveBurst> = Vec::new();
            self.active.retain_mut(|a| {
                if cur >= a.last_frame + post_frames
                    || a.last_frame - a.start_frame > max_frames
                {
                    // Iridium bursts are ≥7 ms; ignore single-frame blips.
                    if a.last_frame > a.start_frame + 1 {
                        finished.push(ActiveBurst {
                            bin: a.bin,
                            start_frame: a.start_frame,
                            last_frame: a.last_frame,
                        });
                    }
                    false
                } else {
                    true
                }
            });
            for b in finished {
                // Suppress split duplicates of an already-extracted burst
                // (time overlap and nearby frequency).
                let dup = self.recent.iter().any(|&(s, e, bin)| {
                    b.start_frame <= e + 2
                        && s <= b.last_frame + 2
                        && (bin as i64 - b.bin as i64).unsigned_abs() < self.dedup_bins as u64
                });
                if dup {
                    continue;
                }
                self.recent.push((b.start_frame, b.last_frame, b.bin));
                if self.recent.len() > 8 {
                    self.recent.remove(0);
                }
                to_extract.push(b);
            }

            self.next_frame += 1;
        }

        // Demodulate finished bursts in parallel (extract is &self, reads the
        // sample buffer immutably; par_iter().collect() preserves order, so
        // the output is identical to the previous inline single-threaded
        // extraction). Done before the buffer is drained below.
        let this = &*self;
        let out: Vec<WidebandBurst> =
            to_extract.par_iter().filter_map(|b| this.extract(b)).collect();

        // Drop samples no longer needed: everything before the earliest
        // active burst (minus pre-roll) or the current frame.
        let earliest = self
            .active
            .iter()
            .map(|a| a.start_frame)
            .min()
            .unwrap_or(self.next_frame)
            * frame_len;
        let pre = (PRE_S * self.input_rate) as u64;
        let keep_from = earliest.saturating_sub(pre).max(self.start_abs);
        let drop = (keep_from - self.start_abs) as usize;
        if drop > 0 && drop <= self.buf.len() {
            self.buf.drain(..drop);
            self.start_abs = keep_from;
        }
        out
    }

    /// Downmix one finished burst to the channel rate and demodulate.
    fn extract(&self, b: &ActiveBurst) -> Option<WidebandBurst> {
        let frame_len = self.nfft as u64;
        let pre = (PRE_S * self.input_rate) as u64;
        let post = (POST_S * self.input_rate) as u64;
        let s0 = (b.start_frame * frame_len).saturating_sub(pre).max(self.start_abs);
        let s1 = ((b.last_frame + 1) * frame_len + post)
            .min(self.start_abs + self.buf.len() as u64);
        if s1 <= s0 {
            return None;
        }
        // Bin → signed frequency offset.
        let bin = b.bin;
        let f_off = if bin <= self.nfft / 2 {
            bin as f64 * self.input_rate / self.nfft as f64
        } else {
            (bin as f64 - self.nfft as f64) * self.input_rate / self.nfft as f64
        };
        if self.debug {
            eprintln!(
                "burst: bin {bin}, offset {f_off:+.0} Hz, frames {}..{} (t={:.4}s, {} frames)",
                b.start_frame,
                b.last_frame,
                (b.start_frame * frame_len) as f64 / self.input_rate,
                b.last_frame - b.start_frame + 1,
            );
        }
        // Mix to baseband and decimate with a proper windowed-sinc DDC.
        // A boxcar-of-decim averager is a poor anti-alias filter: its sinc
        // sidelobes fold ~8 dB of wideband noise into the 250 kHz channel
        // (measured peak/mean 8.5 dB vs 16.6 dB through a real anti-alias
        // FIR on the same burst) — enough to push a marginal burst below
        // the demod's acquisition gate. The DDC's two-stage Blackman-Harris
        // FIR rejects the out-of-channel noise the same way the
        // single-channel decoder does.
        let rel0 = (s0 - self.start_abs) as usize;
        let rel1 = (s1 - self.start_abs) as usize;
        let mut ddc = Ddc::new(self.input_rate, CHANNEL_RATE, f_off, WIDEBAND_PASSBAND_HZ).ok()?;
        let mut chan: Vec<Complex<f32>> = Vec::new();
        ddc.process(&self.buf[rel0..rel1], &mut chan);
        // Raised-cosine matched filter (gr-iridium-style) before the noise
        // estimate + demod: lifts per-symbol SNR a few dB, lowering the UW
        // acquisition cost and cleaning the bits the IDA FEC sees.
        if !self.mf_taps.is_empty() {
            chan = matched_filter(&chan, &self.mf_taps);
        }

        // Robust noise-floor estimate (20th percentile of channel power):
        // the segment is pre-roll + burst + post, so a low order statistic
        // lands in the noise regardless of burst length. This seeds the
        // demod, whose own EMA floor would not converge within this short
        // isolated segment (see IridiumDemod::seed_noise).
        let mut powers: Vec<f32> = chan.iter().map(|s| s.norm_sqr()).collect();
        let noise_floor = if powers.is_empty() {
            1.0
        } else {
            let k = powers.len() / 5;
            powers.select_nth_unstable_by(k, |a, b| a.total_cmp(b));
            powers[k]
        };
        if self.debug {
            let n = chan.len().max(1);
            let mean = chan.iter().map(|s| s.norm_sqr()).sum::<f32>() / n as f32;
            let peak = chan.iter().map(|s| s.norm_sqr()).fold(0.0f32, f32::max);
            eprintln!(
                "  channel: {} samples, mean pwr {:.2e}, peak pwr {:.2e}, noise {:.2e}, peak/noise {:.1} dB",
                chan.len(),
                mean,
                peak,
                noise_floor,
                10.0 * (peak / noise_floor.max(1e-12)).log10()
            );
        }
        // Quiet tail so the demod's burst-end detection and lookahead
        // complete within this call.
        chan.extend(std::iter::repeat(Complex::new(0.0f32, 0.0)).take((CHANNEL_RATE * 0.15) as usize));

        let mut demod = IridiumDemod::new(CHANNEL_RATE);
        demod.seed_noise(noise_floor);
        let mut bits_out = demod.process(&chan);
        bits_out
            .pop()
            .map(|b| WidebandBurst { offset_hz: f_off + b.cfo_hz, bits: b.bits })
    }
}
