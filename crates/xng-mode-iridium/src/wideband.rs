//! Wideband Iridium front end: FFT burst detection across the whole
//! capture (gr-iridium's architecture — facts from iridium-sniffer's
//! ARCHITECTURE.md, GPL, facts only: sliding FFT with per-bin adaptive
//! noise floor, ~40 kHz burst grouping, per-burst downmix to the 250 kHz
//! channel rate) feeding the existing single-channel demodulator.

use crate::demod::IridiumDemod;
use crate::CHANNEL_RATE;
use num_complex::Complex;
use rustfft::Fft;
use std::sync::Arc;

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
    decim: usize,
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
            decim,
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
        })
    }

    /// Feed wideband IQ; returns demodulated bursts (bit streams with
    /// their frequency offsets).
    pub fn process(&mut self, input: &[Complex<f32>]) -> Vec<WidebandBurst> {
        self.buf.extend_from_slice(input);
        let mut out = Vec::new();
        let frame_len = self.nfft as u64;

        loop {
            let frame_start = self.next_frame * frame_len;
            if frame_start + frame_len > self.start_abs + self.buf.len() as u64 {
                break;
            }
            let rel = (frame_start - self.start_abs) as usize;
            let mut spec: Vec<Complex<f32>> = self.buf[rel..rel + self.nfft]
                .iter()
                .zip(&self.window)
                .map(|(s, &w)| s * w)
                .collect();
            self.fft.process(&mut spec);
            let mag: Vec<f32> = spec.iter().map(|c| c.norm_sqr()).collect();

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
                hot[k] = m > *f * THRESHOLD;
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
                        && (bin as i64 - b.bin as i64).unsigned_abs() < 120
                });
                if dup {
                    continue;
                }
                self.recent.push((b.start_frame, b.last_frame, b.bin));
                if self.recent.len() > 8 {
                    self.recent.remove(0);
                }
                if let Some(burst) = self.extract(&b) {
                    out.push(burst);
                }
            }

            self.next_frame += 1;
        }

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
        // Mix to baseband and decimate with a simple boxcar-of-decim FIR
        // (the demod's own processing tolerates the soft rolloff).
        let rel0 = (s0 - self.start_abs) as usize;
        let rel1 = (s1 - self.start_abs) as usize;
        let step = -std::f64::consts::TAU * f_off / self.input_rate;
        let mut chan: Vec<Complex<f32>> = Vec::with_capacity((rel1 - rel0) / self.decim + 1);
        let mut acc = Complex::new(0.0f32, 0.0);
        let mut n = 0usize;
        for (i, s) in self.buf[rel0..rel1].iter().enumerate() {
            let ph = step * (rel0 + i) as f64;
            acc += s * Complex::from_polar(1.0, ph as f32);
            n += 1;
            if n == self.decim {
                chan.push(acc / self.decim as f32);
                acc = Complex::new(0.0, 0.0);
                n = 0;
            }
        }
        // Quiet tail so the demod's burst-end detection and lookahead
        // complete within this call.
        chan.extend(std::iter::repeat(Complex::new(0.0f32, 0.0)).take((CHANNEL_RATE * 0.15) as usize));

        let mut demod = IridiumDemod::new(CHANNEL_RATE);
        let mut bits_out = demod.process(&chan);
        bits_out
            .pop()
            .map(|b| WidebandBurst { offset_hz: f_off + b.cfo_hz, bits: b.bits })
    }
}
