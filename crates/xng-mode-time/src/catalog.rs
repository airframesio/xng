//! Multi-band time-signal station catalog and capability auto-scan.
//!
//! A static table of the world's standard-frequency / time-signal stations
//! with their carrier frequencies, band class (LF / HF), modulation family,
//! and the decoder this crate can run against each. [`receivable`] is the
//! entry point `xng scan` / `--mode time` use to pick channels from an SDR's
//! tunable range.
//!
//! Every fact here (carriers, modulation family) is anchored to a published
//! source — see PROVENANCE.md. Carrier frequencies are stored in Hz.

/// RF band class of a time station.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Band {
    /// Low frequency (< 300 kHz). Longwave time stations (WWVB, DCF77, MSF,
    /// JJY, TDF, RBU) carry their time code by carrier amplitude/phase
    /// modulation; need a longwave-capable front end.
    Lf,
    /// High frequency (3–30 MHz). Shortwave standard-frequency stations (WWV,
    /// WWVH, CHU, BPM, RWM, YVTO) — receivable on any HF SDR.
    Hf,
}

impl Band {
    /// Classify a carrier frequency into its band.
    pub fn of(hz: u64) -> Band {
        if hz < 300_000 {
            Band::Lf
        } else {
            Band::Hf
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Band::Lf => "lf",
            Band::Hf => "hf",
        }
    }
}

/// Modulation family carried by a station — what a decoder must demodulate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Modulation {
    /// AM voice + 1000/1200 Hz seconds tick + a 100 Hz subcarrier BCD time
    /// code (WWV/WWVH, modified IRIG-H). Fully decodable here.
    AmSubcarrierBcd,
    /// AM carrier + audio AFSK digital time code in seconds 31–39
    /// (CHU, Bell-103 300 baud). Fully decodable here.
    AmAfsk,
    /// LF carrier amplitude/phase shift keying carrying a 1 bit/second time
    /// code (WWVB pulse-width; DCF77/MSF/JJY pulse-width/phase). Catalog-only
    /// for the LF stations until an LF capture path exists.
    LfPulseWidth,
    /// HF carrier + seconds ticks, no broadcast digital time code we decode
    /// (BPM tone schedule, RWM A1/A2 ticks, YVTO tone). Carrier+tone only.
    CarrierTone,
}

impl Modulation {
    pub fn as_str(self) -> &'static str {
        match self {
            Modulation::AmSubcarrierBcd => "am-subcarrier-bcd",
            Modulation::AmAfsk => "am-afsk",
            Modulation::LfPulseWidth => "lf-pulse-width",
            Modulation::CarrierTone => "carrier-tone",
        }
    }
}

/// What this crate can actually do with a station's signal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Capability {
    /// A full time/date decoder is implemented (CHU, WWV/WWVH).
    Decode,
    /// The waveform is understood and catalogued but no decoder is wired yet
    /// (the LF BCD/phase stations — documented follow-up).
    CatalogOnly,
}

impl Capability {
    pub fn as_str(self) -> &'static str {
        match self {
            Capability::Decode => "decode",
            Capability::CatalogOnly => "catalog-only",
        }
    }
}

/// Which decoder family handles a station (used by the runtime to pick the
/// audio decode path once a carrier is AM-demodulated).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decoder {
    /// CHU AFSK (Bell-103) packet decoder ([`crate::chu`]).
    Chu,
    /// WWV/WWVH 100 Hz subcarrier BCD decoder ([`crate::wwv`]).
    Wwv,
    /// No decoder (catalog/carrier-tone only).
    None,
}

/// One time-signal station entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Station {
    /// Short call sign / identifier (e.g. "WWV", "CHU", "DCF77").
    pub name: &'static str,
    /// Operator / location, for display.
    pub location: &'static str,
    /// Broadcast carrier frequencies, Hz. Many stations transmit the same
    /// time code on several HF carriers simultaneously.
    pub carriers: &'static [u64],
    /// Modulation family.
    pub modulation: Modulation,
    /// Whether this crate can decode it or only catalog it.
    pub capability: Capability,
    /// The decoder to run after AM-demod (when `capability == Decode`).
    pub decoder: Decoder,
}

impl Station {
    /// Band class is derived from the lowest carrier (a station is in one
    /// band; CHU/WWV carriers are all HF, WWVB/DCF77/etc. all LF).
    pub fn band(&self) -> Band {
        Band::of(self.carriers[0])
    }

    /// Rank for auto-scan ordering (lower = preferred). Decodable digital/BCD
    /// stations first, then carrier+tone, then catalog-only.
    fn rank(&self) -> u8 {
        match (self.capability, self.modulation) {
            // The two fully decodable digital/BCD families lead.
            (Capability::Decode, Modulation::AmSubcarrierBcd) => 0,
            (Capability::Decode, Modulation::AmAfsk) => 0,
            (Capability::Decode, _) => 1,
            // Carrier+tone stations we can detect/label but not time-decode.
            (Capability::CatalogOnly, Modulation::CarrierTone) => 2,
            // LF pulse-width stations — documented follow-up.
            (Capability::CatalogOnly, _) => 3,
        }
    }
}

/// The full multi-band catalog. Carrier values and modulation families are
/// the published broadcast facts (PROVENANCE.md); the `capability`/`decoder`
/// columns reflect what this crate implements.
pub const CATALOG: &[Station] = &[
    // ---- HF standard-frequency stations (fully or partly decodable) ----
    Station {
        name: "WWV",
        location: "NIST, Fort Collins, Colorado, USA",
        carriers: &[2_500_000, 5_000_000, 10_000_000, 15_000_000, 20_000_000],
        modulation: Modulation::AmSubcarrierBcd,
        capability: Capability::Decode,
        decoder: Decoder::Wwv,
    },
    Station {
        name: "WWVH",
        location: "NIST, Kekaha, Kauai, Hawaii, USA",
        carriers: &[2_500_000, 5_000_000, 10_000_000, 15_000_000],
        modulation: Modulation::AmSubcarrierBcd,
        capability: Capability::Decode,
        decoder: Decoder::Wwv,
    },
    Station {
        name: "CHU",
        location: "NRC, Ottawa, Ontario, Canada",
        carriers: &[3_330_000, 7_850_000, 14_670_000],
        modulation: Modulation::AmAfsk,
        capability: Capability::Decode,
        decoder: Decoder::Chu,
    },
    Station {
        name: "BPM",
        location: "NTSC, Pucheng, Shaanxi, China",
        carriers: &[2_500_000, 5_000_000, 10_000_000, 15_000_000],
        modulation: Modulation::CarrierTone,
        capability: Capability::CatalogOnly,
        decoder: Decoder::None,
    },
    Station {
        name: "RWM",
        location: "Russian time service, Taldom, Moscow",
        carriers: &[4_996_000, 9_996_000, 14_996_000],
        modulation: Modulation::CarrierTone,
        capability: Capability::CatalogOnly,
        decoder: Decoder::None,
    },
    Station {
        name: "YVTO",
        location: "Observatorio Cagigal, Caracas, Venezuela",
        carriers: &[5_000_000],
        modulation: Modulation::CarrierTone,
        capability: Capability::CatalogOnly,
        decoder: Decoder::None,
    },
    // ---- LF longwave time stations (catalog-only follow-up) ----
    Station {
        name: "WWVB",
        location: "NIST, Fort Collins, Colorado, USA",
        carriers: &[60_000],
        modulation: Modulation::LfPulseWidth,
        capability: Capability::CatalogOnly,
        decoder: Decoder::None,
    },
    Station {
        name: "DCF77",
        location: "PTB, Mainflingen, Germany",
        carriers: &[77_500],
        modulation: Modulation::LfPulseWidth,
        capability: Capability::CatalogOnly,
        decoder: Decoder::None,
    },
    Station {
        name: "MSF",
        location: "NPL, Anthorn, United Kingdom",
        carriers: &[60_000],
        modulation: Modulation::LfPulseWidth,
        capability: Capability::CatalogOnly,
        decoder: Decoder::None,
    },
    Station {
        name: "JJY",
        location: "NICT, Japan (Ohtakadoya-yama 40 kHz / Hagane-yama 60 kHz)",
        carriers: &[40_000, 60_000],
        modulation: Modulation::LfPulseWidth,
        capability: Capability::CatalogOnly,
        decoder: Decoder::None,
    },
    Station {
        name: "TDF",
        location: "France Inter, Allouis, France",
        carriers: &[162_000],
        modulation: Modulation::LfPulseWidth,
        capability: Capability::CatalogOnly,
        decoder: Decoder::None,
    },
    Station {
        name: "RBU",
        location: "Russian time service, Taldom, Moscow",
        carriers: &[66_660],
        modulation: Modulation::LfPulseWidth,
        capability: Capability::CatalogOnly,
        decoder: Decoder::None,
    },
];

/// A station carrier that falls within a tunable range: which station, which
/// carrier, and the station's auto-scan rank (lower = preferred).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Receivable {
    pub station: &'static Station,
    /// The specific carrier (Hz) within the tunable range.
    pub carrier_hz: u64,
}

/// Stations whose carriers fall in `[lo_hz, hi_hz]`, ranked: fully decodable
/// digital/BCD stations first (CHU AFSK, WWV/WWVH BCD), then carrier+tone HF
/// stations, then catalog-only LF stations. Within a rank, ordered by carrier
/// frequency. One [`Receivable`] is returned per in-range carrier (a station
/// with several in-range carriers appears multiple times — the highest-rank
/// pick wins per station in [`best_per_station`]).
pub fn receivable(lo_hz: u64, hi_hz: u64) -> Vec<Receivable> {
    let (lo, hi) = if lo_hz <= hi_hz { (lo_hz, hi_hz) } else { (hi_hz, lo_hz) };
    let mut out: Vec<Receivable> = Vec::new();
    for st in CATALOG {
        for &c in st.carriers {
            if c >= lo && c <= hi {
                out.push(Receivable { station: st, carrier_hz: c });
            }
        }
    }
    out.sort_by(|a, b| {
        a.station
            .rank()
            .cmp(&b.station.rank())
            .then(a.carrier_hz.cmp(&b.carrier_hz))
            .then(a.station.name.cmp(b.station.name))
    });
    out
}

/// One best carrier per receivable station (the lowest-rank, then lowest
/// in-range carrier), in rank order. This is what a channel planner wants:
/// at most one channel per station so the SDR isn't asked to watch five WWV
/// harmonics of the same time code at once.
pub fn best_per_station(lo_hz: u64, hi_hz: u64) -> Vec<Receivable> {
    let all = receivable(lo_hz, hi_hz);
    let mut seen: Vec<&str> = Vec::new();
    let mut out = Vec::new();
    for r in all {
        if !seen.contains(&r.station.name) {
            seen.push(r.station.name);
            out.push(r);
        }
    }
    out
}

/// Look up a station entry by name (case-insensitive).
pub fn by_name(name: &str) -> Option<&'static Station> {
    CATALOG.iter().find(|s| s.name.eq_ignore_ascii_case(name))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_has_all_required_stations() {
        for name in [
            "WWV", "WWVH", "CHU", "WWVB", "DCF77", "MSF", "JJY", "BPM", "RWM",
            "TDF", "RBU", "YVTO",
        ] {
            assert!(by_name(name).is_some(), "missing station {name}");
        }
        assert_eq!(CATALOG.len(), 12);
    }

    #[test]
    fn required_carriers_present() {
        assert_eq!(
            by_name("WWV").unwrap().carriers,
            &[2_500_000, 5_000_000, 10_000_000, 15_000_000, 20_000_000]
        );
        assert_eq!(
            by_name("WWVH").unwrap().carriers,
            &[2_500_000, 5_000_000, 10_000_000, 15_000_000]
        );
        assert_eq!(by_name("CHU").unwrap().carriers, &[3_330_000, 7_850_000, 14_670_000]);
        assert_eq!(by_name("WWVB").unwrap().carriers, &[60_000]);
        assert_eq!(by_name("DCF77").unwrap().carriers, &[77_500]);
        assert_eq!(by_name("MSF").unwrap().carriers, &[60_000]);
        assert_eq!(by_name("JJY").unwrap().carriers, &[40_000, 60_000]);
        assert_eq!(
            by_name("BPM").unwrap().carriers,
            &[2_500_000, 5_000_000, 10_000_000, 15_000_000]
        );
        assert_eq!(by_name("RWM").unwrap().carriers, &[4_996_000, 9_996_000, 14_996_000]);
        assert_eq!(by_name("TDF").unwrap().carriers, &[162_000]);
        assert_eq!(by_name("RBU").unwrap().carriers, &[66_660]);
        assert_eq!(by_name("YVTO").unwrap().carriers, &[5_000_000]);
    }

    #[test]
    fn band_classification() {
        assert_eq!(by_name("WWV").unwrap().band(), Band::Hf);
        assert_eq!(by_name("CHU").unwrap().band(), Band::Hf);
        assert_eq!(by_name("WWVB").unwrap().band(), Band::Lf);
        assert_eq!(by_name("DCF77").unwrap().band(), Band::Lf);
        assert_eq!(by_name("TDF").unwrap().band(), Band::Lf);
        assert_eq!(Band::of(60_000), Band::Lf);
        assert_eq!(Band::of(3_330_000), Band::Hf);
    }

    #[test]
    fn receivable_ranks_decodable_first() {
        // A broad HF window (2.4–16 MHz) catches WWV/WWVH/CHU (decode) plus
        // BPM/RWM (carrier-tone) plus YVTO. Decodable stations rank ahead.
        let r = best_per_station(2_400_000, 16_000_000);
        let names: Vec<&str> = r.iter().map(|x| x.station.name).collect();
        // The first three slots must all be Decode-capable.
        for top in &r[..3] {
            assert_eq!(top.station.capability, Capability::Decode, "{names:?}");
        }
        // CHU/WWV/WWVH all present.
        assert!(names.contains(&"CHU"));
        assert!(names.contains(&"WWV"));
        assert!(names.contains(&"WWVH"));
        // Carrier-tone-only stations are also surfaced, but ranked lower.
        assert!(names.contains(&"BPM"));
    }

    #[test]
    fn receivable_picks_carriers_in_range() {
        // Narrow window around CHU 7850 kHz only.
        let r = receivable(7_800_000, 7_900_000);
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].station.name, "CHU");
        assert_eq!(r[0].carrier_hz, 7_850_000);
    }

    #[test]
    fn receivable_lf_window_returns_lf_stations() {
        // A 40–80 kHz LF window catches JJY(40/60), WWVB/MSF(60), DCF77(77.5),
        // RBU(66.66). All catalog-only.
        let r = best_per_station(40_000, 80_000);
        let names: Vec<&str> = r.iter().map(|x| x.station.name).collect();
        for st in &r {
            assert_eq!(st.station.capability, Capability::CatalogOnly);
            assert_eq!(st.station.band(), Band::Lf);
        }
        assert!(names.contains(&"DCF77"));
        assert!(names.contains(&"WWVB"));
        assert!(names.contains(&"JJY"));
        assert!(names.contains(&"RBU"));
    }

    #[test]
    fn receivable_empty_outside_any_carrier() {
        assert!(receivable(800_000, 900_000).is_empty());
    }

    #[test]
    fn best_per_station_dedups_multi_carrier() {
        // WWV has five HF carriers; a window covering all of them yields one
        // WWV entry from best_per_station (its lowest in-range carrier).
        let r = best_per_station(2_000_000, 21_000_000);
        let wwv: Vec<_> = r.iter().filter(|x| x.station.name == "WWV").collect();
        assert_eq!(wwv.len(), 1);
        assert_eq!(wwv[0].carrier_hz, 2_500_000);
    }
}
