//! Morse ident decoding for navaids (ICAO Annex 10 Vol I 3.3.6, 3.1.3.9:
//! International Morse on a 1020 Hz tone at about 7 words per minute).
//!
//! The input is the ident tone's power envelope at [`ENV_RATE`] (from a
//! narrow filter around the measured tone frequency). Decoding is:
//!
//! 1. an adaptive key threshold halfway (in dB) between the noise floor and
//!    the keyed tone level, both tracked from the envelope's distribution,
//!    with hysteresis;
//! 2. glitch removal, then a dot length estimated from the mark durations
//!    (with the 7 wpm standard as the prior);
//! 3. symbol → letter decoding, words split at long gaps;
//! 4. a vote across repetitions, since navaids repeat the ident every few
//!    seconds. A single badly received repetition never sets the ident.
//! 5. **Epoch folding** for weak signals: the repetition period is found by
//!    autocorrelating the envelope, and the repetitions are averaged before
//!    decoding, so K repetitions give about 10·log10(K) dB more SNR than any
//!    one of them. Navaid keyers repeat on a fixed period, which is what
//!    makes this work.

use rustfft::{num_complex::Complex, FftPlanner};
use std::collections::{HashMap, VecDeque};

/// Envelope sample rate (Hz).
pub const ENV_RATE: f64 = 200.0;
/// Standard ident speed: 7 wpm → 1.2/7 s dot.
pub const STANDARD_DOT_S: f64 = 1.2 / 7.0;
/// History used for the threshold statistics (s).
const STATS_S: f64 = 24.0;
/// Shortest mark/space kept (s); shorter runs are glitches.
const GLITCH_S: f64 = 0.035;
/// Shortest mark or space kept in a folded cycle (0.4 of a 7 wpm dot).
const FOLD_GLITCH_S: f64 = 0.4 * STANDARD_DOT_S;
/// A gap at least this long ends a word (one ident repetition).
const WORD_GAP_S: f64 = 1.2;
/// Votes are kept for this long (s).
const VOTE_WINDOW_S: f64 = 180.0;
/// Runs in the longest ident word (TEST, or I and three letters of up to
/// four symbols each, is under 32). A word running longer is noise or
/// voice: it is dropped up to the next word gap.
const MAX_WORD_RUNS: usize = 40;
/// Envelope history for folding (s) and the ident periods searched (s).
const FOLD_HIST_S: f64 = 120.0;
const PERIOD_MIN_S: f64 = 4.0;
const PERIOD_MAX_S: f64 = 40.0;
/// What an ident of this kind of station looks like (ICAO Annex 10: VOR
/// idents are two or three letters; ILS localizer idents are two or three
/// letters, which may be preceded by an I). Anything else is a decoding
/// error, so it is never reported.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum IdentRule {
    /// Two to four letters or digits.
    #[default]
    Any,
    Vor,
    Localizer,
}

impl IdentRule {
    pub fn plausible(self, word: &str) -> bool {
        let letters = word.chars().all(|c| c.is_ascii_uppercase());
        // A station under maintenance keys TST (or TEST) instead.
        if self != IdentRule::Any && (word == "TST" || word == "TEST") {
            return true;
        }
        match self {
            IdentRule::Any => {
                (2..=4).contains(&word.len()) && word.chars().all(|c| c.is_ascii_alphanumeric())
            }
            IdentRule::Vor => letters && (2..=3).contains(&word.len()),
            IdentRule::Localizer => {
                letters
                    && ((2..=3).contains(&word.len()) || (word.starts_with('I') && word.len() == 4))
            }
        }
    }
}

const MORSE: [(&str, char); 36] = [
    (".-", 'A'),
    ("-...", 'B'),
    ("-.-.", 'C'),
    ("-..", 'D'),
    (".", 'E'),
    ("..-.", 'F'),
    ("--.", 'G'),
    ("....", 'H'),
    ("..", 'I'),
    (".---", 'J'),
    ("-.-", 'K'),
    (".-..", 'L'),
    ("--", 'M'),
    ("-.", 'N'),
    ("---", 'O'),
    (".--.", 'P'),
    ("--.-", 'Q'),
    (".-.", 'R'),
    ("...", 'S'),
    ("-", 'T'),
    ("..-", 'U'),
    ("...-", 'V'),
    (".--", 'W'),
    ("-..-", 'X'),
    ("-.--", 'Y'),
    ("--..", 'Z'),
    ("-----", '0'),
    (".----", '1'),
    ("..---", '2'),
    ("...--", '3'),
    ("....-", '4'),
    (".....", '5'),
    ("-....", '6'),
    ("--...", '7'),
    ("---..", '8'),
    ("----.", '9'),
];

/// The Morse code for a letter or digit.
pub fn code_for(c: char) -> Option<&'static str> {
    let c = c.to_ascii_uppercase();
    MORSE.iter().find(|(_, ch)| *ch == c).map(|(code, _)| *code)
}

fn char_for(code: &str) -> Option<char> {
    MORSE.iter().find(|(c, _)| *c == code).map(|(_, ch)| *ch)
}

/// Streaming Morse ident decoder.
pub struct IdentDecoder {
    /// Recent envelope (dB) for the threshold statistics.
    hist: VecDeque<f32>,
    keyed: bool,
    run: usize,
    /// Completed (is_mark, length in samples) runs of the current word.
    runs: Vec<(bool, usize)>,
    /// Decoded words with the sample time they ended.
    votes: VecDeque<(u64, String)>,
    t: u64,
    noise_db: f32,
    tone_db: f32,
    /// Smoothed estimate of the tone-to-noise ratio of the keyed tone (dB).
    pub tone_snr_db: f32,
    /// The latest word decoded, voted or not.
    pub last_word: Option<String>,
    /// When `last_word` was decoded (envelope samples).
    last_word_t: u64,
    /// The current word ran past [`MAX_WORD_RUNS`]; ignore it to the gap.
    overlong: bool,
    /// Linear power history for folding.
    lin: VecDeque<f32>,
    /// Latest folded decode: (word, repetitions folded).
    folded: Option<(String, u32)>,
    /// When a fold last decoded `folded` (envelope samples); the fold is
    /// dropped once this is older than the vote window.
    folded_t: u64,
    /// The latest confident fold and how many folds in a row agreed on it.
    fold_candidate: Option<(String, u32)>,
    rule: IdentRule,
}

impl Default for IdentDecoder {
    fn default() -> Self {
        Self::new()
    }
}

impl IdentDecoder {
    pub fn new() -> Self {
        Self::with_rule(IdentRule::Any)
    }

    pub fn with_rule(rule: IdentRule) -> Self {
        Self {
            rule,
            hist: VecDeque::new(),
            keyed: false,
            run: 0,
            runs: Vec::new(),
            votes: VecDeque::new(),
            t: 0,
            noise_db: f32::NAN,
            tone_db: f32::NAN,
            tone_snr_db: 0.0,
            last_word: None,
            last_word_t: 0,
            overlong: false,
            lin: VecDeque::new(),
            folded: None,
            folded_t: 0,
            fold_candidate: None,
        }
    }

    fn refresh_levels(&mut self) {
        let mut v: Vec<f32> = self.hist.iter().copied().collect();
        if v.len() < (2.0 * ENV_RATE) as usize {
            return;
        }
        v.sort_by(|a, b| a.total_cmp(b));
        // Idents are keyed well under half the time, so the 25th percentile
        // is noise and the top few percent are the tone.
        let noise = v[v.len() / 4];
        let tone = v[(v.len() as f64 * 0.97) as usize];
        self.noise_db = noise;
        self.tone_db = tone;
        self.tone_snr_db = tone - noise;
    }

    /// Feed envelope power samples (linear, any scale).
    pub fn push(&mut self, power: &[f32]) {
        let max_hist = (STATS_S * ENV_RATE) as usize;
        let refresh_every = (0.5 * ENV_RATE) as u64;
        let max_lin = (FOLD_HIST_S * ENV_RATE) as usize;
        let fold_every = (5.0 * ENV_RATE) as u64;
        for &p in power {
            self.lin.push_back(p);
            if self.lin.len() > max_lin {
                self.lin.pop_front();
            }
            if self.t.is_multiple_of(fold_every)
                && self.lin.len() as f64 >= 2.2 * PERIOD_MIN_S * ENV_RATE
            {
                self.fold();
            }
            let db = 10.0 * p.max(1e-30).log10();
            self.hist.push_back(db);
            if self.hist.len() > max_hist {
                self.hist.pop_front();
            }
            self.t += 1;
            if self.t.is_multiple_of(refresh_every) || self.noise_db.is_nan() {
                self.refresh_levels();
                self.expire_votes();
            }
            let usable = self.tone_snr_db >= 4.0 && !self.noise_db.is_nan();
            let mid = 0.5 * (self.noise_db + self.tone_db);
            let hyst = 0.12 * (self.tone_db - self.noise_db);
            let now = if !usable {
                false
            } else if self.keyed {
                db > mid - hyst
            } else {
                db > mid + hyst
            };
            if now == self.keyed {
                self.run += 1;
            } else {
                self.end_run();
                self.keyed = now;
                self.run = 1;
            }
            // A long silence ends the word even before the next mark.
            if !self.keyed && self.run == (WORD_GAP_S * ENV_RATE) as usize {
                self.finish_word();
            }
        }
    }

    fn end_run(&mut self) {
        let glitch = (GLITCH_S * ENV_RATE) as usize;
        if self.run == 0 {
            return;
        }
        if self.run < glitch {
            // Merge a glitch into the previous run (it flips back).
            if let Some(last) = self.runs.last_mut() {
                last.1 += self.run;
            }
            return;
        }
        if self.overlong {
            return;
        }
        match self.runs.last_mut() {
            Some(last) if last.0 == self.keyed => last.1 += self.run,
            _ => {
                if self.runs.is_empty() && !self.keyed {
                    return; // leading silence
                }
                self.runs.push((self.keyed, self.run));
                if self.runs.len() > MAX_WORD_RUNS {
                    self.runs.clear();
                    self.overlong = true;
                }
            }
        }
    }

    fn finish_word(&mut self) {
        let runs = std::mem::take(&mut self.runs);
        if std::mem::take(&mut self.overlong) {
            return;
        }
        if let Some(w) = decode_runs(&runs).filter(|w| self.rule.plausible(w)) {
            self.last_word = Some(w.clone());
            self.last_word_t = self.t;
            self.votes.push_back((self.t, w));
        }
        self.expire_votes();
    }

    /// Drop votes older than the window, by the running sample clock (so
    /// they expire even when the station stops keying).
    fn expire_votes(&mut self) {
        let keep = (VOTE_WINDOW_S * ENV_RATE) as u64;
        while self
            .votes
            .front()
            .is_some_and(|(t, _)| self.t.saturating_sub(*t) > keep)
        {
            self.votes.pop_front();
        }
    }

    /// The ident: the folded word once two folds agree on it, else the word
    /// decoded most often in the vote window, with its count.
    pub fn ident(&self) -> Option<(String, u32)> {
        let keep = (VOTE_WINDOW_S * ENV_RATE) as u64;
        if let Some(f) = &self.folded {
            // Brief decoding failures keep it; a fold nothing has confirmed
            // for the vote window gives way to the votes.
            if self.t.saturating_sub(self.folded_t) <= keep {
                return Some(f.clone());
            }
        }
        let mut counts: HashMap<&str, u32> = HashMap::new();
        for (_, w) in &self.votes {
            *counts.entry(w.as_str()).or_default() += 1;
        }
        let mut best: Option<(&str, u32)> = None;
        let mut second = 0;
        for (w, c) in counts {
            match best {
                Some((_, bc)) if c <= bc => second = second.max(c),
                _ => {
                    second = best.map_or(0, |b| b.1);
                    best = Some((w, c));
                }
            }
        }
        // Without a confident fold, a word must repeat three times, be at
        // least half of everything decoded, and beat every other word: noise
        // makes short words (E, T, EE) too often for two matches to mean
        // anything, and a tie says nothing about which is right.
        let total = self.votes.len() as u32;
        best.filter(|(_, c)| *c >= 3 && 2 * *c >= total && *c > second)
            .map(|(w, c)| (w.to_string(), c))
    }

    /// The latest word decoded within the vote window, voted or not (what
    /// the station is keying now; [`Self::last_word`] keeps the history).
    pub fn recent_word(&self) -> Option<&str> {
        let keep = (VOTE_WINDOW_S * ENV_RATE) as u64;
        self.last_word
            .as_deref()
            .filter(|_| self.t.saturating_sub(self.last_word_t) <= keep)
    }

    /// Find the repetition period, average the repetitions and decode.
    fn fold(&mut self) {
        let x: Vec<f32> = self.lin.iter().copied().collect();
        let n = x.len();
        let mean = x.iter().sum::<f32>() / n as f32;
        let nfft = (2 * n).next_power_of_two();
        let mut b: Vec<Complex<f64>> = x
            .iter()
            .map(|&v| Complex::new((v - mean) as f64, 0.0))
            .collect();
        b.resize(nfft, Complex::new(0.0, 0.0));
        let mut planner = FftPlanner::new();
        planner.plan_fft_forward(nfft).process(&mut b);
        for c in b.iter_mut() {
            *c = Complex::new(c.norm_sqr(), 0.0);
        }
        planner.plan_fft_inverse(nfft).process(&mut b);
        let r0 = b[0].re.max(1e-300);
        let lo = (PERIOD_MIN_S * ENV_RATE) as usize;
        let hi = ((PERIOD_MAX_S * ENV_RATE) as usize).min(n / 2);
        if hi <= lo {
            return;
        }
        // Unbiased autocorrelation (divide by overlap) so long lags compete.
        let ac: Vec<f64> = (lo..=hi)
            .map(|l| b[l].re / r0 * n as f64 / (n - l) as f64)
            .collect();
        let max = ac.iter().cloned().fold(f64::MIN, f64::max);
        if max < 0.15 {
            self.folded = None;
            self.fold_candidate = None;
            return;
        }
        // The shortest lag near the maximum is the period (not a multiple).
        let Some(i) = ac.iter().position(|&v| v >= 0.8 * max) else {
            self.fold_candidate = None;
            return;
        };
        // Peak of that lobe.
        let mut j = i;
        while j + 1 < ac.len() && ac[j + 1] >= ac[j] {
            j += 1;
        }
        let period = lo + j;
        let k = n / period;
        // Three repetitions at least: with two, the spread between them
        // says too little about the noise to trust a weak fold.
        if k < 3 {
            self.fold_candidate = None;
            return;
        }
        let start = n - k * period;
        let reps: Vec<&[f32]> = (0..k)
            .map(|r| &x[start + r * period..start + (r + 1) * period])
            .collect();
        // A word is reported once two folds in a row (5 s apart, each with
        // more data) agree on it; it stays until another word does the same.
        // A failed fold breaks the run: the two must be adjacent.
        let Some(w) = decode_folded(&reps, self.rule) else {
            self.fold_candidate = None;
            return;
        };
        match &mut self.fold_candidate {
            Some((c, n)) if *c == w => *n += 1,
            _ => self.fold_candidate = Some((w.clone(), 1)),
        }
        if self.fold_candidate.as_ref().is_some_and(|(_, n)| *n >= 2) {
            self.folded = Some((w, k as u32));
            self.folded_t = self.t;
        } else if self.folded.as_ref().is_some_and(|(f, _)| *f == w) {
            // The published word heard again: still fresh.
            self.folded_t = self.t;
        }
    }

    /// The keyed tone's level (dB, input scale) once a tone stands clear of
    /// the noise.
    pub fn tone_level_db(&self) -> Option<f32> {
        (self.tone_snr_db >= 4.0 && self.tone_db.is_finite()).then_some(self.tone_db)
    }

    /// Number of words heard in the vote window.
    pub fn words_heard(&self) -> usize {
        self.votes.len()
    }
}

/// Envelope correlation length (samples at [`ENV_RATE`]) after the ±5 Hz
/// keying filter: neighboring samples are not independent noise.
const ENV_CORR: f64 = 20.0;
/// Every folded mark and space must sit this many standard errors on its
/// side of the threshold.
const FOLD_MIN_Z: f64 = 1.0;
/// The same for a space inside a letter.
const FOLD_MIN_Z_GAP: f64 = 2.0;
/// A folded space may sit at most this far up from the noise level toward
/// the tone level (0 = noise, 1 = tone).
const FOLD_MAX_SPACE_POS: f64 = 0.35;

/// Fold repetitions of one ident cycle (linear power) and decode them.
///
/// Returns a word only when every mark and space of the folded cycle stands
/// clear of the threshold by [`FOLD_MIN_Z`] standard errors (estimated from
/// the spread between repetitions). A noise pattern that happens to fold into
/// letters fails this, so a weak ident reports nothing rather than a wrong
/// word.
fn decode_folded(reps: &[&[f32]], rule: IdentRule) -> Option<String> {
    let period = reps.first()?.len();
    let k = reps.len() as f32;
    let t: Vec<f32> = (0..period)
        .map(|i| reps.iter().map(|r| r[i]).sum::<f32>() / k)
        .collect();
    let mut sorted = t.clone();
    sorted.sort_by(|a, b| a.total_cmp(b));
    let lo_v = sorted[sorted.len() / 4];
    let hi_v = sorted[(sorted.len() as f64 * 0.97) as usize];
    if 10.0 * (hi_v / lo_v.max(1e-30)).log10() < 3.0 {
        return None;
    }
    let thr = (lo_v * hi_v).sqrt();
    let on: Vec<bool> = t.iter().map(|&v| v > thr).collect();
    // Rotate so the cycle starts in its longest quiet stretch.
    let (mut best_len, mut best_end, mut cur) = (0, 0, 0);
    for i in 0..2 * period {
        if !on[i % period] {
            cur += 1;
            if cur > best_len {
                best_len = cur;
                best_end = i % period;
            }
        } else {
            cur = 0;
        }
    }
    let first = best_end + 1;
    // Runs as (is_mark, start offset, length).
    let mut runs: Vec<(bool, usize, usize)> = Vec::new();
    for i in 0..period {
        let v = on[(first + i) % period];
        match runs.last_mut() {
            Some(r) if r.0 == v => r.2 += 1,
            _ => runs.push((v, i, 1)),
        }
    }
    // Folding averages the noise down, so anything much shorter than a 7 wpm
    // dot is left over noise, not keying (idents are sent at about 7 wpm).
    let glitch = (FOLD_GLITCH_S * ENV_RATE) as usize;
    runs.retain(|r| r.2 >= glitch);
    let mut merged: Vec<(bool, usize, usize)> = Vec::new();
    for r in runs {
        match merged.last_mut() {
            Some(m) if m.0 == r.0 => m.2 = r.1 + r.2 - m.1,
            _ => merged.push(r),
        }
    }
    while merged.first().is_some_and(|r| !r.0) {
        merged.remove(0);
    }
    while merged.last().is_some_and(|r| !r.0) {
        merged.pop();
    }
    let runs: Vec<(bool, usize)> = merged.iter().map(|r| (r.0, r.2)).collect();
    let word = decode_runs(&runs).filter(|w| rule.plausible(w))?;
    if reps.len() >= 2 {
        // Each run's middle (the filtered envelope ramps over its edges):
        // mean level and standard error from the spread between repetitions.
        let stats: Vec<(bool, f64, f64, usize)> = merged
            .iter()
            .map(|&(mark, s0, len)| {
                let trim = len / 4;
                let (a, b) = (s0 + trim, s0 + len - trim);
                let vals: Vec<f64> = reps
                    .iter()
                    .flat_map(|r| (a..b).map(move |i| r[(first + i) % period] as f64))
                    .collect();
                let m = vals.iter().sum::<f64>() / vals.len() as f64;
                let var = vals.iter().map(|v| (v - m) * (v - m)).sum::<f64>()
                    / (vals.len() - 1).max(1) as f64;
                let n_eff = reps.len() as f64 * ((b - a) as f64 / ENV_CORR).max(1.0);
                (mark, m, (var / n_eff).sqrt().max(1e-30), len)
            })
            .collect();
        let mean_of = |want: bool| {
            let v: Vec<f64> = stats.iter().filter(|s| s.0 == want).map(|s| s.1).collect();
            v.iter().sum::<f64>() / v.len().max(1) as f64
        };
        // Noise level from the quiet stretch between repetitions.
        let quiet: f64 = {
            let n = best_len.max(1);
            let start = best_end + 1 + period - n;
            reps.iter()
                .flat_map(|r| (0..n).map(move |i| r[(start + i) % period] as f64))
                .sum::<f64>()
                / (n * reps.len()) as f64
        };
        let tone = mean_of(true);
        let span = (tone - quiet).max(1e-30);
        let marks_s: Vec<f64> = runs
            .iter()
            .filter(|r| r.0)
            .map(|r| r.1 as f64 / ENV_RATE)
            .collect();
        let dot = estimate_dot(&marks_s) * ENV_RATE;
        for &(mark, m, se, len) in &stats {
            let z = if mark {
                (m - thr as f64) / se
            } else {
                (thr as f64 - m) / se
            };
            // Where the run sits between the noise and tone levels: a space
            // inside a letter that is only a dip in a long mark sits high.
            let pos = (m - quiet) / span;
            let clear = if mark {
                pos > 0.5
            } else {
                pos < FOLD_MAX_SPACE_POS
            };
            // A gap inside a letter is where noise splits a dash into dots
            // (A read as S): it must stand clear by more.
            let need = if !mark && (len as f64) < 2.0 * dot {
                FOLD_MIN_Z_GAP
            } else {
                FOLD_MIN_Z
            };
            if z < need || !clear {
                return None;
            }
        }
    }
    Some(word)
}

/// Decode one word from alternating mark/space runs (in envelope samples).
pub fn decode_runs(runs: &[(bool, usize)]) -> Option<String> {
    let marks: Vec<f64> = runs
        .iter()
        .filter(|r| r.0)
        .map(|r| r.1 as f64 / ENV_RATE)
        .collect();
    if marks.is_empty() {
        return None;
    }
    let dot = estimate_dot(&marks);
    let mut word = String::new();
    let mut sym = String::new();
    for &(mark, len) in runs {
        let units = len as f64 / ENV_RATE / dot;
        if mark {
            sym.push(if units < 2.0 { '.' } else { '-' });
        } else if units >= 2.0 {
            word.push(char_for(&sym)?);
            sym.clear();
        }
    }
    if !sym.is_empty() {
        word.push(char_for(&sym)?);
    }
    (!word.is_empty()).then_some(word)
}

/// Dot length (s): split the marks into short and long by the largest ratio
/// gap; with only one length present, use the standard speed to tell whether
/// they are dots or dashes.
fn estimate_dot(marks: &[f64]) -> f64 {
    let mut m = marks.to_vec();
    m.sort_by(|a, b| a.total_cmp(b));
    let mut split = None;
    let mut best = 1.6;
    for i in 1..m.len() {
        let r = m[i] / m[i - 1];
        if r > best {
            best = r;
            split = Some(i);
        }
    }
    let median = |v: &[f64]| v[v.len() / 2];
    match split {
        Some(i) => {
            let short = median(&m[..i]);
            let long = median(&m[i..]);
            // Weighted: dashes are three dots.
            (short * i as f64 + long / 3.0 * (m.len() - i) as f64) / m.len() as f64
        }
        None => {
            let med = median(&m);
            if med > 2.0 * STANDARD_DOT_S {
                med / 3.0
            } else {
                med
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(word: &str, dot_s: f64, pre_s: f64) -> Vec<(bool, f64)> {
        let mut v = vec![(false, pre_s)];
        for (i, c) in word.chars().enumerate() {
            if i > 0 {
                v.push((false, 3.0 * dot_s));
            }
            for (j, s) in code_for(c).unwrap().chars().enumerate() {
                if j > 0 {
                    v.push((false, dot_s));
                }
                v.push((true, if s == '.' { dot_s } else { 3.0 * dot_s }));
            }
        }
        v
    }

    #[test]
    fn decodes_repeated_ident() {
        let mut d = IdentDecoder::new();
        let mut env = Vec::new();
        for _ in 0..4 {
            for (on, s) in key("SAC", STANDARD_DOT_S, 4.0) {
                let n = (s * ENV_RATE) as usize;
                env.extend(std::iter::repeat_n(if on { 1.0 } else { 0.01 }, n));
            }
        }
        env.extend(std::iter::repeat_n(0.01f32, (3.0 * ENV_RATE) as usize));
        d.push(&env);
        assert_eq!(d.ident().map(|x| x.0).as_deref(), Some("SAC"));
    }

    #[test]
    fn decodes_runs_at_other_speeds() {
        for dot in [0.08, 0.12, 0.17, 0.25] {
            let runs: Vec<(bool, usize)> = key("IABC", dot, 0.0)
                .into_iter()
                .skip(1)
                .map(|(m, s)| (m, (s * ENV_RATE).round() as usize))
                .collect();
            assert_eq!(decode_runs(&runs).as_deref(), Some("IABC"), "dot {dot}");
        }
    }

    #[test]
    fn ident_rules() {
        assert!(!IdentRule::Any.plausible("??"));
        assert!(IdentRule::Any.plausible("AB1"));
        // Annex 10: the localizer's I is optional.
        for w in ["ISAC", "SAC", "SA", "TST"] {
            assert!(IdentRule::Localizer.plausible(w), "{w}");
        }
        assert!(!IdentRule::Localizer.plausible("SACR"));
        assert!(!IdentRule::Vor.plausible("ISAC"));
    }

    fn vote(d: &mut IdentDecoder, w: &str, n: usize) {
        for _ in 0..n {
            d.votes.push_back((d.t, w.to_string()));
        }
    }

    #[test]
    fn tied_votes_report_nothing() {
        let mut d = IdentDecoder::new();
        vote(&mut d, "ABC", 3);
        vote(&mut d, "ABD", 3);
        assert_eq!(d.ident(), None);
        let mut d = IdentDecoder::new();
        vote(&mut d, "ABC", 3);
        vote(&mut d, "ABD", 1);
        assert_eq!(d.ident().map(|x| x.0).as_deref(), Some("ABC"));
    }

    #[test]
    fn votes_expire_when_keying_stops() {
        let mut d = IdentDecoder::new();
        let mut env = Vec::new();
        for _ in 0..4 {
            for (on, s) in key("TST", STANDARD_DOT_S, 4.0) {
                let n = (s * ENV_RATE) as usize;
                env.extend(std::iter::repeat_n(if on { 1.0 } else { 0.01 }, n));
            }
        }
        d.push(&env);
        assert!(d.recent_word().is_some());
        // Carrier on, no keying, past the vote window.
        d.push(&vec![0.01f32; ((VOTE_WINDOW_S + 10.0) * ENV_RATE) as usize]);
        assert_eq!(d.words_heard(), 0);
        assert_eq!(d.recent_word(), None);
        assert_eq!(d.last_word.as_deref(), Some("TST"));
    }

    #[test]
    fn stale_fold_gives_way_to_votes() {
        let mut d = IdentDecoder::new();
        d.folded = Some(("ABC".into(), 4));
        d.folded_t = 0;
        vote(&mut d, "XYZ", 3);
        assert_eq!(d.ident().map(|x| x.0).as_deref(), Some("ABC"));
        d.t = (VOTE_WINDOW_S * ENV_RATE) as u64 + 1;
        // Votes were cast at t = 0 too, so cast fresh ones.
        d.votes.clear();
        vote(&mut d, "XYZ", 3);
        assert_eq!(d.ident().map(|x| x.0).as_deref(), Some("XYZ"));
    }

    #[test]
    fn endless_keying_is_dropped() {
        let mut d = IdentDecoder::new();
        // Dots with no word gap for a minute, then a gap.
        let mut env = vec![0.01f32; (2.0 * ENV_RATE) as usize];
        for _ in 0..200 {
            env.extend(std::iter::repeat_n(1.0, 34));
            env.extend(std::iter::repeat_n(0.01, 34));
        }
        env.extend(std::iter::repeat_n(0.01f32, (2.0 * ENV_RATE) as usize));
        d.push(&env);
        assert!(d.runs.is_empty());
        assert_eq!(d.last_word, None);
    }
}
