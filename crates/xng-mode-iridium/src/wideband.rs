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
/// Burst spectral width for the detection mask (gr-iridium fft_burst_tagger
/// default). Each detected burst masks ±BURST_WIDTH_HZ/2 so neighbouring
/// duplex channels (~41.7 kHz apart) are detected separately, not merged.
const BURST_WIDTH_HZ: f32 = 40_000.0;
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
    /// Burst-mask half-width in bins (±this is masked around each detected
    /// burst so neighbours stay separate). From XNG_IRIDIUM_BURST_WIDTH_HZ.
    half_width_bins: usize,
    /// Recent decoded-bit hashes, to drop duplicate re-detections of one burst
    /// (a strong burst's skirt re-detected off center decodes to the same bits).
    recent_bits: std::collections::VecDeque<u64>,
    /// Raised-cosine matched-filter taps applied per burst (empty = disabled).
    mf_taps: Vec<f32>,
}

/// A demodulated burst: bit stream plus its frequency offset from the capture
/// center. `bits` is the unfiltered demod; `alt_bits` is the matched-filter
/// alternate (present only when it differs). The frame/CRC layer decodes
/// `bits` first and uses `alt_bits` only if the primary yields no valid frame —
/// so clean strong bursts stay bit-exact while weak bursts get the matched
/// filter's rescue.
pub struct WidebandBurst {
    pub offset_hz: f64,
    pub bits: Vec<u8>,
    pub alt_bits: Option<Vec<u8>>,
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
            debug: std::env::var("XNG_IRIDIUM_DEBUG").is_ok(),
            det_threshold: crate::demod::env_f32("XNG_IRIDIUM_THRESHOLD", THRESHOLD),
            half_width_bins: {
                // ±20 kHz mask around each burst (gr-iridium burst_width 40 kHz).
                // Duplex channels are ~41.7 kHz apart, so neighbours are not
                // masked into one another.
                let bw = crate::demod::env_f32("XNG_IRIDIUM_BURST_WIDTH_HZ", BURST_WIDTH_HZ) as f64;
                let bin_width = input_rate / nfft as f64;
                ((bw / bin_width / 2.0).round() as usize).max(1)
            },
            recent_bits: std::collections::VecDeque::new(),
            // Root-raised-cosine matched filter (matched to Iridium's RRC
            // transmit pulse; gr-iridium uses RRC alpha 0.4, 51 taps). The
            // received TX-RRC pulse convolved with this RX-RRC is a full raised
            // cosine — Nyquist, so the symbol centers carry no inter-symbol
            // interference, which the unfiltered primary path does suffer.
            // Applied as the alternate after the unfiltered demod (see
            // `extract`); on by default, XNG_IRIDIUM_RC_ALPHA=0 disables it.
            mf_taps: {
                let alpha = crate::demod::env_f32("XNG_IRIDIUM_RC_ALPHA", 0.4) as f64;
                if alpha > 0.0 {
                    crate::modulate::rrc_taps(CHANNEL_RATE / crate::demod::SYMBOL_RATE, 51, alpha)
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

        let post_frames = (POST_S * self.input_rate / frame_len as f64).ceil() as u64;
        let max_frames = (MAX_BURST_S * self.input_rate / frame_len as f64).ceil() as u64;
        let hw = self.half_width_bins as i64;
        for mag in &mags {
            // Update the per-bin noise floor. Warm up with a running mean over
            // the first WARMUP_FRAMES (no detection yet), then track with a slow
            // EMA. Detecting before the floor is settled produces band-wide
            // false bursts.
            if self.floor.is_empty() {
                self.floor = vec![0.0; self.nfft];
            }
            self.floor_frames += 1;
            if self.floor_frames <= WARMUP_FRAMES {
                for (k, &m) in mag.iter().enumerate() {
                    self.floor[k] += (m - self.floor[k]) / self.floor_frames as f32;
                }
                self.next_frame += 1;
                continue;
            }

            // Per-peak burst detection with a fixed-width burst mask
            // (gr-iridium's fft_burst_tagger). The previous contiguous-bin
            // grouping plus a 120-bin dedup dropped simultaneous duplex bursts;
            // the duplex IDA band packs many bursts ~42 kHz apart, so masking
            // only each claimed ±half_width region and letting the next
            // strongest peak start its own burst recovers them.
            let mut hot = vec![false; self.nfft];
            for k in 0..self.nfft {
                let f = self.floor[k];
                hot[k] = f > 0.0 && mag[k] > f * self.det_threshold;
            }

            // Extend active bursts whose center still carries energy.
            for a in &mut self.active {
                let c = a.bin as i64;
                if (-1..=1).any(|d| {
                    let b = c + d;
                    b >= 0 && (b as usize) < self.nfft && hot[b as usize]
                }) {
                    a.last_frame = self.next_frame;
                }
            }

            // Mask the ±half_width region of every active burst (true = bin
            // available), so an in-progress burst spawns no duplicate.
            let mut mask = vec![true; self.nfft];
            for a in &self.active {
                let lo = (a.bin as i64 - hw).max(0) as usize;
                let hi = (a.bin as i64 + hw).min(self.nfft as i64 - 1) as usize;
                mask[lo..=hi].fill(false);
            }

            // Every available hot bin is a candidate peak; claim them strongest
            // first.
            let mut peaks: Vec<(usize, f32)> = Vec::new();
            for k in hw as usize..self.nfft - hw as usize {
                if mask[k] && hot[k] {
                    peaks.push((k, mag[k] / self.floor[k]));
                }
            }
            peaks.sort_unstable_by(|a, b| b.1.total_cmp(&a.1));
            for (peak_bin, _) in peaks {
                if !mask[peak_bin] {
                    continue;
                }
                // Center the burst on the energy centroid of its contiguous hot
                // run so a slightly off-center spectral peak still yields the
                // true channel center for the DDC.
                let mut lo = peak_bin;
                while lo > 0 && mask[lo - 1] && hot[lo - 1] {
                    lo -= 1;
                }
                let mut hi = peak_bin;
                while hi + 1 < self.nfft && mask[hi + 1] && hot[hi + 1] {
                    hi += 1;
                }
                let (mut num, mut den) = (0.0f64, 0.0f64);
                for b in lo..=hi {
                    num += mag[b] as f64 * b as f64;
                    den += mag[b] as f64;
                }
                let center = if den > 0.0 { (num / den).round() as usize } else { peak_bin };
                self.active.push(ActiveBurst {
                    bin: center,
                    start_frame: self.next_frame,
                    last_frame: self.next_frame,
                });
                let lo = (center as i64 - hw).max(0) as usize;
                let hi = (center as i64 + hw).min(self.nfft as i64 - 1) as usize;
                mask[lo..=hi].fill(false);
            }

            // Track the noise floor only on bins not covered by a burst, so a
            // sustained burst never lifts its own bins' floor (gr-iridium
            // freezes its whole baseline during a burst; per-bin is finer).
            for (k, &m) in mag.iter().enumerate() {
                if mask[k] {
                    self.floor[k] += (m - self.floor[k]) / 512.0;
                }
            }

            // Finish bursts that have gone quiet (or run too long). The mask
            // already prevents split duplicates, so no frequency dedup here.
            let cur = self.next_frame;
            self.active.retain_mut(|a| {
                if cur >= a.last_frame + post_frames
                    || a.last_frame - a.start_frame > max_frames
                {
                    // Iridium bursts are ≥7 ms; ignore single-frame blips.
                    if a.last_frame > a.start_frame + 1 {
                        to_extract.push(ActiveBurst {
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

            self.next_frame += 1;
        }

        // Demodulate finished bursts in parallel (extract is &self, reads the
        // sample buffer immutably; par_iter().collect() preserves order, so
        // the output is identical to the previous inline single-threaded
        // extraction). Done before the buffer is drained below.
        let this = &*self;
        let mut out: Vec<WidebandBurst> =
            to_extract.par_iter().flat_map_iter(|b| this.extract(b)).collect();

        // Drop duplicate decodes. A strong burst's spectral skirt can light
        // bins past its ±half_width mask and be re-detected off center; the
        // wide per-burst DDC then re-recovers the same burst from a nearby bin,
        // so two detections at ~the same frequency yield identical bit streams.
        // Key on the bits AND a coarse frequency bucket: a genuinely distinct
        // channel (or a zero-order-hold image hundreds of kHz away) lands in a
        // different bucket and is kept, while a same-frequency re-recovery is
        // dropped.
        out.retain(|b| {
            let mut h = std::collections::hash_map::DefaultHasher::new();
            std::hash::Hash::hash(&b.bits, &mut h);
            let bucket = (b.offset_hz / 60_000.0).round() as i64;
            let key = std::hash::Hasher::finish(&h) ^ (bucket as u64).wrapping_mul(0x9e3779b97f4a7c15);
            if self.recent_bits.contains(&key) {
                return false;
            }
            self.recent_bits.push_back(key);
            if self.recent_bits.len() > 256 {
                self.recent_bits.pop_front();
            }
            true
        });

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
    fn extract(&self, b: &ActiveBurst) -> Vec<WidebandBurst> {
        let frame_len = self.nfft as u64;
        let pre = (PRE_S * self.input_rate) as u64;
        let post = (POST_S * self.input_rate) as u64;
        let s0 = (b.start_frame * frame_len).saturating_sub(pre).max(self.start_abs);
        let s1 = ((b.last_frame + 1) * frame_len + post)
            .min(self.start_abs + self.buf.len() as u64);
        if s1 <= s0 {
            return Vec::new();
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
        let Ok(mut ddc) = Ddc::new(self.input_rate, CHANNEL_RATE, f_off, WIDEBAND_PASSBAND_HZ) else {
            return Vec::new();
        };
        let mut chan: Vec<Complex<f32>> = Vec::new();
        ddc.process(&self.buf[rel0..rel1], &mut chan);
        // Emit BOTH the unfiltered and matched-filtered demod candidates and let
        // the downstream frame/CRC decode keep whichever is valid. The two
        // populations overlap in SNR and in UW-fit cost, so neither an SNR nor a
        // cost threshold can route them; only the full data CRC distinguishes a
        // clean burst (which the matched filter would corrupt) from a weak one
        // the matched filter rescues. Corrupt candidates fail the frame decode
        // downstream and drop out; identical-bit duplicates are deduped here.
        let mf_chan = if self.mf_taps.is_empty() {
            None
        } else {
            Some(matched_filter(&chan, &self.mf_taps))
        };
        let primary = self.demod_channel(chan, f_off);
        let alt = mf_chan.and_then(|mf| self.demod_channel(mf, f_off));
        let burst = match (primary, alt) {
            (Some(mut p), Some(a)) => {
                if p.bits != a.bits {
                    p.alt_bits = Some(a.bits);
                }
                Some(p)
            }
            // Only one path decoded (or neither): no alternate to carry.
            (p, a) => p.or(a),
        };
        burst.into_iter().collect()
    }

    /// Seed the per-burst noise floor and run the single-channel demodulator on
    /// one (optionally matched-filtered) channel segment; returns the first
    /// burst it recovers, offset back to the capture center.
    fn demod_channel(&self, mut chan: Vec<Complex<f32>>, f_off: f64) -> Option<WidebandBurst> {
        // Robust noise-floor estimate (20th percentile of channel power): the
        // segment is pre-roll + burst + post, so a low order statistic lands in
        // the noise regardless of burst length. Seeds the demod, whose own EMA
        // floor would not converge within this short isolated segment.
        let noise_floor = if chan.is_empty() {
            1.0
        } else {
            let mut powers: Vec<f32> = chan.iter().map(|s| s.norm_sqr()).collect();
            let k = powers.len() / 5;
            powers.select_nth_unstable_by(k, |a, b| a.total_cmp(b));
            powers[k]
        };
        // Quiet tail so the demod's burst-end detection and lookahead complete.
        chan.extend(std::iter::repeat(Complex::new(0.0f32, 0.0)).take((CHANNEL_RATE * 0.15) as usize));
        let mut demod = IridiumDemod::new(CHANNEL_RATE);
        demod.seed_noise(noise_floor);
        demod
            .process(&chan)
            .pop()
            .map(|b| WidebandBurst { offset_hz: f_off + b.cfo_hz, bits: b.bits, alt_bits: None })
    }
}
