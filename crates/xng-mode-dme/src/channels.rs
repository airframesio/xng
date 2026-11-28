//! DME channel plan (ICAO Annex 10 Vol I §3.5.3.3 and Table A).
//!
//! 252 channels, 1X..126X and 1Y..126Y. The aircraft interrogates on
//! `1024 + n` MHz; the ground transponder replies 63 MHz below or above:
//!
//! | channels | interrogation | reply |
//! |---|---|---|
//! | 1X–63X   | 1025–1087 MHz | 962–1024 MHz (−63) |
//! | 64X–126X | 1088–1150 MHz | 1151–1213 MHz (+63) |
//! | 1Y–63Y   | 1025–1087 MHz | 1088–1150 MHz (+63) |
//! | 64Y–126Y | 1088–1150 MHz | 1025–1087 MHz (−63) |
//!
//! Pulse-pair spacing (µs): X 12/12, Y 36 interrogation / 30 reply.
//! Transponder reply delay (µs): X 50, Y 56.
//!
//! VHF pairing (DME collocated with a VOR or an ILS localizer): 17X..59X ↔
//! 108.00..112.20 MHz and 70X..126X ↔ 112.30..117.90 MHz in 100 kHz steps,
//! the Y channel of the same number 50 kHz above. 108.00–111.95 MHz with an
//! odd tenth digit is an ILS localizer; the rest are VORs. Channels 1–16 and
//! 60–69 have no VHF pair (DME-only / TACAN).
//!
//! Written from the Annex 10 tables; no code from other decoders.

use serde::{Deserialize, Serialize};
use std::fmt;

/// X or Y channel (W/Z are DME/P only and not handled).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Band {
    X,
    Y,
}

/// Which side of the link a frequency carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    /// Ground transponder: replies, squitter and ident.
    Reply,
    /// Airborne interrogators.
    Interrogation,
}

/// What a paired VHF frequency is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PairedKind {
    Vor,
    Ils,
}

/// One DME channel (1..=126, X or Y).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DmeChannel {
    pub number: u8,
    pub band: Band,
}

impl DmeChannel {
    pub fn new(number: u8, band: Band) -> Option<Self> {
        (1..=126).contains(&number).then_some(Self { number, band })
    }

    /// Every channel, 1X..126X then 1Y..126Y.
    pub fn all() -> impl Iterator<Item = DmeChannel> {
        [Band::X, Band::Y]
            .into_iter()
            .flat_map(|b| (1..=126u8).map(move |n| DmeChannel { number: n, band: b }))
    }

    /// Airborne interrogation frequency, Hz.
    pub fn interrogation_hz(&self) -> u64 {
        (1024 + self.number as u64) * 1_000_000
    }

    /// Ground reply frequency, Hz.
    pub fn reply_hz(&self) -> u64 {
        let i = self.interrogation_hz();
        let up = match self.band {
            Band::X => self.number >= 64,
            Band::Y => self.number <= 63,
        };
        if up {
            i + 63_000_000
        } else {
            i - 63_000_000
        }
    }

    /// Pulse-pair spacing on the given side, µs.
    pub fn spacing_us(&self, role: Role) -> u32 {
        match (self.band, role) {
            (Band::X, _) => 12,
            (Band::Y, Role::Interrogation) => 36,
            (Band::Y, Role::Reply) => 30,
        }
    }

    /// Transponder reply delay, µs.
    pub fn reply_delay_us(&self) -> u32 {
        match self.band {
            Band::X => 50,
            Band::Y => 56,
        }
    }

    /// Paired VHF (VOR or ILS localizer) frequency, Hz.
    pub fn paired_vhf_hz(&self) -> Option<u64> {
        let n = self.number as u64;
        // In 50 kHz units from 108.00 MHz.
        let steps = match n {
            17..=59 => (n - 17) * 2,
            70..=126 => 86 + (n - 70) * 2, // 112.30 MHz = 108.00 + 86 × 50 kHz
            _ => return None,
        };
        let y = matches!(self.band, Band::Y) as u64;
        Some(108_000_000 + (steps + y) * 50_000)
    }

    /// Whether the paired VHF frequency is a VOR or an ILS localizer.
    pub fn paired_kind(&self) -> Option<PairedKind> {
        self.paired_vhf_hz().map(vhf_kind)
    }

    /// The channel paired with a VOR / localizer frequency (Hz, ±10 kHz).
    pub fn from_vhf(freq_hz: u64) -> Option<Self> {
        DmeChannel::all().find(|c| {
            c.paired_vhf_hz()
                .is_some_and(|f| f.abs_diff(freq_hz) <= 10_000)
        })
    }

    /// Every (channel, role) whose frequency is within `tol_hz` of `freq_hz`.
    /// One RF frequency can carry a Y reply and both interrogations (1025 MHz
    /// is the 64Y reply and the 1X and 1Y interrogations); the pulse spacing
    /// (30, 12, 36 µs) tells them apart.
    pub fn at_frequency(freq_hz: u64, tol_hz: u64) -> Vec<(DmeChannel, Role)> {
        let mut out = Vec::new();
        for c in DmeChannel::all() {
            if c.reply_hz().abs_diff(freq_hz) <= tol_hz {
                out.push((c, Role::Reply));
            }
            if c.interrogation_hz().abs_diff(freq_hz) <= tol_hz {
                out.push((c, Role::Interrogation));
            }
        }
        out
    }
}

/// VOR or ILS for a VHF navaid frequency.
pub fn vhf_kind(freq_hz: u64) -> PairedKind {
    // 108.00–111.95 MHz with an odd tenth digit is a localizer.
    let khz = freq_hz / 1_000;
    let tenth = (khz / 100) % 10;
    if (108_000..112_000).contains(&khz) && tenth % 2 == 1 {
        PairedKind::Ils
    } else {
        PairedKind::Vor
    }
}

impl fmt::Display for DmeChannel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}{}",
            self.number,
            if self.band == Band::X { 'X' } else { 'Y' }
        )
    }
}

impl std::str::FromStr for DmeChannel {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let s = s.trim().to_ascii_uppercase();
        let (num, band) = if let Some(n) = s.strip_suffix('X') {
            (n, Band::X)
        } else if let Some(n) = s.strip_suffix('Y') {
            (n, Band::Y)
        } else {
            return Err(format!("not a DME channel: {s}"));
        };
        let n: u8 = num.parse().map_err(|_| format!("not a DME channel: {s}"))?;
        DmeChannel::new(n, band).ok_or_else(|| format!("DME channel out of range: {s}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ch(s: &str) -> DmeChannel {
        s.parse().unwrap()
    }

    #[test]
    fn rejects_bad_names_without_panicking() {
        for s in ["", "X", "1é", "é", "64Z", "0X", "127Y", "-1X", "６４X"] {
            assert!(s.parse::<DmeChannel>().is_err(), "{s}");
        }
        assert_eq!(ch(" 64y ").to_string(), "64Y");
    }

    #[test]
    fn reply_frequencies_match_annex_10() {
        assert_eq!(ch("1X").reply_hz(), 962_000_000);
        assert_eq!(ch("63X").reply_hz(), 1_024_000_000);
        assert_eq!(ch("64X").reply_hz(), 1_151_000_000);
        assert_eq!(ch("126X").reply_hz(), 1_213_000_000);
        assert_eq!(ch("1Y").reply_hz(), 1_088_000_000);
        assert_eq!(ch("63Y").reply_hz(), 1_150_000_000);
        assert_eq!(ch("64Y").reply_hz(), 1_025_000_000);
        assert_eq!(ch("126Y").reply_hz(), 1_087_000_000);
        assert_eq!(ch("1X").interrogation_hz(), 1_025_000_000);
        assert_eq!(ch("126Y").interrogation_hz(), 1_150_000_000);
    }

    #[test]
    fn vhf_pairing() {
        assert_eq!(ch("17X").paired_vhf_hz(), Some(108_000_000));
        assert_eq!(ch("17Y").paired_vhf_hz(), Some(108_050_000));
        assert_eq!(ch("18X").paired_vhf_hz(), Some(108_100_000));
        assert_eq!(ch("59X").paired_vhf_hz(), Some(112_200_000));
        assert_eq!(ch("59Y").paired_vhf_hz(), Some(112_250_000));
        assert_eq!(ch("70X").paired_vhf_hz(), Some(112_300_000));
        assert_eq!(ch("126X").paired_vhf_hz(), Some(117_900_000));
        assert_eq!(ch("126Y").paired_vhf_hz(), Some(117_950_000));
        assert_eq!(ch("16X").paired_vhf_hz(), None);
        assert_eq!(ch("65X").paired_vhf_hz(), None);
        // 109.90 MHz (ILS) ↔ 36X; 113.00 MHz (VOR) ↔ 77X.
        assert_eq!(DmeChannel::from_vhf(109_900_000), Some(ch("36X")));
        assert_eq!(ch("36X").paired_kind(), Some(PairedKind::Ils));
        assert_eq!(DmeChannel::from_vhf(113_000_000), Some(ch("77X")));
        assert_eq!(ch("77X").paired_kind(), Some(PairedKind::Vor));
        assert_eq!(vhf_kind(108_150_000), PairedKind::Ils);
        assert_eq!(vhf_kind(108_200_000), PairedKind::Vor);
        assert_eq!(vhf_kind(112_100_000), PairedKind::Vor);
    }

    #[test]
    fn shared_frequency_is_reply_and_interrogation() {
        let hits = DmeChannel::at_frequency(1_025_000_000, 0);
        assert!(hits.contains(&(ch("64Y"), Role::Reply)));
        assert!(hits.contains(&(ch("1X"), Role::Interrogation)));
        assert!(hits.contains(&(ch("1Y"), Role::Interrogation)));
        assert_eq!(hits.len(), 3);
    }

    #[test]
    fn spacings() {
        assert_eq!(ch("20X").spacing_us(Role::Reply), 12);
        assert_eq!(ch("20Y").spacing_us(Role::Reply), 30);
        assert_eq!(ch("20Y").spacing_us(Role::Interrogation), 36);
        assert_eq!(ch("20Y").to_string(), "20Y");
    }
}
