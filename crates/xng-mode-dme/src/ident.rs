//! DME identification (Annex 10 §3.5.3.6): International Morse keyed at
//! least every 40 s, each key-down a burst of pulse pairs at 1350 pp/s.
//! Dots last 0.1–0.16 s and dashes 0.3–0.48 s.
//!
//! The ident pairs are exactly periodic (740.7 µs), while squitter and
//! replies are random. A pair counts as an ident "hit" when another pair sits
//! one to four periods before it (±0.8 µs plus 0.1 µs per period); random
//! pairs at 2700 pp/s land there about 1.8 % of the time. The period is learned
//! from triple coincidences (`t`, `t − Δ`, `t − 2Δ` with equal gaps) that
//! agree to ±1 µs. Hits are counted in 10 ms bins; a bin is "on" with at
//! least two hits and 3σ above the background of random coincidences, a 5-bin
//! majority vote and a glitch merge clean the keying, and the on/off run lengths
//! are read as Morse with the dot length estimated from the shortest element.
//!
//! Because periodicity, not amplitude, separates ident pairs from noise, the
//! detector is fed the weak pairs too (see [`crate::detect::Thresholds`]).

use std::collections::VecDeque;

/// Nominal ident period, µs (1350 pp/s).
pub const NOMINAL_PERIOD_US: f64 = 1e6 / 1350.0;
/// Period search window, µs (±2.5 %).
const PERIOD_MIN: f64 = 722.0;
const PERIOD_MAX: f64 = 760.0;
/// Triple-coincidence gap equality, µs.
const TRIPLE_TOL: f64 = 0.6;
/// Delay between learning the period and scoring hits, µs.
const LOOKBACK_US: f64 = 200_000.0;
/// Keying bin, µs.
const BIN_US: f64 = 10_000.0;
/// Fewest hits per bin for "on"; raised above the random-coincidence
/// background (see `IdentDetector::on_threshold`).
const HITS_ON: u32 = 2;
/// Ident hit tolerance: `HIT_TOL + HIT_TOL_PER_K · k` µs at k periods back.
const HIT_TOL: f64 = 0.8;
const HIT_TOL_PER_K: f64 = 0.1;
/// Runs shorter than this (bins) are glitches and merged into neighbours
/// (dots and element gaps are at least 100 ms = 10 bins).
const GLITCH_BINS: u32 = 5;
/// Off time that ends an ident, bins.
const END_BINS: u32 = 120;
/// Shortest on-run kept, bins.
const MIN_ON_BINS: u32 = 6;
/// Longest on-run kept, bins (a dash is ≤ 0.48 s).
const MAX_ON_BINS: u32 = 80;

/// One decoded ident.
#[derive(Debug, Clone, PartialEq)]
pub struct Ident {
    /// Decoded text, e.g. "SFO".
    pub text: String,
    /// The Morse elements, letters separated by spaces (".-- ...").
    pub morse: String,
    /// Estimated dot length, ms.
    pub dot_ms: f32,
    /// Measured ident pulse-pair rate, Hz.
    pub rate_hz: f64,
    /// Stream time of the first key-down, µs.
    pub start_us: f64,
    /// Stream time of the end of the last key-down, µs.
    pub end_us: f64,
}

/// Streaming ident detector for one pulse spacing.
pub struct IdentDetector {
    /// Pairs of the last two periods (period learning).
    recent: VecDeque<f64>,
    /// Pairs waiting to be scored.
    delayed: VecDeque<f64>,
    /// Scored pairs of the last four periods.
    hist: VecDeque<f64>,
    period: Option<f64>,
    /// Recent triple-coincidence periods: (time, Δ).
    triples: VecDeque<(f64, f64)>,
    bin: i64,
    bin_hits: u32,
    /// Raw on/off of the last five bins (majority vote).
    raw: VecDeque<bool>,
    state: bool,
    run: u32,
    /// Runs of the current ident: (on?, length in bins).
    runs: Vec<(bool, u32)>,
    session_start_bin: Option<i64>,
    /// Background (random coincidence) hits per off bin, smoothed.
    background: f64,
    /// Total ident hits (diagnostics).
    pub hits: u64,
}

impl Default for IdentDetector {
    fn default() -> Self {
        Self::new()
    }
}

impl IdentDetector {
    pub fn new() -> Self {
        Self {
            recent: VecDeque::with_capacity(64),
            delayed: VecDeque::new(),
            hist: VecDeque::with_capacity(64),
            period: None,
            triples: VecDeque::new(),
            bin: 0,
            bin_hits: 0,
            raw: VecDeque::from(vec![false; 5]),
            state: false,
            run: 0,
            runs: Vec::new(),
            session_start_bin: None,
            background: 0.0,
            hits: 0,
        }
    }

    /// Learned ident period, µs.
    pub fn period_us(&self) -> Option<f64> {
        self.period
    }

    /// Random coincidences counted as ident hits, per second.
    pub fn background_hits_per_s(&self) -> f64 {
        self.background * 1e6 / BIN_US
    }

    /// Whether the keying is currently "on" (ident being sent).
    pub fn keyed(&self) -> bool {
        self.state
    }

    /// Feed one pair time (µs, non-decreasing); returns a decoded ident when
    /// one completes.
    ///
    /// The period is learned from pairs as they arrive, but hits are scored
    /// [`LOOKBACK_US`] later, so the first Morse element is not lost while
    /// the period locks.
    pub fn push(&mut self, t: f64) -> Option<Ident> {
        while self
            .recent
            .front()
            .is_some_and(|&p| p < t - 2.0 * PERIOD_MAX - 10.0)
        {
            self.recent.pop_front();
        }
        // Triple coincidence: t, p = t − Δ, q = p − Δ.
        for (i, &p) in self.recent.iter().enumerate().rev() {
            let d = t - p;
            if d < PERIOD_MIN {
                continue;
            }
            if d > PERIOD_MAX {
                break;
            }
            let target = p - d;
            if self
                .recent
                .iter()
                .take(i)
                .any(|&q| (q - target).abs() <= TRIPLE_TOL)
            {
                self.triples.push_back((t, d));
            }
        }
        while self.triples.front().is_some_and(|&(tt, _)| tt < t - 3e6) {
            self.triples.pop_front();
        }
        self.update_period();
        self.recent.push_back(t);
        self.delayed.push_back(t);
        self.drain(t - LOOKBACK_US)
    }

    /// Score delayed pairs up to time `until`.
    fn drain(&mut self, until: f64) -> Option<Ident> {
        let mut done = None;
        while let Some(&t) = self.delayed.front() {
            if t > until {
                break;
            }
            self.delayed.pop_front();
            if let Some(id) = self.advance_bins(t) {
                done = Some(id);
            }
            while self
                .hist
                .front()
                .is_some_and(|&p| p < t - 4.0 * PERIOD_MAX - 10.0)
            {
                self.hist.pop_front();
            }
            if let Some(per) = self.period {
                let mut hit = None;
                'k: for k in 1..=4 {
                    let target = t - k as f64 * per;
                    let tol = HIT_TOL + HIT_TOL_PER_K * k as f64;
                    for &p in self.hist.iter().rev() {
                        if p < target - tol {
                            break;
                        }
                        if (p - target).abs() <= tol {
                            hit = Some((t - p) / k as f64);
                            break 'k;
                        }
                    }
                }
                if let Some(obs) = hit {
                    self.bin_hits += 1;
                    self.hits += 1;
                    // Track slow drift of the transponder's ident clock.
                    if (obs - per).abs() < 0.5 {
                        self.period = Some(per + 0.02 * (obs - per));
                    }
                }
            }
            self.hist.push_back(t);
        }
        done
    }

    /// Advance the keying clock to time `t` (also call on quiet channels to
    /// let a finished ident complete).
    pub fn advance_to(&mut self, t: f64) -> Option<Ident> {
        let a = self.drain(t - LOOKBACK_US);
        let b = self.advance_bins(t - LOOKBACK_US);
        b.or(a)
    }

    fn advance_bins(&mut self, t: f64) -> Option<Ident> {
        let b = (t / BIN_US).floor() as i64;
        let mut out = None;
        while self.bin < b {
            let on = self.bin_hits >= self.on_threshold();
            if !self.state && !on {
                self.background += 0.01 * (self.bin_hits as f64 - self.background);
            }
            self.bin_hits = 0;
            if let Some(id) = self.push_bin(on) {
                out = Some(id);
            }
            self.bin += 1;
            // Skip long silences quickly.
            if !self.state && self.session_start_bin.is_none() && b - self.bin > 10 {
                self.raw.iter_mut().for_each(|r| *r = false);
                self.bin = b;
            }
        }
        out
    }

    /// Hits a bin needs to count as keyed: about 3σ above the random
    /// coincidences seen while the ident is off (strong busy channels see a
    /// few per bin; weak ones almost none).
    fn on_threshold(&self) -> u32 {
        let l = self.background;
        HITS_ON.max((l + 3.0 * l.sqrt() + 1.0).ceil() as u32)
    }

    fn update_period(&mut self) {
        if self.triples.len() < 6 {
            return;
        }
        // Densest ±1 µs cluster of triple periods.
        let mut ds: Vec<f64> = self.triples.iter().map(|&(_, d)| d).collect();
        ds.sort_by(|a, b| a.total_cmp(b));
        let mut best = (0usize, 0usize);
        let mut j = 0;
        for i in 0..ds.len() {
            while ds[i] - ds[j] > 2.0 {
                j += 1;
            }
            if i + 1 - j > best.1 - best.0 {
                best = (j, i + 1);
            }
        }
        let n = best.1 - best.0;
        if n < 6 {
            return;
        }
        let mean = ds[best.0..best.1].iter().sum::<f64>() / n as f64;
        match self.period {
            Some(p) if (p - mean).abs() < 1.0 => {}
            // A new cluster must clearly beat the old period to replace it.
            Some(p) => {
                let old = ds.iter().filter(|&&d| (d - p).abs() <= 1.0).count();
                if n > 2 * old + 4 {
                    self.period = Some(mean);
                }
            }
            None => self.period = Some(mean),
        }
    }

    fn push_bin(&mut self, on: bool) -> Option<Ident> {
        self.raw.pop_front();
        self.raw.push_back(on);
        let votes = self.raw.iter().filter(|&&r| r).count();
        let smoothed = votes >= 3;
        let bin_now = self.bin - 2; // the vote is centred two bins back
        if smoothed == self.state {
            self.run += 1;
        } else {
            if self.session_start_bin.is_some() || self.state {
                self.runs.push((self.state, self.run));
            }
            if smoothed && self.session_start_bin.is_none() {
                self.session_start_bin = Some(bin_now);
                self.runs.clear();
            }
            self.state = smoothed;
            self.run = 1;
        }
        if !self.state && self.session_start_bin.is_some() && self.run >= END_BINS {
            let start = self.session_start_bin.take().unwrap();
            let runs = std::mem::take(&mut self.runs);
            return decode_runs(&runs, start, self.period.unwrap_or(NOMINAL_PERIOD_US));
        }
        None
    }
}

/// Turn on/off runs (bins) into an ident.
fn decode_runs(runs: &[(bool, u32)], start_bin: i64, period: f64) -> Option<Ident> {
    // Trim leading off-runs, then fold glitches (runs under GLITCH_BINS)
    // into the neighbouring runs.
    let mut merged: Vec<(bool, u32)> = Vec::new();
    for &(on, len) in runs.iter() {
        if merged.is_empty() && (!on || len < GLITCH_BINS) {
            continue; // leading silence or glitch
        }
        match merged.last_mut() {
            Some(last) if last.0 == on => last.1 += len,
            Some(last) if len < GLITCH_BINS => last.1 += len,
            _ => merged.push((on, len)),
        }
    }
    while merged.last().is_some_and(|r| !r.0) {
        merged.pop();
    }
    let runs = merged;
    let ons: Vec<u32> = runs.iter().filter(|r| r.0).map(|r| r.1).collect();
    if ons.is_empty()
        || ons
            .iter()
            .any(|&l| !(MIN_ON_BINS..=MAX_ON_BINS).contains(&l))
    {
        return None;
    }
    // Dot length: mean of the elements within 1.6× the shortest.
    let shortest = *ons.iter().min().unwrap() as f32;
    let dots: Vec<f32> = ons
        .iter()
        .map(|&l| l as f32)
        .filter(|&l| l <= 1.6 * shortest)
        .collect();
    let mut dot = dots.iter().sum::<f32>() / dots.len() as f32;
    // All dashes (e.g. "MOM") would look like dots: a 0.3 s+ "dot" is a dash.
    if dot >= 26.0 {
        dot /= 3.0;
    }
    let split = 2.0 * dot;
    let mut morse = String::new();
    let mut text = String::new();
    let mut letter = String::new();
    let total: u32 = runs.iter().map(|r| r.1).sum();
    for (on, len) in runs {
        let len = len as f32;
        if on {
            letter.push(if len < split { '.' } else { '-' });
        } else if len >= split {
            text.push(morse_char(&letter)?);
            morse.push_str(&letter);
            morse.push(' ');
            letter.clear();
        }
    }
    if !letter.is_empty() {
        text.push(morse_char(&letter)?);
        morse.push_str(&letter);
    }
    if !(2..=4).contains(&text.len()) {
        return None;
    }
    let start_us = start_bin as f64 * BIN_US;
    Some(Ident {
        text,
        morse: morse.trim_end().to_string(),
        dot_ms: dot * (BIN_US / 1000.0) as f32,
        rate_hz: 1e6 / period,
        start_us,
        end_us: start_us + total as f64 * BIN_US,
    })
}

/// International Morse (ITU-R M.1677) letters and digits.
pub const MORSE: [(char, &str); 36] = [
    ('A', ".-"),
    ('B', "-..."),
    ('C', "-.-."),
    ('D', "-.."),
    ('E', "."),
    ('F', "..-."),
    ('G', "--."),
    ('H', "...."),
    ('I', ".."),
    ('J', ".---"),
    ('K', "-.-"),
    ('L', ".-.."),
    ('M', "--"),
    ('N', "-."),
    ('O', "---"),
    ('P', ".--."),
    ('Q', "--.-"),
    ('R', ".-."),
    ('S', "..."),
    ('T', "-"),
    ('U', "..-"),
    ('V', "...-"),
    ('W', ".--"),
    ('X', "-..-"),
    ('Y', "-.--"),
    ('Z', "--.."),
    ('0', "-----"),
    ('1', ".----"),
    ('2', "..---"),
    ('3', "...--"),
    ('4', "....-"),
    ('5', "....."),
    ('6', "-...."),
    ('7', "--..."),
    ('8', "---.."),
    ('9', "----."),
];

/// Character for a Morse element string.
pub fn morse_char(code: &str) -> Option<char> {
    MORSE.iter().find(|(_, m)| *m == code).map(|(c, _)| *c)
}

/// Morse elements for a character.
pub fn morse_code(c: char) -> Option<&'static str> {
    let c = c.to_ascii_uppercase();
    MORSE.iter().find(|(k, _)| *k == c).map(|(_, m)| *m)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn morse_roundtrip() {
        for (c, m) in MORSE {
            assert_eq!(morse_char(m), Some(c));
            assert_eq!(morse_code(c), Some(m));
        }
    }

    #[test]
    fn decodes_clean_periodic_keying() {
        // "SFO" with 0.125 s dots, ideal pairs, no noise.
        let mut det = IdentDetector::new();
        let per = NOMINAL_PERIOD_US;
        let dot = 125_000.0;
        let mut t = 1_000_000.0;
        let mut out = None;
        for (li, c) in "SFO".chars().enumerate() {
            if li > 0 {
                t += 3.0 * dot;
            }
            for (ei, e) in morse_code(c).unwrap().chars().enumerate() {
                if ei > 0 {
                    t += dot;
                }
                let len = if e == '.' { dot } else { 3.0 * dot };
                let end = t + len;
                while t < end {
                    if let Some(id) = det.push(t) {
                        out = Some(id);
                    }
                    t += per;
                }
            }
        }
        if let Some(id) = det.advance_to(t + 3e6) {
            out = Some(id);
        }
        let id = out.expect("ident");
        assert_eq!(id.text, "SFO");
        assert!((id.rate_hz - 1350.0).abs() < 1.0);
    }
}
