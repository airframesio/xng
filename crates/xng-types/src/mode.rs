use serde::{Deserialize, Serialize};
use std::fmt;

/// Decode modes xng knows about. Wave-1 native cores cover everything except
/// Iridium (wave 2); `Extern` marks messages injected by second-class wrapped
/// external decoders.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    /// Plain-old ACARS on VHF (AM/MSK 2400 bd).
    AcarsPoa,
    /// VDL Mode 2 (D8PSK 31.5 kbps, AVLC).
    Vdl2,
    /// HF Data Link (ARINC 635).
    Hfdl,
    /// Inmarsat Classic Aero, L-band (P/R/T channels).
    AeroL,
    /// Inmarsat Classic Aero, C-band feeder bursts.
    AeroC,
    /// Inmarsat STD-C / EGC (SafetyNET, FleetNET).
    StdC,
    /// Maritime AIS (GMSK 9600 bd, ITU-R M.1371).
    Ais,
    /// Mode S / ADS-B on 1090 MHz.
    Adsb,
    /// Iridium (wave 2).
    Iridium,
    /// Message injected via a wrapped external decoder.
    Extern,
}

impl Mode {
    pub fn as_str(&self) -> &'static str {
        match self {
            Mode::AcarsPoa => "acars",
            Mode::Vdl2 => "vdl2",
            Mode::Hfdl => "hfdl",
            Mode::AeroL => "aero-l",
            Mode::AeroC => "aero-c",
            Mode::StdC => "std-c",
            Mode::Ais => "ais",
            Mode::Adsb => "adsb",
            Mode::Iridium => "iridium",
            Mode::Extern => "extern",
        }
    }

    /// All modes with planned native cores, in wave order.
    pub fn native_modes() -> &'static [Mode] {
        &[
            Mode::AcarsPoa,
            Mode::Ais,
            Mode::Adsb,
            Mode::Vdl2,
            Mode::StdC,
            Mode::AeroL,
            Mode::AeroC,
            Mode::Hfdl,
            Mode::Iridium,
        ]
    }
}

impl fmt::Display for Mode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for Mode {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "acars" | "poa" | "acars-poa" => Ok(Mode::AcarsPoa),
            "vdl2" | "vdlm2" | "vdl-m2" => Ok(Mode::Vdl2),
            "hfdl" => Ok(Mode::Hfdl),
            "aero-l" | "aerol" | "aero" => Ok(Mode::AeroL),
            "aero-c" | "aeroc" => Ok(Mode::AeroC),
            "std-c" | "stdc" | "egc" => Ok(Mode::StdC),
            "ais" => Ok(Mode::Ais),
            "adsb" | "ads-b" | "1090" | "modes" => Ok(Mode::Adsb),
            "iridium" | "irdm" => Ok(Mode::Iridium),
            other => Err(format!("unknown mode: {other}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_str() {
        for mode in Mode::native_modes() {
            if *mode == Mode::Extern {
                continue;
            }
            let parsed: Mode = mode.as_str().parse().unwrap();
            assert_eq!(parsed, *mode);
        }
    }

    #[test]
    fn serde_snake_case() {
        assert_eq!(serde_json::to_string(&Mode::AcarsPoa).unwrap(), "\"acars_poa\"");
    }
}
