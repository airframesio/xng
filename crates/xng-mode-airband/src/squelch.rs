//! Block-based carrier squelch.
//!
//! The channel's power is measured over short blocks (10 ms). A noise-floor
//! estimate follows the quiet channel: it drops quickly to a lower block (so a
//! transmission that ends is noticed at once) and rises slowly (so a
//! transmission does not drag the floor up with it). The squelch opens when a
//! block is a set margin above the floor (`Auto`) or above a fixed level
//! (`Manual`) for `open_blocks` blocks in a row, and closes after `hang_blocks`
//! blocks below the threshold minus a hysteresis margin.
//!
//! An optional tone gate (CTCSS) must also report the tone before the squelch
//! opens, and its loss closes the squelch like a carrier drop.

use serde::{Deserialize, Serialize};

/// How the open threshold is chosen. Written as text in configs:
/// `auto`, `auto:12` (margin in dB), `manual:-40` (dBFS) or `off`.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub enum SquelchMode {
    /// Open when the channel is `snr_db` above the tracked noise floor.
    Auto { snr_db: f32 },
    /// Open above a fixed channel power in dBFS.
    Manual { dbfs: f32 },
    /// Always open (continuous audio).
    Off,
}

impl Default for SquelchMode {
    fn default() -> Self {
        SquelchMode::Auto { snr_db: DEFAULT_SNR_DB }
    }
}

impl std::str::FromStr for SquelchMode {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, String> {
        let s = s.trim().to_ascii_lowercase();
        let (kind, arg) = match s.split_once(':') {
            Some((k, a)) => (k, Some(a)),
            None => (s.as_str(), None),
        };
        let num = |a: Option<&str>| -> Result<Option<f32>, String> {
            a.map(|v| v.trim().parse::<f32>().map_err(|_| format!("bad squelch value {v:?}"))).transpose()
        };
        Ok(match kind {
            "auto" => SquelchMode::Auto { snr_db: num(arg)?.unwrap_or(DEFAULT_SNR_DB) },
            "manual" => SquelchMode::Manual {
                dbfs: num(arg)?.ok_or("manual squelch needs a level, e.g. manual:-40")?,
            },
            "off" => SquelchMode::Off,
            other => return Err(format!("unknown squelch {other:?} (auto[:snr_db], manual:dbfs, off)")),
        })
    }
}

impl std::fmt::Display for SquelchMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SquelchMode::Auto { snr_db } => write!(f, "auto:{snr_db}"),
            SquelchMode::Manual { dbfs } => write!(f, "manual:{dbfs}"),
            SquelchMode::Off => f.write_str("off"),
        }
    }
}

impl TryFrom<String> for SquelchMode {
    type Error = String;
    fn try_from(s: String) -> Result<Self, String> {
        s.parse()
    }
}

impl From<SquelchMode> for String {
    fn from(m: SquelchMode) -> String {
        m.to_string()
    }
}

/// Default auto-squelch margin above the noise floor.
pub const DEFAULT_SNR_DB: f32 = 9.0;
/// Close threshold sits this far below the open threshold.
pub const HYSTERESIS_DB: f32 = 3.0;

/// What a block did to the squelch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Transition {
    None,
    Opened,
    Closed,
}

/// Rate per block at which the floor falls toward a quieter block.
const FLOOR_DOWN: f32 = 0.25;

#[derive(Clone, Debug)]
pub struct Squelch {
    mode: SquelchMode,
    floor_db: Option<f32>,
    floor_up: f32,
    floor_up_open: f32,
    open: bool,
    above: u32,
    below: u32,
    open_blocks: u32,
    hang_blocks: u32,
}

impl Squelch {
    /// `block_s` is the block length in seconds; `open_s` the time a signal
    /// must be present before opening; `hang_s` how long the squelch stays open
    /// after the signal drops.
    pub fn new(mode: SquelchMode, block_s: f32, open_s: f32, hang_s: f32) -> Self {
        let blocks = |s: f32| ((s / block_s).round() as u32).max(1);
        // Rise time constants: 5 s while closed, 60 s while open (so a stuck
        // carrier eventually closes instead of holding the channel forever).
        let up = |tc: f32| 1.0 - (-block_s / tc).exp();
        Self {
            mode,
            floor_db: None,
            floor_up: up(5.0),
            floor_up_open: up(60.0),
            open: matches!(mode, SquelchMode::Off),
            above: 0,
            below: 0,
            open_blocks: blocks(open_s),
            hang_blocks: if hang_s <= 0.0 { 0 } else { blocks(hang_s) },
        }
    }

    pub fn mode(&self) -> SquelchMode {
        self.mode
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    /// Current noise-floor estimate in dBFS (`-inf` before the first block).
    pub fn noise_db(&self) -> f32 {
        self.floor_db.unwrap_or(f32::NEG_INFINITY)
    }

    /// Current open threshold in dBFS.
    pub fn threshold_db(&self) -> f32 {
        match self.mode {
            SquelchMode::Auto { snr_db } => self.noise_db() + snr_db,
            SquelchMode::Manual { dbfs } => dbfs,
            SquelchMode::Off => f32::NEG_INFINITY,
        }
    }

    /// Number of blocks a signal must persist before the squelch opens. The
    /// caller keeps this many blocks of audio as pre-roll so the first
    /// syllable is not lost.
    pub fn open_blocks(&self) -> u32 {
        self.open_blocks
    }

    /// Feed one block's mean power (dBFS). `tone_ok` is the CTCSS gate (pass
    /// `true` when no tone is configured).
    pub fn update(&mut self, power_db: f32, tone_ok: bool) -> Transition {
        if !power_db.is_finite() {
            return Transition::None;
        }
        // Noise floor tracking.
        let floor = self.floor_db.get_or_insert(power_db);
        if power_db < *floor {
            *floor += FLOOR_DOWN * (power_db - *floor);
        } else if !self.open || self.below > 0 {
            *floor += self.floor_up * (power_db - *floor);
        } else {
            *floor += self.floor_up_open * (power_db - *floor);
        }

        if matches!(self.mode, SquelchMode::Off) {
            return Transition::None;
        }

        let open_at = self.threshold_db();
        if !self.open {
            if power_db >= open_at && tone_ok {
                self.above += 1;
                if self.above >= self.open_blocks {
                    self.open = true;
                    self.above = 0;
                    self.below = 0;
                    return Transition::Opened;
                }
            } else {
                self.above = 0;
            }
            Transition::None
        } else {
            let close_at = open_at - HYSTERESIS_DB;
            if power_db < close_at || !tone_ok {
                self.below += 1;
                if self.below > self.hang_blocks {
                    self.open = false;
                    self.below = 0;
                    self.above = 0;
                    return Transition::Closed;
                }
            } else {
                self.below = 0;
            }
            Transition::None
        }
    }

    /// True while the squelch is open but the signal has dropped (hang time).
    pub fn in_hang(&self) -> bool {
        self.open && self.below > 0
    }

    /// Force the squelch closed (used when a scanning receiver retunes).
    pub fn reset(&mut self) {
        self.open = matches!(self.mode, SquelchMode::Off);
        self.above = 0;
        self.below = 0;
    }

    /// Forget the noise floor too (a retune lands on a different channel).
    pub fn reset_floor(&mut self) {
        self.reset();
        self.floor_db = None;
    }

    /// Seed the noise floor (a scanning receiver restores the floor it last
    /// measured on a channel instead of starting from scratch).
    pub fn set_floor(&mut self, db: f32) {
        if db.is_finite() {
            self.floor_db = Some(db);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sq() -> Squelch {
        Squelch::new(SquelchMode::Auto { snr_db: 9.0 }, 0.01, 0.02, 0.1)
    }

    #[test]
    fn opens_after_two_loud_blocks_and_closes_after_hang() {
        let mut s = sq();
        for _ in 0..200 {
            assert_eq!(s.update(-50.0, true), Transition::None);
        }
        assert!((s.noise_db() + 50.0).abs() < 0.5);
        assert_eq!(s.update(-30.0, true), Transition::None);
        assert_eq!(s.update(-30.0, true), Transition::Opened);
        assert!(s.is_open());
        // Ten hang blocks keep it open, the eleventh closes it.
        for _ in 0..10 {
            assert_eq!(s.update(-50.0, true), Transition::None);
            assert!(s.in_hang());
        }
        assert_eq!(s.update(-50.0, true), Transition::Closed);
    }

    #[test]
    fn single_spike_does_not_open() {
        let mut s = sq();
        for _ in 0..100 {
            s.update(-50.0, true);
        }
        assert_eq!(s.update(-20.0, true), Transition::None);
        assert_eq!(s.update(-50.0, true), Transition::None);
        assert!(!s.is_open());
    }

    #[test]
    fn hysteresis_holds_a_fading_signal() {
        let mut s = sq();
        for _ in 0..100 {
            s.update(-50.0, true);
        }
        s.update(-40.0, true);
        assert_eq!(s.update(-40.0, true), Transition::Opened);
        // Threshold is ~-41; -43 is inside the 3 dB hysteresis, stays open.
        for _ in 0..100 {
            assert_eq!(s.update(-43.0, true), Transition::None);
        }
        assert!(s.is_open());
    }

    #[test]
    fn floor_tracks_a_rising_noise_slowly() {
        let mut s = sq();
        for _ in 0..100 {
            s.update(-60.0, true);
        }
        // Noise rises 6 dB: below the 9 dB margin, so never opens, and the
        // floor follows it up over a few seconds.
        for _ in 0..1500 {
            assert_eq!(s.update(-54.0, true), Transition::None);
        }
        assert!((s.noise_db() + 54.0).abs() < 1.0, "{}", s.noise_db());
    }

    #[test]
    fn stuck_carrier_eventually_closes() {
        let mut s = sq();
        for _ in 0..100 {
            s.update(-60.0, true);
        }
        let mut closed = false;
        // A carrier that never stops: the open-state floor rise (60 s) lets
        // the squelch close within a few minutes.
        for _ in 0..30_000 {
            if s.update(-40.0, true) == Transition::Closed {
                closed = true;
                break;
            }
        }
        assert!(closed);
    }

    #[test]
    fn manual_threshold() {
        let mut s = Squelch::new(SquelchMode::Manual { dbfs: -35.0 }, 0.01, 0.01, 0.0);
        assert_eq!(s.update(-40.0, true), Transition::None);
        assert_eq!(s.update(-30.0, true), Transition::Opened);
        assert_eq!(s.update(-39.0, true), Transition::Closed);
    }

    #[test]
    fn tone_gate_blocks_and_drops() {
        let mut s = Squelch::new(SquelchMode::Manual { dbfs: -35.0 }, 0.01, 0.01, 0.0);
        assert_eq!(s.update(-20.0, false), Transition::None);
        assert_eq!(s.update(-20.0, true), Transition::Opened);
        assert_eq!(s.update(-20.0, false), Transition::Closed);
    }

    #[test]
    fn off_is_always_open() {
        let mut s = Squelch::new(SquelchMode::Off, 0.01, 0.02, 0.1);
        assert!(s.is_open());
        assert_eq!(s.update(-90.0, true), Transition::None);
        assert!(s.is_open());
    }
}
