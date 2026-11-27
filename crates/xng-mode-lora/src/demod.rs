//! LoRa receiver: preamble detection, sync word, carrier and timing offset
//! estimation from the up/down chirps, then FFT demodulation of the header
//! and payload symbols.
//!
//! Input is complex baseband at two samples per chip (`2 * bw`). Symbols are
//! dechirped and FFT'd at one sample per chip; the second sample per chip
//! gives half-chip timing resolution when the frame is aligned.

use crate::coding::{decode_header, decode_payload, payload_symbol_count, CodingParams};
use num_complex::Complex;
use rustfft::{Fft, FftPlanner};
use std::f64::consts::PI;
use std::sync::Arc;

/// Samples per chip at the demodulator input.
pub const OVERSAMPLE: usize = 2;

/// Receiver settings.
#[derive(Debug, Clone, Copy)]
pub struct RxParams {
    pub sf: u8,
    pub bw_hz: f64,
    /// Carrier frequency, used to turn carrier offset into clock drift.
    pub carrier_hz: f64,
    /// Low data rate optimization: `None` = the radios' automatic rule
    /// (on when a symbol lasts longer than 16 ms).
    pub ldro: Option<bool>,
}

impl RxParams {
    pub fn ldro(&self) -> bool {
        self.ldro
            .unwrap_or_else(|| (1u64 << self.sf) as f64 / self.bw_hz > 0.016)
    }
}

/// One received LoRa frame.
#[derive(Debug, Clone)]
pub struct LoraFrame {
    pub sf: u8,
    pub bw_hz: f64,
    pub cr: u8,
    pub ldro: bool,
    /// Sync word as programmed in the radio (0x2B, 0x12, 0x34 ...).
    pub sync_word: u8,
    pub payload: Vec<u8>,
    /// `None` when the header said there is no CRC.
    pub crc_ok: Option<bool>,
    pub snr_db: f32,
    pub cfo_hz: f32,
    pub corrected_bits: u32,
}

#[derive(Debug, Clone, Copy)]
struct Peak {
    bin: usize,
    pow: f32,
    total: f32,
    value: Complex<f32>,
    /// Interpolated bin (parabolic over neighbors), in [0, N).
    fine: f64,
}

enum State {
    Search,
    Align { start: usize },
    Frame(FrameState),
}

struct FrameState {
    /// Position (2x samples) of the first header symbol.
    data_start: f64,
    /// 2x samples per symbol, drift corrected.
    step: f64,
    /// Carrier offset in bins.
    cfo: f64,
    sync_word: u8,
    header: Option<(crate::coding::HeaderBlock, usize)>,
    snr_acc: (f64, usize),
}

pub struct Demodulator {
    p: RxParams,
    n: usize,
    fft: Arc<dyn Fft<f32>>,
    upchirp: Vec<Complex<f32>>,
    buf: Vec<Complex<f32>>,
    /// Read position of the search window in `buf` (2x samples).
    pos: usize,
    history: Vec<Peak>,
    state: State,
    scratch: Vec<Complex<f32>>,
    level: f32,
}

impl Demodulator {
    pub fn new(p: RxParams) -> Self {
        assert!((7..=12).contains(&p.sf), "SF must be 7..12");
        let n = 1usize << p.sf;
        let mut planner = FftPlanner::new();
        let fft = planner.plan_fft_forward(n);
        let upchirp = (0..n)
            .map(|i| {
                let i = i as f64;
                let ph = 2.0 * PI * (i * i / (2.0 * n as f64) - i / 2.0);
                Complex::new(ph.cos() as f32, ph.sin() as f32)
            })
            .collect();
        Self {
            p,
            n,
            fft,
            upchirp,
            buf: Vec::new(),
            pos: 0,
            history: Vec::new(),
            state: State::Search,
            scratch: vec![Complex::new(0.0, 0.0); n],
            level: -100.0,
        }
    }

    pub fn params(&self) -> &RxParams {
        &self.p
    }

    /// Smoothed input power, dBFS.
    pub fn level_dbfs(&self) -> f32 {
        self.level
    }

    /// One symbol window starting at 2x position `at`: decimate to 1x,
    /// remove `cfo` bins of carrier offset, dechirp (`up` = expect an
    /// upchirp) and FFT. `None` when the buffer is too short.
    fn window(&mut self, at: f64, cfo: f64, up: bool) -> Option<Peak> {
        if at < 0.0 {
            return None;
        }
        let start = at.floor() as usize;
        let mu = (at - start as f64) as f32;
        let n = self.n;
        if start + OVERSAMPLE * (n - 1) + 1 >= self.buf.len() {
            return None;
        }
        for i in 0..n {
            let j = start + OVERSAMPLE * i;
            let x = self.buf[j] * (1.0 - mu) + self.buf[j + 1] * mu;
            let r = if up {
                self.upchirp[i].conj()
            } else {
                self.upchirp[i]
            };
            let mut y = x * r;
            if cfo != 0.0 {
                let ph = -2.0 * PI * cfo * (at / OVERSAMPLE as f64 + i as f64) / n as f64;
                y *= Complex::new(ph.cos() as f32, ph.sin() as f32);
            }
            self.scratch[i] = y;
        }
        self.fft.process(&mut self.scratch);
        let mut best = 0;
        let mut best_pow = 0.0f32;
        let mut total = 0.0f32;
        for (k, v) in self.scratch.iter().enumerate() {
            let p = v.norm_sqr();
            total += p;
            if p > best_pow {
                best_pow = p;
                best = k;
            }
        }
        let a = self.scratch[(best + n - 1) % n].norm();
        let b = self.scratch[best].norm();
        let c = self.scratch[(best + 1) % n].norm();
        let denom = a - 2.0 * b + c;
        let delta = if denom.abs() > 1e-12 {
            (0.5 * (a - c) / denom).clamp(-0.5, 0.5)
        } else {
            0.0
        };
        Some(Peak {
            bin: best,
            pow: best_pow,
            total,
            value: self.scratch[best],
            fine: (best as f64 + delta as f64).rem_euclid(n as f64),
        })
    }

    fn signed(&self, bin: f64) -> f64 {
        let n = self.n as f64;
        let b = bin.rem_euclid(n);
        if b > n / 2.0 {
            b - n
        } else {
            b
        }
    }

    fn strong(&self, pk: &Peak) -> bool {
        // Noise alone peaks near ln(N) times the mean bin power.
        pk.pow > 6.0 * (self.n as f32).ln() * (pk.total / self.n as f32)
    }

    /// Feed 2x-rate samples; returns completed frames (CRC good or bad).
    pub fn process(&mut self, input: &[Complex<f32>]) -> Vec<LoraFrame> {
        if !input.is_empty() {
            let p = input.iter().map(|x| x.norm_sqr()).sum::<f32>() / input.len() as f32;
            let db = 10.0 * (p + 1e-20).log10();
            self.level = if self.level <= -99.0 {
                db
            } else {
                0.9 * self.level + 0.1 * db
            };
        }
        self.buf.extend_from_slice(input);
        let mut out = Vec::new();
        let sym = OVERSAMPLE * self.n;
        loop {
            match std::mem::replace(&mut self.state, State::Search) {
                State::Search => {
                    let Some(pk) = self.window(self.pos as f64, 0.0, true) else {
                        break;
                    };
                    let here = self.pos;
                    self.pos += sym;
                    if !self.strong(&pk) {
                        self.history.clear();
                        continue;
                    }
                    let near = |a: usize, b: usize, n: usize| {
                        let d = (a + n - b) % n;
                        d <= 1 || d >= n - 1
                    };
                    if let Some(last) = self.history.last() {
                        if !near(last.bin, pk.bin, self.n) {
                            self.history.clear();
                        }
                    }
                    self.history.push(pk);
                    if self.history.len() >= 3 {
                        // Next symbol boundary assuming no carrier offset.
                        let d = pk.bin % self.n;
                        let start = here + OVERSAMPLE * ((self.n - d) % self.n);
                        self.history.clear();
                        self.state = State::Align { start };
                    }
                }
                State::Align { start } => match self.align(start) {
                    Ok(Some(fs)) => self.state = State::Frame(fs),
                    Ok(None) => {
                        self.state = State::Align { start };
                        break;
                    }
                    Err(resume) => {
                        self.pos = resume;
                        self.state = State::Search;
                    }
                },
                State::Frame(mut fs) => match self.frame(&mut fs) {
                    Ok(Some((frame, end))) => {
                        out.push(frame);
                        self.pos = end;
                        self.state = State::Search;
                    }
                    Ok(None) => {
                        self.state = State::Frame(fs);
                        break;
                    }
                    Err(resume) => {
                        self.pos = resume;
                        self.state = State::Search;
                    }
                },
            }
        }
        self.trim();
        out
    }

    /// Drop consumed samples, keeping a few symbols of history.
    fn trim(&mut self) {
        let sym = OVERSAMPLE * self.n;
        let keep_from = match &self.state {
            State::Search => self.pos,
            State::Align { start } => *start,
            State::Frame(fs) => fs.data_start.max(0.0) as usize,
        }
        .saturating_sub(4 * sym);
        if keep_from > 16 * sym {
            self.buf.drain(..keep_from);
            self.pos = self.pos.saturating_sub(keep_from);
            match &mut self.state {
                State::Search => {}
                State::Align { start } => *start -= keep_from,
                State::Frame(fs) => fs.data_start -= keep_from as f64,
            }
        }
    }

    /// Walk the aligned preamble, read the sync word, find the downchirps
    /// and estimate carrier and timing offsets. `Ok(None)`: need more
    /// samples. `Err(pos)`: not a frame, resume searching at `pos`.
    fn align(&mut self, start: usize) -> Result<Option<FrameState>, usize> {
        let sym = OVERSAMPLE * self.n;
        // Pick the decimation phase closest to the chip timing: a half-chip
        // offset smears every chirp's frequency wrap.
        let (Some(a), Some(b)) = (
            self.window(start as f64, 0.0, true),
            self.window(start as f64 + 1.0, 0.0, true),
        ) else {
            return Ok(None);
        };
        let start = if b.pow > a.pow { start + 1 } else { start };
        let mut pre: Vec<Peak> = Vec::new();
        let mut m = 0usize;
        loop {
            let at = (start + m * sym) as f64;
            let Some(pk) = self.window(at, 0.0, true) else {
                return Ok(None);
            };
            let sb = self.signed(pk.bin as f64);
            if sb.abs() <= 1.0 && self.strong(&pk) {
                pre.push(pk);
                m += 1;
                if m > 80 {
                    return Err(start + m * sym);
                }
                continue;
            }
            break;
        }
        if pre.is_empty() {
            return Err(start + sym);
        }
        let s1_at = (start + m * sym) as f64;
        let (Some(s1), Some(s2), Some(down), Some(down_as_up)) = (
            self.window(s1_at, 0.0, true),
            self.window(s1_at + sym as f64, 0.0, true),
            self.window(s1_at + 2.0 * sym as f64, 0.0, false),
            self.window(s1_at + 2.0 * sym as f64, 0.0, true),
        ) else {
            return Ok(None);
        };
        let resume = start + (m + 1) * sym;
        if !self.strong(&down) || down.pow < down_as_up.pow {
            return Err(resume);
        }
        // Up-peak offset u = d + f, down-peak v = f - d (bins).
        // Up-peak u and down-peak v, refined by the phase advance between
        // consecutive identical chirps (a dechirped chirp train is one
        // continuous tone, so the advance is 2*pi*frac(peak)).
        let refine = |coarse: f64, pks: &[Peak]| -> f64 {
            let mut acc = Complex::new(0.0f64, 0.0);
            for w in pks.windows(2) {
                if w[0].bin == w[1].bin {
                    let z = w[1].value * w[0].value.conj();
                    acc += Complex::new(z.re as f64, z.im as f64);
                }
            }
            if acc.norm() == 0.0 {
                return coarse;
            }
            let frac = acc.arg() / (2.0 * PI);
            frac + (coarse - frac).round()
        };
        let u0 = pre.iter().map(|p| self.signed(p.fine)).sum::<f64>() / pre.len() as f64;
        let u = refine(u0, &pre);
        let mut downs = vec![down];
        if let Some(d2) = self.window(s1_at + 3.0 * sym as f64, 0.0, false) {
            downs.push(d2);
        }
        let v = refine(self.signed(down.fine), &downs);
        let mut cfo = (u + v) / 2.0;
        let mut d = (u - v) / 2.0; // window start minus true boundary, chips
                                   // Re-measure on the corrected grid: with timing and carrier nearly
                                   // right the chirps are clean and the residuals are accurate.
        for _ in 0..2 {
            let shift = OVERSAMPLE as f64 * d;
            let mut ups = Vec::new();
            for k in 1..pre.len() {
                if let Some(pk) = self.window((start + k * sym) as f64 - shift, cfo, true) {
                    ups.push(pk);
                }
            }
            let mut dns = Vec::new();
            for k in 2..4 {
                if let Some(pk) = self.window(s1_at + (k * sym) as f64 - shift, cfo, false) {
                    dns.push(pk);
                }
            }
            if ups.is_empty() || dns.is_empty() {
                break;
            }
            let ur = refine(
                ups.iter().map(|p| self.signed(p.fine)).sum::<f64>() / ups.len() as f64,
                &ups,
            );
            let vr = refine(
                dns.iter().map(|p| self.signed(p.fine)).sum::<f64>() / dns.len() as f64,
                &dns,
            );
            cfo += (ur + vr) / 2.0;
            d += (ur - vr) / 2.0;
        }
        let sync_val = |pk: &Peak| -> Option<u8> {
            let n = self.n as f64;
            let rel = (pk.fine - u).rem_euclid(n).round() % n;
            let nib = (rel / 8.0).round();
            if (rel - nib * 8.0).abs() > 2.0 || nib > 15.0 {
                return None;
            }
            Some(nib as u8)
        };
        let (Some(hi), Some(lo)) = (sync_val(&s1), sync_val(&s2)) else {
            return Err(resume);
        };
        let sync_word = (hi << 4) | lo;
        // Clock drift: the transmitter's crystal sets both its carrier and
        // its chip rate, so the carrier offset predicts the timing drift.
        let cfo_hz = cfo * self.p.bw_hz / self.n as f64;
        let drift = if self.p.carrier_hz > 0.0 {
            cfo_hz / self.p.carrier_hz
        } else {
            0.0
        };
        let step = sym as f64 * (1.0 - drift);
        let down_start = s1_at + 2.0 * sym as f64 - OVERSAMPLE as f64 * d;
        let data_start = down_start + 2.25 * sym as f64;
        Ok(Some(FrameState {
            data_start,
            step,
            cfo,
            sync_word,
            header: None,
            snr_acc: (0.0, 0),
        }))
    }

    fn demod_symbols(
        &mut self,
        fs: &mut FrameState,
        first: usize,
        count: usize,
    ) -> Option<Vec<u32>> {
        let mut out = Vec::with_capacity(count);
        for k in first..first + count {
            let at = fs.data_start + k as f64 * fs.step;
            let pk = self.window(at, fs.cfo, true)?;
            let noise = ((pk.total - pk.pow) / (self.n as f32 - 1.0)).max(1e-20);
            fs.snr_acc.0 += (pk.pow / (self.n as f32 * noise)) as f64;
            fs.snr_acc.1 += 1;
            out.push(pk.bin as u32);
        }
        Some(out)
    }

    fn frame(&mut self, fs: &mut FrameState) -> Result<Option<(LoraFrame, usize)>, usize> {
        let sf = self.p.sf;
        let sym = (OVERSAMPLE * self.n) as f64;
        if fs.header.is_none() {
            let Some(chirps) = self.demod_symbols(fs, 0, 8) else {
                return Ok(None);
            };
            match decode_header(&chirps, sf) {
                Some(hb) if hb.header.length > 0 => {
                    let ldro = self.p.ldro();
                    let n = payload_symbol_count(
                        hb.header.length as usize,
                        hb.header.has_crc,
                        &CodingParams {
                            sf,
                            cr: hb.header.cr,
                            ldro,
                        },
                    );
                    fs.header = Some((hb, n));
                }
                _ => return Err((fs.data_start + sym) as usize),
            }
        }
        let (hb, nsym) = fs.header.clone().unwrap();
        let Some(chirps) = self.demod_symbols(fs, 8, nsym) else {
            // Undo the SNR samples taken on this partial attempt.
            fs.snr_acc = (0.0, 0);
            return Ok(None);
        };
        let ldro = self.p.ldro();
        let pl = decode_payload(&hb, &chirps, sf, ldro);
        let snr = if fs.snr_acc.1 > 0 {
            fs.snr_acc.0 / fs.snr_acc.1 as f64
        } else {
            0.0
        };
        let end = fs.data_start + (8 + nsym) as f64 * fs.step;
        Ok(Some((
            LoraFrame {
                sf,
                bw_hz: self.p.bw_hz,
                cr: hb.header.cr,
                ldro,
                sync_word: fs.sync_word,
                payload: pl.bytes,
                crc_ok: pl.crc_ok,
                snr_db: (10.0 * snr.max(1e-12).log10()) as f32,
                cfo_hz: (fs.cfo * self.p.bw_hz / self.n as f64) as f32,
                corrected_bits: pl.corrected,
            },
            end.max(0.0) as usize,
        )))
    }
}
