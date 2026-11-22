//! CTCSS (sub-audible tone) gate.
//!
//! Land-mobile NFM channels share a frequency between users by keying a
//! continuous low tone (67.0–254.1 Hz, EIA/TIA-603 table). The gate measures
//! the configured tone and a set of neighbouring tones over a sliding window
//! of demodulated audio; it reports the tone present when the configured
//! tone's power dominates its neighbours and a fixed share of the audio
//! power.

use crate::filters::Goertzel;

/// The standard CTCSS tone table (Hz).
pub const TONES: [f32; 50] = [
    67.0, 69.3, 71.9, 74.4, 77.0, 79.7, 82.5, 85.4, 88.5, 91.5, 94.8, 97.4, 100.0, 103.5, 107.2,
    110.9, 114.8, 118.8, 123.0, 127.3, 131.8, 136.5, 141.3, 146.2, 150.0, 151.4, 156.7, 159.8,
    162.2, 165.5, 167.9, 171.3, 173.8, 177.3, 179.9, 183.5, 186.2, 189.9, 192.8, 196.6, 199.5,
    203.5, 206.5, 210.7, 218.1, 225.7, 229.1, 233.6, 241.8, 250.3,
];

/// Window length; resolves tones ~2.5 Hz apart at the bottom of the table.
pub const WINDOW_S: f64 = 0.4;

pub struct CtcssGate {
    target: Goertzel,
    neighbours: Vec<Goertzel>,
    window: usize,
    energy: f32,
    present: bool,
}

impl CtcssGate {
    pub fn new(tone_hz: f32, rate: f64) -> Self {
        // Compare against the two nearest table tones on each side, plus
        // half-way points, so an adjacent tone cannot open the gate.
        let mut others: Vec<f32> = TONES
            .iter()
            .copied()
            .filter(|t| (t - tone_hz).abs() > 1.0)
            .collect();
        others.sort_by(|a, b| (a - tone_hz).abs().total_cmp(&(b - tone_hz).abs()));
        others.truncate(4);
        Self {
            target: Goertzel::new(tone_hz as f64, rate),
            neighbours: others.iter().map(|&f| Goertzel::new(f as f64, rate)).collect(),
            window: (WINDOW_S * rate) as usize,
            energy: 0.0,
            present: false,
        }
    }

    /// Feed demodulated audio (before the voice high-pass, which removes the
    /// tone). Returns whether the tone is present as of the last full window.
    pub fn push(&mut self, audio: &[f32]) -> bool {
        for &x in audio {
            self.target.push(x);
            for g in &mut self.neighbours {
                g.push(x);
            }
            self.energy += x * x;
            if self.target.len() >= self.window {
                let p = self.target.power();
                let mean_sq = self.energy / self.window as f32;
                let best_other = self.neighbours.iter().map(|g| g.power()).fold(0.0f32, f32::max);
                // A pure tone of amplitude A reads A²/4 in `p` and A²/2 in
                // mean_sq; require the tone to carry ≥ ~2 % of the audio power
                // and to beat every neighbour by 6 dB.
                self.present = p > 0.01 * mean_sq && p > 4.0 * best_other;
                self.target.reset();
                for g in &mut self.neighbours {
                    g.reset();
                }
                self.energy = 0.0;
            }
        }
        self.present
    }

    pub fn present(&self) -> bool {
        self.present
    }

    pub fn reset(&mut self) {
        self.target.reset();
        for g in &mut self.neighbours {
            g.reset();
        }
        self.energy = 0.0;
        self.present = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f64::consts::PI;

    fn audio_with_tone(tone: f64, rate: f64, secs: f64) -> Vec<f32> {
        let n = (rate * secs) as usize;
        (0..n)
            .map(|i| {
                let t = i as f64 / rate;
                // voice-band content plus a tone at -20 dB relative
                (0.5 * (2.0 * PI * 900.0 * t).sin()
                    + 0.3 * (2.0 * PI * 1_700.0 * t).sin()
                    + 0.1 * (2.0 * PI * tone * t).sin()) as f32
            })
            .collect()
    }

    #[test]
    fn detects_its_tone() {
        let rate = 16_000.0;
        let mut g = CtcssGate::new(100.0, rate);
        assert!(g.push(&audio_with_tone(100.0, rate, 1.0)));
    }

    #[test]
    fn rejects_neighbour_tone() {
        let rate = 16_000.0;
        let mut g = CtcssGate::new(100.0, rate);
        assert!(!g.push(&audio_with_tone(103.5, rate, 1.0)));
        let mut g = CtcssGate::new(100.0, rate);
        assert!(!g.push(&audio_with_tone(97.4, rate, 1.0)));
    }

    #[test]
    fn rejects_no_tone() {
        let rate = 16_000.0;
        let mut g = CtcssGate::new(100.0, rate);
        let x: Vec<f32> = audio_with_tone(100.0, rate, 1.0)
            .iter()
            .enumerate()
            .map(|(i, _)| (0.5 * (2.0 * PI * 900.0 * i as f64 / rate).sin()) as f32)
            .collect();
        assert!(!g.push(&x));
    }
}
