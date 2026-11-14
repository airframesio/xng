//! WWV / WWVH (NIST) 100 Hz subcarrier BCD time-code decoder.
//!
//! WWV and WWVH carry an identical 1-bit-per-second, 60-second time code on a
//! 100 Hz subcarrier (a modified IRIG-H frame). Each second is a 100 Hz tone
//! burst whose duration codes the bit:
//!
//! - **binary 0 = 170 ms**, **binary 1 = 470 ms**, **position marker = 770 ms**
//!   (nominal 200/500/800 ms minus a 30 ms tone-suppressed lead-in, so each
//!   pulse starts 30 ms after the true second).
//! - **Second 0 = a hole** (no 100 Hz pulse) = the frame reference.
//!
//! BCD is **LSB-first, weights 1-2-4-8**. The 60-second layout, fields, and
//! the per-second classifier are from the NIST WWV/WWVH time-code description
//! (NIST SP-432); see PROVENANCE.md.
//!
//! The station is labelled by its seconds-tick tone (WWV = 1000 Hz, WWVH =
//! 1200 Hz); the time code itself is identical. This module decodes the time
//! code; station labelling lives in [`crate`] (it has the audio).

use crate::audio::{Biquad, Goertzel};

/// Subcarrier frequency carrying the time code, Hz.
pub const SUBCARRIER_HZ: f64 = 100.0;
/// Tone-suppressed lead-in at the start of every pulse, seconds.
pub const LEADIN_S: f64 = 0.030;
/// Decoded per-second symbol.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Symbol {
    /// Binary 0 (~170 ms tone).
    Zero,
    /// Binary 1 (~470 ms tone).
    One,
    /// Position marker (~770 ms tone) — appears at seconds 9,19,29,39,49,59.
    Marker,
    /// No pulse (the second-0 reference hole, or a missed/empty second).
    Hole,
}

impl Symbol {
    /// Bit value for BCD assembly (markers and holes contribute 0).
    fn bit(self) -> u8 {
        matches!(self, Symbol::One) as u8
    }
}

/// Classify one second's measured 100 Hz tone-burst length into a [`Symbol`].
/// Thresholds: `< 0.32 s → 0`, `0.32–0.62 s → 1`, `> 0.62 s → marker`; a length
/// below ~50 ms is treated as a hole (no pulse).
pub fn classify(tone_len_s: f64) -> Symbol {
    if tone_len_s < 0.05 {
        Symbol::Hole
    } else if tone_len_s < 0.32 {
        Symbol::Zero
    } else if tone_len_s <= 0.62 {
        Symbol::One
    } else {
        Symbol::Marker
    }
}

/// Measure the 100 Hz tone-burst length within one second of audio.
///
/// `audio` is the AM-demodulated audio for one second (length ≈ `sample_rate`).
/// The chain is: 100 Hz bandpass → per-window Goertzel power → threshold
/// against the per-second noise floor → count contiguous in-tone time from the
/// pulse onset. Returns the burst length in seconds (0 for a hole).
pub fn tone_length(audio: &[f32], sample_rate: f64) -> f64 {
    if audio.is_empty() {
        return 0.0;
    }
    // Narrow bandpass on the 100 Hz subcarrier.
    let mut bp = Biquad::bandpass(SUBCARRIER_HZ, 8.0, sample_rate);
    let filt = bp.filter(audio);

    // Short Goertzel windows (~20 ms) give the on/off envelope at the
    // resolution we need (170/470/770 ms boundaries are 300 ms apart).
    let win = (sample_rate * 0.020).round().max(1.0) as usize;
    let mut powers = Vec::new();
    let mut g = Goertzel::new(SUBCARRIER_HZ, sample_rate);
    let mut count = 0usize;
    for &s in &filt {
        g.add(s);
        count += 1;
        if count >= win {
            powers.push(g.power() / win as f32);
            g.reset();
            count = 0;
        }
    }
    if powers.is_empty() {
        return 0.0;
    }

    // Threshold halfway (geometric) between the quiet floor and the peak.
    let peak = powers.iter().cloned().fold(0.0f32, f32::max);
    let floor = powers.iter().cloned().fold(f32::INFINITY, f32::min);
    if peak <= 0.0 {
        return 0.0;
    }
    let thresh = (floor.max(peak * 1e-4) * peak).sqrt().max(peak * 0.15);

    // Count the longest run of in-tone windows (the pulse). Holes have no run.
    let mut best_run = 0usize;
    let mut run = 0usize;
    for &p in &powers {
        if p >= thresh {
            run += 1;
            best_run = best_run.max(run);
        } else {
            run = 0;
        }
    }
    let win_s = win as f64 / sample_rate;
    best_run as f64 * win_s
}

/// A fully parsed WWV/WWVH minute.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct WwvFrame {
    /// UTC hour (0–23).
    pub hour: u8,
    /// UTC minute (0–59).
    pub minute: u8,
    /// Day of year (1–366).
    pub day_of_year: u16,
    /// Full year (2000 + decade*10 + units).
    pub year: u16,
    /// DUT1 in seconds (signed, 0.1 s resolution).
    pub dut1_s_tenths: i8,
    /// Leap-second pending (second 3).
    pub leap_pending: bool,
    /// Daylight-saving indicator bits (DST1 = sec55, DST2 = sec2).
    pub dst1: bool,
    pub dst2: bool,
    /// How many of the 6 position markers + the sec-0 hole were found (0–7):
    /// a sync-confidence measure.
    pub sync_score: u8,
}

/// BCD weights, LSB-first.
const W: [u16; 4] = [1, 2, 4, 8];

/// Assemble a BCD value from `symbols` at the given `(second, weight)` pairs.
fn bcd(symbols: &[Symbol], pairs: &[(usize, u16)]) -> u16 {
    pairs
        .iter()
        .map(|&(sec, w)| symbols[sec].bit() as u16 * w)
        .sum()
}

/// Parse a 60-symbol minute (index = second 0..59) into a [`WwvFrame`].
///
/// Returns `None` if the frame doesn't sync: the second-0 hole and the six
/// position markers at {9,19,29,39,49,59} are the reference grid; we require at
/// least the sec-0 hole plus a majority of markers.
pub fn parse_minute(symbols: &[Symbol]) -> Option<WwvFrame> {
    if symbols.len() < 60 {
        return None;
    }
    // Sync grid: sec 0 is a hole; secs 9,19,29,39,49,59 are markers.
    let markers = [9usize, 19, 29, 39, 49, 59];
    let hole_ok = symbols[0] == Symbol::Hole;
    let marker_hits = markers.iter().filter(|&&s| symbols[s] == Symbol::Marker).count();
    let sync_score = hole_ok as u8 + marker_hits as u8;
    if !hole_ok || marker_hits < 4 {
        return None;
    }

    // MINUTE = units(10-13) + tens(15-17).
    let min_units = bcd(symbols, &[(10, W[0]), (11, W[1]), (12, W[2]), (13, W[3])]);
    let min_tens = bcd(symbols, &[(15, 10), (16, 20), (17, 40)]);
    let minute = (min_units + min_tens) as u8;

    // HOUR = units(20-23) + tens(25,26).
    let hour_units = bcd(symbols, &[(20, W[0]), (21, W[1]), (22, W[2]), (23, W[3])]);
    let hour_tens = bcd(symbols, &[(25, 10), (26, 20)]);
    let hour = (hour_units + hour_tens) as u8;

    // DOY = units(30-33) + tens(35-38) + hundreds(40,41).
    let doy_units = bcd(symbols, &[(30, W[0]), (31, W[1]), (32, W[2]), (33, W[3])]);
    let doy_tens = bcd(symbols, &[(35, 10), (36, 20), (37, 40), (38, 80)]);
    let doy_hundreds = bcd(symbols, &[(40, 100), (41, 200)]);
    let day_of_year = doy_units + doy_tens + doy_hundreds;

    // YEAR = 2000 + decade(51-54)*1 weights 10/20/40/80 + units(4-7).
    let year_units = bcd(symbols, &[(4, W[0]), (5, W[1]), (6, W[2]), (7, W[3])]);
    let year_decade = bcd(symbols, &[(51, 10), (52, 20), (53, 40), (54, 80)]);
    let year = 2000 + year_decade + year_units;

    // DUT1 = ±(56*1 + 57*2 + 58*4)*0.1 s, sign from sec 50 (1 = +).
    let dut1_mag = bcd(symbols, &[(56, 1), (57, 2), (58, 4)]) as i8;
    let dut1_sign = if symbols[50] == Symbol::One { 1 } else { -1 };
    let dut1_s_tenths = dut1_sign * dut1_mag;

    let leap_pending = symbols[3] == Symbol::One;
    let dst1 = symbols[55] == Symbol::One;
    let dst2 = symbols[2] == Symbol::One;

    if hour > 23 || minute > 59 || !(1..=366).contains(&day_of_year) {
        return None;
    }

    Some(WwvFrame {
        hour,
        minute,
        day_of_year,
        year,
        dut1_s_tenths,
        leap_pending,
        dst1,
        dst2,
        sync_score,
    })
}

/// Station label inferred from which seconds-tick tone is present.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WwvStation {
    /// 1000 Hz tick → WWV (Colorado).
    Wwv,
    /// 1200 Hz tick → WWVH (Hawaii).
    Wwvh,
    /// Tick tone not determined.
    Unknown,
}

impl WwvStation {
    pub fn name(self) -> &'static str {
        match self {
            WwvStation::Wwv => "WWV",
            WwvStation::Wwvh => "WWVH",
            WwvStation::Unknown => "WWV/WWVH",
        }
    }
}

/// Label the station by comparing 1000 Hz (WWV) vs 1200 Hz (WWVH) tick energy
/// over a slice of audio (a second's tick is a short burst near the second
/// boundary; integrating the whole minute's audio is enough to pick the tone
/// that's consistently present).
pub fn label_station(audio: &[f32], sample_rate: f64) -> WwvStation {
    if audio.is_empty() {
        return WwvStation::Unknown;
    }
    let mut g_wwv = Goertzel::new(1000.0, sample_rate);
    let mut g_wwvh = Goertzel::new(1200.0, sample_rate);
    for &s in audio {
        g_wwv.add(s);
        g_wwvh.add(s);
    }
    let (pw, ph) = (g_wwv.power(), g_wwvh.power());
    if pw <= 0.0 && ph <= 0.0 {
        return WwvStation::Unknown;
    }
    if pw >= ph * 1.5 {
        WwvStation::Wwv
    } else if ph >= pw * 1.5 {
        WwvStation::Wwvh
    } else {
        WwvStation::Unknown
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_thresholds() {
        assert_eq!(classify(0.0), Symbol::Hole);
        assert_eq!(classify(0.17), Symbol::Zero);
        assert_eq!(classify(0.31), Symbol::Zero);
        assert_eq!(classify(0.47), Symbol::One);
        assert_eq!(classify(0.62), Symbol::One);
        assert_eq!(classify(0.77), Symbol::Marker);
    }

    /// Build a 60-symbol minute encoding a known UTC.
    #[allow(clippy::too_many_arguments)]
    fn make_minute(
        year: u16,
        doy: u16,
        hour: u8,
        minute: u8,
        dut1_tenths: i8,
        leap: bool,
        dst1: bool,
        dst2: bool,
    ) -> Vec<Symbol> {
        let mut s = vec![Symbol::Zero; 60];
        // Reference grid.
        s[0] = Symbol::Hole;
        for &m in &[9usize, 19, 29, 39, 49, 59] {
            s[m] = Symbol::Marker;
        }
        let set = |s: &mut [Symbol], sec: usize, on: bool| {
            s[sec] = if on { Symbol::One } else { Symbol::Zero };
        };
        // BCD writer (LSB-first across the listed seconds).
        let put = |s: &mut [Symbol], secs: &[usize], mut v: u16| {
            for &sec in secs {
                set(s, sec, v & 1 == 1);
                v >>= 1;
            }
        };
        // minute units(10-13)/tens.
        put(&mut s, &[10, 11, 12, 13], minute as u16 % 10);
        put(&mut s, &[15, 16, 17], minute as u16 / 10);
        // hour.
        put(&mut s, &[20, 21, 22, 23], hour as u16 % 10);
        put(&mut s, &[25, 26], hour as u16 / 10);
        // doy.
        put(&mut s, &[30, 31, 32, 33], doy % 10);
        put(&mut s, &[35, 36, 37, 38], (doy / 10) % 10);
        put(&mut s, &[40, 41], doy / 100);
        // year units(4-7) + decade(51-54).
        let yy = year - 2000;
        put(&mut s, &[4, 5, 6, 7], yy % 10);
        put(&mut s, &[51, 52, 53, 54], yy / 10);
        // DUT1 sign(50) + magnitude(56-58).
        set(&mut s, 50, dut1_tenths >= 0);
        put(&mut s, &[56, 57, 58], dut1_tenths.unsigned_abs() as u16);
        // flags.
        set(&mut s, 3, leap);
        set(&mut s, 55, dst1);
        set(&mut s, 2, dst2);
        s
    }

    #[test]
    fn parse_minute_round_trip() {
        let s = make_minute(2026, 159, 12, 34, 3, false, true, false);
        let f = parse_minute(&s).unwrap();
        assert_eq!(f.year, 2026);
        assert_eq!(f.day_of_year, 159);
        assert_eq!(f.hour, 12);
        assert_eq!(f.minute, 34);
        assert_eq!(f.dut1_s_tenths, 3);
        assert!(!f.leap_pending);
        assert!(f.dst1);
        assert!(!f.dst2);
        assert_eq!(f.sync_score, 7);
    }

    #[test]
    fn parse_minute_negative_dut1_and_leap() {
        let s = make_minute(2024, 366, 23, 59, -4, true, false, true);
        let f = parse_minute(&s).unwrap();
        assert_eq!(f.year, 2024);
        assert_eq!(f.day_of_year, 366);
        assert_eq!(f.hour, 23);
        assert_eq!(f.minute, 59);
        assert_eq!(f.dut1_s_tenths, -4);
        assert!(f.leap_pending);
        assert!(f.dst2);
    }

    #[test]
    fn parse_minute_fails_without_sync() {
        let mut s = make_minute(2026, 100, 1, 2, 0, false, false, false);
        s[0] = Symbol::Zero; // destroy the reference hole
        assert!(parse_minute(&s).is_none());
    }
}
