//! DSC message / frame decoder.
//!
//! Turns a recovered symbol sequence (CCIR 493 symbol values, with [`ERASURE`]
//! for unrecoverable characters) into a structured [`DscMessage`]: the format
//! specifier, addressed and self-identification MMSIs, category, telecommands,
//! distress nature/position/time, frequency or working channel, and the
//! end-of-sequence character, plus the recomputed error-check character (ECC)
//! status.
//!
//! Field offsets and semantics follow ITU-R M.493 and are pinned to the
//! external reference vectors documented in PROVENANCE.md.

use crate::symbol::ERASURE;
use serde::{Deserialize, Serialize};

/// Format specifier (the leading symbol, sent twice).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Format {
    /// Symbol 112 — distress alert.
    DistressAlert,
    /// Symbol 116 — all-ships call.
    AllShipsCall,
    /// Symbol 114 — selective call to a group of ships.
    GroupCall,
    /// Symbol 120 — selective call to an individual station.
    IndividualStationCall,
    /// Symbol 102 — selective call to a geographic area.
    GeographicAreaGroupCall,
    /// Symbol 123 — automatic service call.
    AutomaticServiceCall,
    /// Unrecognised / unrecoverable format specifier.
    Unknown,
}

impl Format {
    pub fn from_symbol(s: i32) -> Format {
        match s {
            112 => Format::DistressAlert,
            116 => Format::AllShipsCall,
            114 => Format::GroupCall,
            120 => Format::IndividualStationCall,
            102 => Format::GeographicAreaGroupCall,
            123 => Format::AutomaticServiceCall,
            _ => Format::Unknown,
        }
    }
}

/// Category of call.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Category {
    Routine,
    Safety,
    Urgency,
    Distress,
    Unknown,
}

impl Category {
    pub fn from_symbol(s: i32) -> Category {
        match s {
            100 => Category::Routine,
            108 => Category::Safety,
            110 => Category::Urgency,
            112 => Category::Distress,
            _ => Category::Unknown,
        }
    }
}

/// Nature of distress (distress-alert format only).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NatureOfDistress {
    FireExplosion,
    Flooding,
    Collision,
    Grounding,
    ListingInDangerOfCapsizing,
    Sinking,
    DisabledAndAdrift,
    UndesignatedDistress,
    AbandoningShip,
    PiracyArmedRobberyAttack,
    ManOverboard,
    Unknown,
}

impl NatureOfDistress {
    pub fn from_symbol(s: i32) -> NatureOfDistress {
        match s {
            100 => NatureOfDistress::FireExplosion,
            101 => NatureOfDistress::Flooding,
            102 => NatureOfDistress::Collision,
            103 => NatureOfDistress::Grounding,
            104 => NatureOfDistress::ListingInDangerOfCapsizing,
            105 => NatureOfDistress::Sinking,
            106 => NatureOfDistress::DisabledAndAdrift,
            107 => NatureOfDistress::UndesignatedDistress,
            108 => NatureOfDistress::AbandoningShip,
            109 => NatureOfDistress::PiracyArmedRobberyAttack,
            110 => NatureOfDistress::ManOverboard,
            _ => NatureOfDistress::Unknown,
        }
    }
}

/// First telecommand.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FirstCommand {
    AllModesTp,
    DuplexTp,
    Polling,
    UnableToComply,
    EndOfCall,
    Data,
    J3eTp,
    DistressAcknowledgement,
    DistressAlertRelay,
    TtyFec,
    TtyArq,
    Test,
    ShipPositionOrLocationRegistrationUpdating,
    NoInformation,
    Unknown,
}

impl FirstCommand {
    pub fn from_symbol(s: i32) -> FirstCommand {
        match s {
            100 => FirstCommand::AllModesTp,
            101 => FirstCommand::DuplexTp,
            103 => FirstCommand::Polling,
            104 => FirstCommand::UnableToComply,
            105 => FirstCommand::EndOfCall,
            106 => FirstCommand::Data,
            109 => FirstCommand::J3eTp,
            110 => FirstCommand::DistressAcknowledgement,
            112 => FirstCommand::DistressAlertRelay,
            113 => FirstCommand::TtyFec,
            115 => FirstCommand::TtyArq,
            118 => FirstCommand::Test,
            121 => FirstCommand::ShipPositionOrLocationRegistrationUpdating,
            126 => FirstCommand::NoInformation,
            _ => FirstCommand::Unknown,
        }
    }
}

/// Second telecommand.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SecondCommand {
    NoReasonGiven,
    CongestionAtMaritimeSwitchingCentre,
    Busy,
    QueueIndication,
    StationBarred,
    NoOperatorAvailable,
    OperatorTemporarilyUnavailable,
    EquipmentDisabled,
    UnableToUseProposedChannel,
    UnableToUseProposedMode,
    ShipsAndAircraftOfStatesNotPartiesToAnArmedConflict,
    MedicalTransports,
    PayPhonePublicCallOffice,
    FacsimileData,
    NoRemainingAcsSequentialTransmission,
    OneTimeRemainingAcsSequentialTransmission,
    TwoTimesRemainingAcsSequentialTransmission,
    ThreeTimesRemainingAcsSequentialTransmission,
    FourTimesRemainingAcsSequentialTransmission,
    FiveTimesRemainingAcsSequentialTransmission,
    NoInformation,
    Unknown,
}

impl SecondCommand {
    pub fn from_symbol(s: i32) -> SecondCommand {
        match s {
            100 => SecondCommand::NoReasonGiven,
            101 => SecondCommand::CongestionAtMaritimeSwitchingCentre,
            102 => SecondCommand::Busy,
            103 => SecondCommand::QueueIndication,
            104 => SecondCommand::StationBarred,
            105 => SecondCommand::NoOperatorAvailable,
            106 => SecondCommand::OperatorTemporarilyUnavailable,
            107 => SecondCommand::EquipmentDisabled,
            108 => SecondCommand::UnableToUseProposedChannel,
            109 => SecondCommand::UnableToUseProposedMode,
            110 => SecondCommand::ShipsAndAircraftOfStatesNotPartiesToAnArmedConflict,
            111 => SecondCommand::MedicalTransports,
            112 => SecondCommand::PayPhonePublicCallOffice,
            113 => SecondCommand::FacsimileData,
            120 => SecondCommand::NoRemainingAcsSequentialTransmission,
            121 => SecondCommand::OneTimeRemainingAcsSequentialTransmission,
            122 => SecondCommand::TwoTimesRemainingAcsSequentialTransmission,
            123 => SecondCommand::ThreeTimesRemainingAcsSequentialTransmission,
            124 => SecondCommand::FourTimesRemainingAcsSequentialTransmission,
            125 => SecondCommand::FiveTimesRemainingAcsSequentialTransmission,
            126 => SecondCommand::NoInformation,
            _ => SecondCommand::Unknown,
        }
    }
}

/// End-of-sequence character.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EndOfSequence {
    /// Symbol 117 — Acknowledge RQ (call requires acknowledgement).
    AcknowledgeRq,
    /// Symbol 122 — Acknowledge BQ (answer to a call requiring ack).
    AcknowledgeBq,
    /// Symbol 127 — all other calls.
    OtherCalls,
    Unknown,
}

impl EndOfSequence {
    pub fn from_symbol(s: i32) -> EndOfSequence {
        match s {
            117 => EndOfSequence::AcknowledgeRq,
            122 => EndOfSequence::AcknowledgeBq,
            127 => EndOfSequence::OtherCalls,
            _ => EndOfSequence::Unknown,
        }
    }
}

/// A decoded DSC sequence.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DscMessage {
    /// Recovered symbol stream this decode was produced from.
    pub symbols: Vec<i32>,
    pub format: Format,
    pub category: Category,
    /// Addressed party (MMSI digits, "ALL SHIPS", or an area description).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub to: Option<String>,
    /// Self-identification MMSI of the sender.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tc1: Option<FirstCommand>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tc2: Option<SecondCommand>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nature: Option<NatureOfDistress>,
    /// Free-text expansion (e.g. "Position Requested") when no position field.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nature_description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub position: Option<String>,
    /// UTC time of day as "HH:MM" (distress alerts).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub time: Option<String>,
    /// Decoded frequency pair or working-channel description.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub frequency: Option<String>,
    pub eos: EndOfSequence,
    /// Received error-check character value (-1 when unrecoverable).
    pub ecc: i32,
    /// "OK" when the recomputed ECC matched, else "Error".
    pub status: String,
}

impl DscMessage {
    /// Serializes to a compact JSON object.
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).expect("DscMessage serializes")
    }
}

/// Decodes a recovered symbol stream into a [`DscMessage`].
///
/// The first symbol is the format specifier (the leading symbol is sent
/// twice; if the first copy is an erasure the second is used). Field layout
/// then depends on the format. Erasures ([`ERASURE`]) in addressing/position
/// fields are surfaced as `_` placeholders rather than dropped.
pub fn decode(symbols: &[i32]) -> DscMessage {
    let fmt_sym = match symbols.first().copied() {
        Some(s) if s != ERASURE => s,
        _ => symbols.get(1).copied().unwrap_or(ERASURE),
    };
    let format = Format::from_symbol(fmt_sym);
    match format {
        Format::DistressAlert => decode_distress(symbols),
        Format::AllShipsCall => decode_all_ships(symbols),
        Format::IndividualStationCall => decode_individual(symbols),
        Format::GeographicAreaGroupCall => decode_area(symbols),
        Format::GroupCall | Format::AutomaticServiceCall | Format::Unknown => {
            // Not-yet-implemented or unrecognised: surface the format and any
            // recoverable address rather than fabricating fields.
            let mut msg = blank(symbols, format);
            if matches!(format, Format::GroupCall | Format::AutomaticServiceCall) {
                msg.from = extract_mmsi(symbols, 2);
            }
            msg.status = "Unsupported".into();
            msg
        }
    }
}

fn blank(symbols: &[i32], format: Format) -> DscMessage {
    DscMessage {
        symbols: symbols.to_vec(),
        format,
        category: Category::Unknown,
        to: None,
        from: None,
        tc1: None,
        tc2: None,
        nature: None,
        nature_description: None,
        position: None,
        time: None,
        frequency: None,
        eos: EndOfSequence::Unknown,
        ecc: ERASURE,
        status: String::new(),
    }
}

// --- per-format decoders -------------------------------------------------

fn decode_distress(symbols: &[i32]) -> DscMessage {
    let from = extract_mmsi(symbols, 2);
    let nature = at(symbols, 7).map(NatureOfDistress::from_symbol);
    let position = extract_position(symbols, 8);
    let time = extract_time(symbols, 13);
    let eos = extract_eos(symbols, 16);
    let ecc = at(symbols, 17).unwrap_or(ERASURE);
    let status = ecc_status(symbols, 17);
    DscMessage {
        symbols: symbols.to_vec(),
        format: Format::DistressAlert,
        category: Category::Distress,
        to: Some("ALL SHIPS".into()),
        from,
        tc1: None,
        tc2: None,
        nature,
        nature_description: None,
        position,
        time,
        frequency: None,
        eos,
        ecc,
        status,
    }
}

fn decode_all_ships(symbols: &[i32]) -> DscMessage {
    let category = at(symbols, 2).map(Category::from_symbol).unwrap_or(Category::Unknown);
    let from = extract_mmsi(symbols, 3);
    let tc1 = at(symbols, 8).map(FirstCommand::from_symbol);
    let tc2 = at(symbols, 9).map(SecondCommand::from_symbol);
    let frequency = if tc1 == Some(FirstCommand::J3eTp) {
        extract_frequencies(symbols, 10)
    } else {
        None
    };
    let eos = extract_eos(symbols, 16);
    let ecc = at(symbols, 17).unwrap_or(ERASURE);
    let status = ecc_status(symbols, 17);
    DscMessage {
        symbols: symbols.to_vec(),
        format: Format::AllShipsCall,
        category,
        to: Some("ALL SHIPS".into()),
        from,
        tc1,
        tc2,
        nature: None,
        nature_description: None,
        position: None,
        time: None,
        frequency,
        eos,
        ecc,
        status,
    }
}

fn decode_individual(symbols: &[i32]) -> DscMessage {
    let to = extract_mmsi(symbols, 2);
    let category = at(symbols, 7).map(Category::from_symbol).unwrap_or(Category::Unknown);
    let from = extract_mmsi(symbols, 8);
    let tc1 = at(symbols, 13).map(FirstCommand::from_symbol);
    let tc2 = at(symbols, 14).map(SecondCommand::from_symbol);

    let mut frequency = None;
    let mut position = None;
    let mut nature_description = None;
    match at(symbols, 15) {
        Some(55) => position = extract_position(symbols, 16),
        Some(126) => nature_description = Some("Position Requested".into()),
        _ => frequency = extract_frequencies(symbols, 15),
    }

    let eos = extract_eos(symbols, 21);
    let ecc = at(symbols, 22).unwrap_or(ERASURE);
    let status = ecc_status(symbols, 22);
    DscMessage {
        symbols: symbols.to_vec(),
        format: Format::IndividualStationCall,
        category,
        to,
        from,
        tc1,
        tc2,
        nature: None,
        nature_description,
        position,
        time: None,
        frequency,
        eos,
        ecc,
        status,
    }
}

fn decode_area(symbols: &[i32]) -> DscMessage {
    let to = extract_geographic_area(symbols, 2);
    let category = at(symbols, 7).map(Category::from_symbol).unwrap_or(Category::Unknown);
    let from = extract_mmsi(symbols, 8);
    let tc1 = at(symbols, 13).map(FirstCommand::from_symbol);
    let tc2 = at(symbols, 14).map(SecondCommand::from_symbol);
    let frequency = if tc1 == Some(FirstCommand::J3eTp) {
        extract_frequencies(symbols, 15)
    } else {
        None
    };
    let eos = extract_eos(symbols, 21);
    let ecc = at(symbols, 22).unwrap_or(ERASURE);
    let status = ecc_status(symbols, 22);
    DscMessage {
        symbols: symbols.to_vec(),
        format: Format::GeographicAreaGroupCall,
        category,
        to,
        from,
        tc1,
        tc2,
        nature: None,
        nature_description: None,
        position: None,
        time: None,
        frequency,
        eos,
        ecc,
        status,
    }
}

// --- field extractors ----------------------------------------------------

/// Reads the symbol at `idx`, returning `None` if out of range.
fn at(symbols: &[i32], idx: usize) -> Option<i32> {
    symbols.get(idx).copied()
}

/// Builds an MMSI string from 5 symbols starting at `start`. Each symbol is
/// two decimal digits; the 10th digit (last half of the 5th symbol) is a
/// trailing filler and is dropped. Erased symbols become `__`.
fn extract_mmsi(symbols: &[i32], start: usize) -> Option<String> {
    if start + 5 > symbols.len() {
        return None;
    }
    let mut s = String::with_capacity(10);
    for &sym in &symbols[start..start + 5] {
        if sym == ERASURE {
            s.push_str("__");
        } else {
            s.push_str(&format!("{sym:02}"));
        }
    }
    s.pop(); // drop the trailing 10th digit
    Some(s)
}

/// Decodes a 10-digit position field (5 symbols) into "QQ QQX QQQ QQX" form:
/// quadrant digit, latitude (deg min), longitude (deg min), with N/S and E/W
/// suffixes. Any erasure yields the `--error--` sentinel.
fn extract_position(symbols: &[i32], start: usize) -> Option<String> {
    if start + 5 > symbols.len() {
        return None;
    }
    let slice = &symbols[start..start + 5];
    if slice.contains(&ERASURE) {
        return Some("--error--".into());
    }
    let digits: String = slice.iter().map(|s| format!("{s:02}")).collect();
    if digits.len() != 10 {
        return Some("--error--".into());
    }
    let quadrant = digits[0..1].parse::<u8>().unwrap_or(9);
    let (ns, ew) = match quadrant {
        0 => ('N', 'E'),
        1 => ('N', 'W'),
        2 => ('S', 'E'),
        3 => ('S', 'W'),
        _ => return Some("--error--".into()),
    };
    let lat = format!("{} {}", &digits[1..3], &digits[3..5]);
    let lon = format!("{} {}", &digits[5..8], &digits[8..10]);
    Some(format!("{lat}{ns} {lon}{ew}"))
}

/// Decodes a geographic-area field (5 symbols): quadrant, reference-point
/// latitude/longitude, and the rectangle's vertical/horizontal extents.
fn extract_geographic_area(symbols: &[i32], start: usize) -> Option<String> {
    if start + 5 > symbols.len() {
        return None;
    }
    let slice = &symbols[start..start + 5];
    if slice.contains(&ERASURE) {
        return Some("--error--".into());
    }
    let digits: String = slice.iter().map(|s| format!("{s:02}")).collect();
    if digits.len() != 10 {
        return Some("--error--".into());
    }
    let quadrant = digits[0..1].parse::<u8>().unwrap_or(9);
    let quadrant_name = match quadrant {
        0 => "North-East (NE)",
        1 => "North-West (NW)",
        2 => "South-East (SE)",
        3 => "South-West (SW)",
        _ => return Some("--error--".into()),
    };
    let lat = digits[1..3].parse::<u32>().ok()?;
    let lon = digits[3..6].parse::<u32>().ok()?;
    let vert = digits[6..8].parse::<u32>().ok()?;
    let horiz = digits[8..10].parse::<u32>().ok()?;
    Some(format!(
        "{quadrant_name}, Reference point: {lat}°, {lon}°, Vertical side: {vert}°, Horizontal side: {horiz}°"
    ))
}

/// Decodes the time-of-day field (2 symbols) as "HH:MM". Returns `None` if
/// either half is erased or out of range.
fn extract_time(symbols: &[i32], start: usize) -> Option<String> {
    let hh = at(symbols, start)?;
    let mm = at(symbols, start + 1)?;
    if hh == ERASURE || mm == ERASURE || hh > 23 || mm > 59 {
        return None;
    }
    Some(format!("{hh:02}:{mm:02}"))
}

/// Decodes the end-of-sequence field. The EOS is sent in DX/RX positions;
/// the reference reads symbol 1, 3 or 4 of the 4-symbol field, taking the
/// first non-erased one.
fn extract_eos(symbols: &[i32], start: usize) -> EndOfSequence {
    let s0 = at(symbols, start).unwrap_or(ERASURE);
    let s2 = at(symbols, start + 2).unwrap_or(ERASURE);
    let s3 = at(symbols, start + 3).unwrap_or(ERASURE);
    let s = if s0 != ERASURE {
        s0
    } else if s2 != ERASURE {
        s2
    } else {
        s3
    };
    EndOfSequence::from_symbol(s)
}

/// Decodes the 6-symbol (12-digit) frequency/channel field. The first digit
/// selects the encoding:
/// - 0/1/2: MF/HF frequency in 100 Hz multiples (one or two frequencies);
/// - 9 followed by 0: a VHF channel pair;
/// - others (3, 4, 8): documented in M.493 but not externally pinned here.
fn extract_frequencies(symbols: &[i32], start: usize) -> Option<String> {
    if start + 6 > symbols.len() {
        return None;
    }
    let slice = &symbols[start..start + 6];
    let digits: String = slice
        .iter()
        .map(|&n| if n == ERASURE { "__".to_string() } else { format!("{n:02}") })
        .collect();
    let d1 = digits.as_bytes().first().copied().map(|b| b as char)?;
    match d1 {
        '0' | '1' | '2' => mf_hf_100hz(slice, &digits),
        '9' => {
            let d2 = digits.as_bytes().get(1).copied().map(|b| b as char);
            if d2 == Some('0') {
                vhf_channels(&digits)
            } else {
                Some("--error--".into())
            }
        }
        // 3 = MF/HF working channel, 4 = 10 Hz multiples, 8 = VHF automated:
        // defined in M.493 but not pinned to an external vector here.
        '3' | '4' | '8' => Some("--not implemented--".into()),
        '_' => Some("--error--".into()),
        _ => Some("--error--".into()),
    }
}

fn mf_hf_100hz(slice: &[i32], digits: &str) -> Option<String> {
    if digits.len() < 12 {
        return Some("--error--".into());
    }
    let f1 = format!("{}.{}", &digits[0..5], &digits[5..6]);
    // A second frequency of 126/126/126 (all symbols > 99) means "same as f1"
    // / not present.
    let f2_present = !slice[3..].iter().all(|&n| n > 99);
    if f2_present {
        let f2 = format!("{}.{}", &digits[6..11], &digits[11..12]);
        Some(format!("{f1}/{f2}"))
    } else {
        Some(f1)
    }
}

fn vhf_channels(digits: &str) -> Option<String> {
    if digits.len() < 12 || digits.contains('_') {
        return Some("--error--".into());
    }
    let b = digits.as_bytes();
    let chan_type = |c: u8| match c as char {
        '1' | '2' => "Simplex channel ",
        '0' => "Duplex channel",
        _ => "Unknown channel type",
    };
    let m1 = format!("{} {}", chan_type(b[1]), &digits[3..6]);
    let m2 = format!("{} {}", chan_type(b[7]), &digits[9..12]);
    Some(format!("{m1} - {m2}"))
}

// --- error-check character (ECC) ----------------------------------------

/// Recomputes the ECC and compares it with the received value, returning
/// "OK" or "Error".
///
/// The 7 information bits of the ECC are the modulo-2 sum (even vertical
/// parity) of the corresponding bits of all information characters. The
/// format specifier appears twice in the stream but only one copy counts;
/// the reference (and this code) treats characters [1..ecc_pos) as the
/// information characters (i.e. the duplicate leading format specifier and
/// the ECC itself are excluded).
fn ecc_status(symbols: &[i32], ecc_pos: usize) -> String {
    if validate_ecc(symbols, ecc_pos) {
        "OK".into()
    } else {
        "Error".into()
    }
}

fn validate_ecc(symbols: &[i32], ecc_pos: usize) -> bool {
    let ecc = match at(symbols, ecc_pos) {
        Some(e) if e != ERASURE => e,
        _ => return false,
    };
    // Information characters: indices 1..ecc_pos (skip the duplicate leading
    // format specifier and exclude the ECC).
    let end = ecc_pos.min(symbols.len());
    if end < 1 {
        return false;
    }
    let mut parity = [0u8; 7];
    for &ch in &symbols[1..end] {
        if ch == ERASURE {
            return false; // an erased info character makes the ECC unverifiable
        }
        for (bit, p) in parity.iter_mut().enumerate() {
            *p ^= ((ch >> bit) & 1) as u8;
        }
    }
    let mut calc = 0i32;
    for (bit, &p) in parity.iter().enumerate() {
        calc |= (p as i32) << bit;
    }
    (ecc & 0x7f) == calc
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_and_category_symbol_mapping() {
        assert_eq!(Format::from_symbol(112), Format::DistressAlert);
        assert_eq!(Format::from_symbol(116), Format::AllShipsCall);
        assert_eq!(Format::from_symbol(120), Format::IndividualStationCall);
        assert_eq!(Format::from_symbol(102), Format::GeographicAreaGroupCall);
        assert_eq!(Category::from_symbol(108), Category::Safety);
        assert_eq!(EndOfSequence::from_symbol(122), EndOfSequence::AcknowledgeBq);
    }

    #[test]
    fn mmsi_drops_trailing_digit() {
        // symbols 25 58 05 99 70 -> "2558059970" -> drop last -> "255805997"
        let syms = vec![112, 112, 25, 58, 5, 99, 70];
        assert_eq!(extract_mmsi(&syms, 2).as_deref(), Some("255805997"));
    }
}
