//! The VHF/UHF navaid channel plan (ICAO Annex 10, Volume I, 3.1.3, 3.1.5,
//! 3.3.2 and the DME pairing table in 3.5.3.3.4).
//!
//! - VOR: 108.00–117.95 MHz at 50 kHz spacing. Below 112 MHz VORs use the
//!   even tenths (108.00, 108.05, 108.20, 108.25 …) and localizers the odd
//!   tenths (108.10, 108.15, 108.30 …).
//! - Localizer: the 40 odd-tenth channels 108.10–111.95 MHz.
//! - Glideslope: 40 UHF channels 328.6–335.4 MHz, each paired one to one with
//!   a localizer.
//! - DME: 1X–126X / 1Y–126Y; channels 17X–126Y pair with the VHF channels.

use serde::{Deserialize, Serialize};

/// What kind of navaid a channel carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NavaidKind {
    Vor,
    Localizer,
    Glideslope,
}

impl NavaidKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Vor => "vor",
            Self::Localizer => "localizer",
            Self::Glideslope => "glideslope",
        }
    }
}

/// The localizer/glideslope pairing table, in localizer order (kHz).
pub const ILS_PAIRS_KHZ: [(u32, u32); 40] = [
    (108_100, 334_700),
    (108_150, 334_550),
    (108_300, 334_100),
    (108_350, 333_950),
    (108_500, 329_900),
    (108_550, 329_750),
    (108_700, 330_500),
    (108_750, 330_350),
    (108_900, 329_300),
    (108_950, 329_150),
    (109_100, 331_400),
    (109_150, 331_250),
    (109_300, 332_000),
    (109_350, 331_850),
    (109_500, 332_600),
    (109_550, 332_450),
    (109_700, 333_200),
    (109_750, 333_050),
    (109_900, 333_800),
    (109_950, 333_650),
    (110_100, 334_400),
    (110_150, 334_250),
    (110_300, 335_000),
    (110_350, 334_850),
    (110_500, 329_600),
    (110_550, 329_450),
    (110_700, 330_200),
    (110_750, 330_050),
    (110_900, 330_800),
    (110_950, 330_650),
    (111_100, 331_700),
    (111_150, 331_550),
    (111_300, 332_300),
    (111_350, 332_150),
    (111_500, 332_900),
    (111_550, 332_750),
    (111_700, 333_500),
    (111_750, 333_350),
    (111_900, 331_100),
    (111_950, 330_950),
];

fn khz(freq_hz: u64) -> Option<u32> {
    // Navaid channels sit on a 50 kHz raster; allow ±2 kHz of tuning slop.
    let k = ((freq_hz + 25_000) / 50_000) * 50;
    let err = (freq_hz as i64 - k as i64 * 1000).abs();
    (err <= 2_000).then_some(k as u32)
}

/// Which navaid a channel frequency belongs to, if any.
pub fn kind_for_freq(freq_hz: u64) -> Option<NavaidKind> {
    let k = khz(freq_hz)?;
    if (328_600..=335_400).contains(&k) {
        return ILS_PAIRS_KHZ
            .iter()
            .any(|p| p.1 == k)
            .then_some(NavaidKind::Glideslope);
    }
    if !(108_000..=117_950).contains(&k) {
        return None;
    }
    if k < 112_000 && (k / 100) % 2 == 1 {
        Some(NavaidKind::Localizer)
    } else {
        Some(NavaidKind::Vor)
    }
}

/// The glideslope paired with a localizer (Hz).
pub fn glideslope_for_localizer(loc_hz: u64) -> Option<u64> {
    let k = khz(loc_hz)?;
    ILS_PAIRS_KHZ
        .iter()
        .find(|p| p.0 == k)
        .map(|p| p.1 as u64 * 1000)
}

/// The localizer paired with a glideslope (Hz).
pub fn localizer_for_glideslope(gs_hz: u64) -> Option<u64> {
    let k = khz(gs_hz)?;
    ILS_PAIRS_KHZ
        .iter()
        .find(|p| p.1 == k)
        .map(|p| p.0 as u64 * 1000)
}

/// The DME channel paired with a VOR or localizer frequency, e.g. 108.00 MHz
/// is `17X`, 108.05 MHz is `17Y`, 117.95 MHz is `126Y`.
pub fn dme_channel_for_vhf(freq_hz: u64) -> Option<String> {
    let k = khz(freq_hz)?;
    if !(108_000..=117_950).contains(&k) {
        return None;
    }
    let step = (k - 108_000) / 50; // 0..=199
    let n = 17 + step / 2;
    let xy = if step % 2 == 0 { 'X' } else { 'Y' };
    // 60X–69Y have no VHF pair: 112.25 MHz is 59Y and 112.30 MHz is 70X.
    let n = if k >= 112_300 { n + 10 } else { n };
    Some(format!("{n}{xy}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kinds() {
        assert_eq!(kind_for_freq(108_000_000), Some(NavaidKind::Vor));
        assert_eq!(kind_for_freq(108_100_000), Some(NavaidKind::Localizer));
        assert_eq!(kind_for_freq(108_150_000), Some(NavaidKind::Localizer));
        assert_eq!(kind_for_freq(108_200_000), Some(NavaidKind::Vor));
        assert_eq!(kind_for_freq(111_950_000), Some(NavaidKind::Localizer));
        assert_eq!(kind_for_freq(112_100_000), Some(NavaidKind::Vor));
        assert_eq!(kind_for_freq(115_200_000), Some(NavaidKind::Vor));
        assert_eq!(kind_for_freq(332_000_000), Some(NavaidKind::Glideslope));
        assert_eq!(kind_for_freq(118_000_000), None);
        assert_eq!(kind_for_freq(108_120_000), None);
    }

    #[test]
    fn pairs() {
        assert_eq!(glideslope_for_localizer(109_300_000), Some(332_000_000));
        assert_eq!(glideslope_for_localizer(110_300_000), Some(335_000_000));
        assert_eq!(localizer_for_glideslope(329_150_000), Some(108_950_000));
        // Every pair is unique both ways.
        for (l, g) in ILS_PAIRS_KHZ {
            assert_eq!(kind_for_freq(l as u64 * 1000), Some(NavaidKind::Localizer));
            assert_eq!(
                localizer_for_glideslope(g as u64 * 1000),
                Some(l as u64 * 1000)
            );
        }
    }

    #[test]
    fn dme() {
        assert_eq!(dme_channel_for_vhf(108_000_000).as_deref(), Some("17X"));
        assert_eq!(dme_channel_for_vhf(108_050_000).as_deref(), Some("17Y"));
        assert_eq!(dme_channel_for_vhf(111_950_000).as_deref(), Some("56Y"));
        assert_eq!(dme_channel_for_vhf(112_000_000).as_deref(), Some("57X"));
        assert_eq!(dme_channel_for_vhf(112_250_000).as_deref(), Some("59Y"));
        assert_eq!(dme_channel_for_vhf(112_300_000).as_deref(), Some("70X"));
        assert_eq!(dme_channel_for_vhf(117_950_000).as_deref(), Some("126Y"));
    }
}
