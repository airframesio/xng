//! COSPAS-SARSAT 406 MHz First-Generation Beacon (FGB) message decoder.
//!
//! Decodes the C/S T.001 short (112-bit) and long (144-bit) distress-beacon
//! messages from their hex form (15 hex = 60 protocol/ID bits; 30 hex = 120
//! transmitted bits, the frame sync prefix already removed) into structured
//! fields:
//!
//! * message type (Standard Location / User / National / Return Link Service /
//!   ELT-DT) and short/long format,
//! * country code (maritime identification digits, MID),
//! * the protocol-specific beacon identification (serial / location / aircraft
//!   protocols),
//! * the 15-hex (short) / 22-hex (long) beacon ID,
//! * the encoded position where present (coarse + offset for Standard Location,
//!   absolute lat/lon for User Location, coarse for Return Link Service),
//! * BCH(21,15) PDF-1 and BCH(12,7) PDF-2 error-correction verification.
//!
//! This is the **message/frame decoder** (hex/bits -> structured fields). A
//! spec-faithful modulator and an IQ demodulator (IQ -> bits) are out of scope
//! for this layer — see PROVENANCE.md and the TODO at the bottom of this file.
//!
//! Second-generation beacons (C/S T.018, SGB) are not decoded here.
//!
//! Verification: the field layout, the two BCH generator polynomials, the
//! bit offsets, and the position arithmetic are ported from the externally
//! published reference decoder `amsa-code/fgb-decoder` (Apache-2.0) and every
//! decode is asserted against that project's compliance-kit oracle vectors and
//! the C/S T.001 worked examples. See PROVENANCE.md.

pub mod bits;

use serde::{Deserialize, Serialize};

use bits::{
    bits_to_hex, bits_to_octal, bits_to_u64, expected_bch1, expected_bch2, hex_to_bits,
    transmitted_bch1, transmitted_bch2,
};

/// Short vs long message, from the C/S T.001 format flag (bit 25) / protocol
/// flag (bit 26). When decoding a 15-hex string the format flag is unknown, so
/// the format is reported as [`Format::Unknown`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Format {
    Short,
    Long,
    Unknown,
}

/// The encoded position carried by a location-protocol beacon, in decimal
/// degrees (north / east positive). Present only when the beacon carries
/// position and it is not the "no position" default pattern.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Position {
    pub latitude: f64,
    pub longitude: f64,
}

/// State of one BCH error-correcting field.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BchField {
    /// Parity bits as transmitted.
    pub transmitted: String,
    /// Parity bits recomputed from the protected data.
    pub computed: String,
    /// Whether the field is internally consistent (no detected error).
    pub ok: bool,
}

impl BchField {
    fn new(transmitted: &str, computed: String) -> Self {
        BchField {
            ok: transmitted == computed,
            transmitted: transmitted.to_string(),
            computed,
        }
    }
}

/// A decoded First-Generation Beacon message.
///
/// Field names mirror the `amsa-code/fgb-decoder` JSON oracle so a decode can
/// be asserted against the published compliance vectors.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SarsatBeacon {
    /// Human-readable message type, e.g. "Standard Location (Long)".
    pub message_type: String,
    /// Short / long / unknown format.
    pub format: Format,
    /// The 15-hex (short) or 22-hex (long) beacon identification.
    pub hex_id: String,
    /// Country code (maritime identification digits, T.001 bits 27-36).
    pub country_code: u16,
    /// Protocol type label, e.g. "ELT - Serial", "PLB - Serial",
    /// "Aircraft Address", "Return Link Service".
    pub protocol_type: String,

    // --- protocol-specific identification (present where applicable) ---
    /// C/S type approval certificate (location ELT/EPIRB/PLB protocols).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cs_type_approval: Option<u32>,
    /// Beacon serial number (location ELT/EPIRB/PLB protocols).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub beacon_serial_number: Option<u32>,
    /// 24-bit ICAO aircraft address, hex (aircraft-address protocols).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub aircraft_24bit_address_hex: Option<String>,
    /// 24-bit ICAO aircraft address, octal.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub aircraft_24bit_address_octal: Option<String>,
    /// Aircraft operator designator (3-char, Baudot; aircraft-operator
    /// protocols).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub aircraft_operator: Option<String>,
    /// Aircraft serial number (aircraft-operator protocols).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub aircraft_serial_number: Option<u32>,
    /// Return Link Service: type-approval certificate (TAC) number.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rls_tac_number: Option<String>,
    /// Return Link Service: beacon serial (within TAC).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rls_id: Option<u32>,

    // --- position ---
    /// Coarse encoded position (Standard Location / RLS), if present.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub coarse_position: Option<Position>,
    /// Best/refined position (offset-corrected for Standard Location, absolute
    /// for User Location), if present.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub position: Option<Position>,

    // --- error correction ---
    /// PDF-1 BCH(21,15), always present.
    pub bch1: BchField,
    /// PDF-2 BCH(12,7), present only on long messages carrying position.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bch2: Option<BchField>,

    /// The full bit string the fields were decoded from (T.001-indexed; the
    /// first 25 bits are sync placeholders). Useful for debugging.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub raw_bits: Option<String>,
}

/// Errors from [`decode_hex`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DecodeError {
    /// The hex string was not 15 or 30 characters, or contained non-hex.
    BadLength,
}

impl std::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DecodeError::BadLength => write!(f, "hex must be 15 or 30 hex characters"),
        }
    }
}

impl std::error::Error for DecodeError {}

/// Decode a First-Generation Beacon from its hex form.
///
/// Accepts 15 hex (short beacon ID) or 30 hex (full long message). The decode
/// is lenient: BCH failures are reported in [`BchField::ok`] rather than
/// rejected, matching how real beacon receivers surface miscoded beacons.
pub fn decode_hex(hex: &str) -> Result<SarsatBeacon, DecodeError> {
    let bits = hex_to_bits(hex).ok_or(DecodeError::BadLength)?;
    let is_long_hex = hex.trim().len() == 30;

    let format = message_format(&bits);
    let country_code = bits_to_u64(&bits[27..37]) as u16;

    // Protocol-flag bit 26: '1' = user protocol family, '0' = location /
    // standard / national family. (Format flag bit 25 = short/long.)
    let user_family = bits.as_bytes()[26] == b'1';

    let proto = classify(&bits, user_family);

    let hex_id = compute_hex_id(&bits, &proto);
    let message_type = message_type_label(&proto, format);

    let mut beacon = SarsatBeacon {
        message_type,
        format,
        hex_id,
        country_code,
        protocol_type: proto.label().to_string(),
        cs_type_approval: None,
        beacon_serial_number: None,
        aircraft_24bit_address_hex: None,
        aircraft_24bit_address_octal: None,
        aircraft_operator: None,
        aircraft_serial_number: None,
        rls_tac_number: None,
        rls_id: None,
        coarse_position: None,
        position: None,
        bch1: BchField::new(
            transmitted_bch1(&bits).unwrap_or(""),
            expected_bch1(&bits),
        ),
        bch2: None,
        raw_bits: Some(bits.clone()),
    };

    fill_identification(&mut beacon, &bits, &proto);
    fill_position(&mut beacon, &bits, &proto, is_long_hex);

    if is_long_hex && format == Format::Long && long_carries_position(hex) {
        beacon.bch2 = Some(BchField::new(
            transmitted_bch2(&bits).unwrap_or(""),
            expected_bch2(&bits),
        ));
    }

    Ok(beacon)
}

/// short/long/unknown from the format flag (bit 25) + protocol flag (bit 26).
fn message_format(bits: &str) -> Format {
    // amsa-code uses bits[25..27] in {00,01}=short, {10,11}=long. For 15-hex,
    // bit 25 is '?' so neither matches -> Unknown.
    match &bits[25..27] {
        "00" | "01" => Format::Short,
        "10" | "11" => Format::Long,
        _ => Format::Unknown,
    }
}

/// Protocol classification. Codes from C/S T.001 / amsa-code/fgb-decoder.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Protocol {
    // Location (standard) family — protocol code bits 37-40.
    StdEltSerial,
    StdEpirbSerial,
    StdPlbSerial,
    StdShipMmsi,
    StdAircraftAddress,
    StdAircraftOperator,
    ReturnLinkService,
    EltDt,
    /// Location protocol whose 4-bit code we recognise structurally but don't
    /// model field-by-field. Carries the label.
    LocationOther(&'static str),
    Orbitography,

    // User family — protocol code bits 37-39.
    UserSerial,
    UserAviation,
    UserMaritime,
    UserRadioCallsign,
    UserOther(&'static str),
}

impl Protocol {
    fn label(&self) -> &'static str {
        match self {
            Protocol::StdEltSerial => "ELT - Serial",
            Protocol::StdEpirbSerial => "EPIRB - Serial",
            Protocol::StdPlbSerial => "PLB - Serial",
            Protocol::StdShipMmsi => "Maritime MMSI",
            Protocol::StdAircraftAddress => "Aircraft Address",
            Protocol::StdAircraftOperator => "Aircraft Operator",
            Protocol::ReturnLinkService => "Return Link Service",
            Protocol::EltDt => "ELT(DT) Location",
            Protocol::LocationOther(s) => s,
            Protocol::Orbitography => "Reserved (orbitography)",
            Protocol::UserSerial => "Serial",
            Protocol::UserAviation => "Aviation",
            Protocol::UserMaritime => "Maritime",
            Protocol::UserRadioCallsign => "Radio Call Sign",
            Protocol::UserOther(s) => s,
        }
    }

    /// Whether this is a (standard/national) *location* protocol, which uses
    /// the default-location hexId substitution and coarse position layout.
    fn is_location(&self) -> bool {
        matches!(
            self,
            Protocol::StdEltSerial
                | Protocol::StdEpirbSerial
                | Protocol::StdPlbSerial
                | Protocol::StdShipMmsi
                | Protocol::StdAircraftAddress
                | Protocol::StdAircraftOperator
                | Protocol::ReturnLinkService
                | Protocol::EltDt
                | Protocol::LocationOther(_)
        )
    }
}

fn classify(bits: &str, user_family: bool) -> Protocol {
    if user_family {
        // User protocol code: bits 37-39 (3 bits, index 37..40).
        match &bits[37..40] {
            "011" => Protocol::UserSerial,
            "001" => Protocol::UserAviation,
            "010" => Protocol::UserMaritime,
            "110" => Protocol::UserRadioCallsign,
            "000" => Protocol::UserOther("Orbitography"),
            "111" => Protocol::UserOther("National"),
            "100" => Protocol::UserOther("Test"),
            _ => Protocol::UserOther("User"),
        }
    } else {
        // Standard/location protocol code: bits 37-40 (4 bits, index 37..41).
        // Code "0000" is the orbitography reservation; the message-type
        // "Location (Format - Unknown)" with idPosition is reported by the
        // oracle for the orbitography reservation.
        match &bits[37..41] {
            "0100" => Protocol::StdEltSerial,
            "0110" => Protocol::StdEpirbSerial,
            "0111" => Protocol::StdPlbSerial,
            "0010" => Protocol::StdShipMmsi,
            "0011" => Protocol::StdAircraftAddress,
            "0101" => Protocol::StdAircraftOperator,
            "1101" => Protocol::ReturnLinkService,
            "1001" => Protocol::EltDt,
            "1000" => Protocol::LocationOther("Ship Security"),
            "1110" => Protocol::LocationOther("National ELT"),
            "1111" => Protocol::LocationOther("Standard Test Location"),
            "0000" => Protocol::Orbitography,
            other => {
                // Distinguish the orbitography reservation more precisely is
                // not needed; report a generic location label.
                let _ = other;
                Protocol::LocationOther("Location")
            }
        }
    }
}

/// Build the beacon ID. For location protocols the C/S "15 Hex ID" substitutes
/// the default-location pattern for bits 66-85 (so the ID is position-
/// independent); for user protocols the ID is bits 26-85 verbatim. See
/// `StandardLocation.hexIdWithDefaultLocation` / `BeaconProtocol.hexId`.
fn compute_hex_id(bits: &str, proto: &Protocol) -> String {
    match proto {
        Protocol::ReturnLinkService => {
            // RLS: bits 26-66 (index 26..67, 41 bits) + 9-bit + 10-bit default
            // location pattern. From ReturnLinkServiceLocation.
            let mut id = bits[26..67].to_string();
            id.push_str("011111111"); // 9-bit default lat field
            id.push_str("0111111111"); // 10-bit default lon field
            bits_to_hex(&id)
        }
        p if p.is_location() => {
            // Standard Location: bits 26-64 (index 26..65, 39 bits) + 10-bit +
            // 11-bit default location pattern (position-independent ID).
            let mut id = bits[26..65].to_string();
            id.push_str("0111111111"); // 10-bit default lat field
            id.push_str("01111111111"); // 11-bit default lon field
            bits_to_hex(&id)
        }
        _ => bits_to_hex(&bits[26..86]),
    }
}

fn message_type_label(proto: &Protocol, format: Format) -> String {
    let fmt = match format {
        Format::Short => " (Short)",
        Format::Long => " (Long)",
        Format::Unknown => " (Format - Unknown)",
    };
    match proto {
        Protocol::ReturnLinkService => "Return Link Service Location".to_string(),
        Protocol::EltDt => "ELT(DT) Location".to_string(),
        Protocol::Orbitography => format!("Location{fmt}"),
        p if p.is_location() => format!("Standard Location{fmt}"),
        _ => format!("User{}", match format {
            Format::Long => " Location (Long)",
            Format::Short => " (Short)",
            Format::Unknown => " (Format - Unknown)",
        }),
    }
}

fn fill_identification(beacon: &mut SarsatBeacon, bits: &str, proto: &Protocol) {
    match proto {
        Protocol::StdEltSerial | Protocol::StdEpirbSerial | Protocol::StdPlbSerial => {
            // C/S type approval bits 41-50, serial bits 51-64.
            beacon.cs_type_approval = Some(bits_to_u64(&bits[41..51]) as u32);
            beacon.beacon_serial_number = Some(bits_to_u64(&bits[51..65]) as u32);
        }
        Protocol::StdAircraftAddress => {
            // 24-bit ICAO address bits 41-64 (index 41..65).
            let addr = &bits[41..65];
            beacon.aircraft_24bit_address_hex = Some(bits_to_hex(addr));
            beacon.aircraft_24bit_address_octal = Some(bits_to_octal(addr));
        }
        Protocol::StdAircraftOperator => {
            // Operator designator: 3 chars * 5-bit Baudot at bits 41-55
            // (index 41..56), aircraft serial bits 56-64 (index 56..65).
            beacon.aircraft_operator = Some(baudot5_decode(&bits[41..56]));
            beacon.aircraft_serial_number = Some(bits_to_u64(&bits[56..65]) as u32);
        }
        Protocol::ReturnLinkService => {
            // RLS TAC (2-bit prefix bits 41-42 + 10-bit value bits 43-52) and
            // RLS id (bits 53-66). Layout from ReturnLinkServiceLocation.
            let (tac, id) = rls_tac_and_id(bits);
            beacon.rls_tac_number = Some(tac);
            beacon.rls_id = Some(id);
        }
        Protocol::UserSerial
        | Protocol::UserAviation
        | Protocol::UserMaritime
        | Protocol::UserRadioCallsign
        | Protocol::UserOther(_)
        | Protocol::EltDt
        | Protocol::StdShipMmsi
        | Protocol::Orbitography
        | Protocol::LocationOther(_) => {
            // These families surface their identity primarily through hex_id /
            // country_code, which are already populated. Detailed sub-field
            // modelling for these is left to a follow-up (see PROVENANCE).
        }
    }
}

/// RLS TAC number ("2153" in the oracle examples) and RLS id.
///
/// Layout from amsa-code/fgb-decoder ReturnLinkServiceLocation: the TAC is a
/// 2-bit prefix (bits 41-42) mapping {00->2, 01->1, 10->3, else T} followed by
/// a 10-bit value (bits 43-52) rendered as 3 zero-padded decimal digits; the
/// RLS id is a 14-bit field (bits 53-66).
fn rls_tac_and_id(bits: &str) -> (String, u32) {
    let prefix = match &bits[41..43] {
        "00" => '2',
        "01" => '1',
        "10" => '3',
        _ => 'T',
    };
    let value = bits_to_u64(&bits[43..53]);
    let tac = format!("{prefix}{value:03}");
    let id = bits_to_u64(&bits[53..67]) as u32;
    (tac, id)
}

/// 5-bit modified-Baudot field (used by aircraft operator). Each 5-bit symbol
/// is prefixed with a leading `1` to form the 6-bit table key, exactly as
/// `Conversions.mBaudotBits2mBaudotStr(..., 5)`.
fn baudot5_decode(bitfield: &str) -> String {
    let mut out = String::new();
    let chars: Vec<char> = bitfield.chars().collect();
    let mut i = 0;
    while i + 5 <= chars.len() {
        let mut code = String::from("1");
        code.extend(&chars[i..i + 5]);
        let v = u8::from_str_radix(&code, 2).unwrap_or(0);
        out.push(baudot6_letter(v));
        i += 5;
    }
    out.trim_end_matches([' ', '?']).to_string()
}

/// Modified-Baudot table (6-bit key -> ASCII), from the amsa-code Conversions
/// `mbaudotToAsciiMap`. Letters, space, hyphen, slash and digits.
fn baudot6_letter(v: u8) -> char {
    match v {
        56 => 'A',
        51 => 'B',
        46 => 'C',
        50 => 'D',
        48 => 'E',
        54 => 'F',
        43 => 'G',
        37 => 'H',
        44 => 'I',
        58 => 'J',
        62 => 'K',
        41 => 'L',
        39 => 'M',
        38 => 'N',
        35 => 'O',
        45 => 'P',
        61 => 'Q',
        42 => 'R',
        52 => 'S',
        33 => 'T',
        60 => 'U',
        47 => 'V',
        57 => 'W',
        55 => 'X',
        53 => 'Y',
        49 => 'Z',
        36 => ' ',
        24 => '-',
        23 => '/',
        // digits (figures shift in the modified-Baudot table)
        13 => '0',
        29 => '1',
        25 => '2',
        16 => '3',
        10 => '4',
        1 => '5',
        21 => '6',
        28 => '7',
        12 => '8',
        3 => '9',
        _ => '?',
    }
}

/// Whether a long message carries position (tail not the all-default
/// FFFFFFFF / 00000000 pattern). Mirrors `defaultFFFFFFFF` / `default00000000`.
fn long_carries_position(hex: &str) -> bool {
    let hex = hex.trim();
    if hex.len() != 30 {
        return false;
    }
    let tail = &hex[hex.len() - 8..];
    tail != "FFFFFFFF" && tail != "00000000"
}

fn fill_position(beacon: &mut SarsatBeacon, bits: &str, proto: &Protocol, is_long_hex: bool) {
    if !is_long_hex {
        return;
    }
    if matches!(proto, Protocol::ReturnLinkService) {
        // RLS coarse position: bits 67-85 (19 bits), 30-minute units.
        let pos_bits = &bits[67..86];
        if pos_bits != "0111111110111111111" {
            let (lat_s, lon_s) = rls_coarse_seconds(bits);
            beacon.coarse_position = Some(Position {
                latitude: lat_s as f64 / 3600.0,
                longitude: lon_s as f64 / 3600.0,
            });
            if let Some((rlat, rlon)) = rls_fine_seconds(bits) {
                beacon.position = Some(Position {
                    latitude: rlat as f64 / 3600.0,
                    longitude: rlon as f64 / 3600.0,
                });
            }
        }
        return;
    }
    if proto.is_location() {
        // Coarse position bits 65-85.
        let coarse_bits = &bits[65..86];
        if coarse_bits != "011111111101111111111" {
            let lat_s = std_lat_seconds(bits);
            let lon_s = std_lon_seconds(bits);
            beacon.coarse_position = Some(Position {
                latitude: lat_s as f64 / 3600.0,
                longitude: lon_s as f64 / 3600.0,
            });
            // Offset refinement for standard-location long messages.
            if beacon.format == Format::Long {
                let (rlat, rlon) = offset_position(bits, lat_s, lon_s);
                beacon.position = Some(Position {
                    latitude: rlat as f64 / 3600.0,
                    longitude: rlon as f64 / 3600.0,
                });
            }
        }
    } else if beacon.format == Format::Long {
        // User Location protocol: absolute lat/lon at bits 108-119 / 120-132.
        if let Some(p) = user_location(bits) {
            beacon.position = Some(p);
        }
    }
}

/// RLS coarse position seconds (lat, lon) from bits 67-85, 30-minute units.
/// Mirrors `Common.position(binCode, 67, 19, 1800)`: the 19-bit field splits
/// into a 9-bit lat (sign + 8) and a 10-bit lon (sign + 9).
fn rls_coarse_seconds(bits: &str) -> (i64, i64) {
    let start = 67usize;
    let length = 19usize;
    let lon_len = length / 2 + 1; // odd length: lon gets the extra bit -> 10
    let lat_len = length - lon_len; // 9
    let spu = 1800i64; // 30 minutes in seconds
    // latitude
    let lat_bits = &bits[start + 1..start + lat_len];
    let code = bits_to_u64(lat_bits) as i64;
    let cs = code * spu;
    let deg = cs / 3600;
    let min = cs % 3600 / 60;
    let mut lat = deg * 3600 + min * 60;
    if bits.as_bytes()[start] == b'1' {
        lat = -lat;
    }
    // longitude
    let lon_bits = &bits[start + lat_len + 1..start + lat_len + lon_len];
    let code = bits_to_u64(lon_bits) as i64;
    let cs = code * spu;
    let deg = cs / 3600;
    let min = cs % 3600 / 60;
    let mut lon = deg * 3600 + min * 60;
    if bits.as_bytes()[start + lat_len] == b'1' {
        lon = -lon;
    }
    (lat, lon)
}

/// RLS fine position: coarse seconds plus the offset field (bits 115-132).
/// Mirrors `ReturnLinkServiceLocation.finePosition`. Returns refined seconds, or
/// `None` when the offset is the no-fine-position default.
fn rls_fine_seconds(bits: &str) -> Option<(i64, i64)> {
    let lat_off_bits = bits.get(115..123)?;
    if lat_off_bits == "100001111" {
        return None;
    }
    let (lat_s, lon_s) = rls_coarse_seconds(bits);
    // latitude offset
    let lat_sign = if bits.as_bytes()[115] == b'1' { 1 } else { -1 };
    let lat_min = bits_to_u64(&bits[116..119]) as i64;
    let lat_sec = bits_to_u64(&bits[120..123]) as i64 * 4;
    let lat_off = lat_sign * (lat_min * 60 + lat_sec);
    // longitude offset
    let lon_sign = if bits.as_bytes()[124] == b'1' { 1 } else { -1 };
    let lon_min = bits_to_u64(&bits[125..128]) as i64;
    let lon_sec = bits_to_u64(&bits[129..132]) as i64 * 4;
    let lon_off = lon_sign * (lon_min * 60 + lon_sec);
    let sign = |n: i64| if n < 0 { -1 } else { 1 };
    let rlat = (lat_s.abs() + lat_off) * sign(lat_s);
    let rlon = (lon_s.abs() + lon_off) * sign(lon_s);
    Some((rlat, rlon))
}

fn std_lat_seconds(bits: &str) -> i64 {
    let code = bits_to_u64(&bits[66..75]) as i64;
    let deg = code / 4;
    let mut s = deg * 3600;
    let min = (code % 4) * 15;
    s += min * 60;
    if bits.as_bytes()[65] == b'1' {
        s = -s;
    }
    s
}

fn std_lon_seconds(bits: &str) -> i64 {
    let code = bits_to_u64(&bits[76..86]) as i64;
    let deg = code / 4;
    let mut s = deg * 3600;
    let min = (code % 4) * 15;
    s += min * 60;
    if bits.as_bytes()[75] == b'1' {
        s = -s;
    }
    s
}

/// Apply the Standard Location offset field (bits 113-132) to the coarse
/// position. Returns refined (lat_seconds, lon_seconds). Mirrors
/// `StandardLocation.offsetPosition`.
fn offset_position(bits: &str, lat_s: i64, lon_s: i64) -> (i64, i64) {
    let f = &bits[113..133];
    if f == "10000011111000001111" {
        return (lat_s, lon_s);
    }
    let fb = f.as_bytes();
    let min1 = bits_to_u64(&f[1..6]) as i64;
    let sec1 = bits_to_u64(&f[6..10]) as i64 * 4;
    let mut off1 = min1 * 60 + sec1;
    if fb[0] != b'1' {
        off1 = -off1;
    }
    let mut tlat = lat_s.abs() + off1;
    if lat_s < 0 {
        tlat = -tlat;
    }
    let min2 = bits_to_u64(&f[11..16]) as i64;
    let sec2 = bits_to_u64(&f[16..20]) as i64 * 4;
    let mut off2 = min2 * 60 + sec2;
    if fb[10] != b'1' {
        off2 = -off2;
    }
    let mut tlon = lon_s.abs() + off2;
    if lon_s < 0 {
        tlon = -tlon;
    }
    (tlat, tlon)
}

/// User Location protocol absolute position (bits 108-119 lat, 120-132 lon).
/// Mirrors `User.latitude` / `User.longitude`.
fn user_location(bits: &str) -> Option<Position> {
    let lat_bits = bits.get(108..120)?;
    let lon_bits = bits.get(120..133)?;
    if lat_bits == "011111110000" || lon_bits == "0111111110000" {
        return None;
    }
    let lat = {
        let deg = bits_to_u64(&lat_bits[1..8]) as i64;
        let mut s = deg * 3600;
        let min = bits_to_u64(&lat_bits[8..12]) as i64 * 4;
        s += min * 60;
        if lat_bits.as_bytes()[0] == b'1' {
            s = -s;
        }
        s as f64 / 3600.0
    };
    let lon = {
        let deg = bits_to_u64(&lon_bits[1..9]) as i64;
        let mut s = deg * 3600;
        let min = bits_to_u64(&lon_bits[9..13]) as i64 * 4;
        s += min * 60;
        if lon_bits.as_bytes()[0] == b'1' {
            s = -s;
        }
        s as f64 / 3600.0
    };
    Some(Position {
        latitude: lat,
        longitude: lon,
    })
}

// =====================================================================
// TODO (out of scope for this layer; documented in PROVENANCE.md):
//   * IQ -> bits demodulator: COSPAS-SARSAT FGB uses biphase-L (Manchester)
//     PSK at 400 bps with +/-1.1 rad phase modulation on the 406.025/406.028/
//     406.037 MHz carrier, a 160 ms unmodulated carrier, then a 15-bit bit-sync
//     "1" run + 9-bit frame sync. That demod path is not implemented here.
//   * Spec-faithful modulator/encoder (bits -> IQ).
//   * Second-generation beacons (C/S T.018 SGB, 250-bit / spread-spectrum).
// =====================================================================

#[cfg(test)]
mod unit {
    use super::*;

    #[test]
    fn hex_to_bits_lengths() {
        assert_eq!(hex_to_bits("3EE6F80D1AFFBFF").unwrap().len(), 25 + 1 + 60);
        assert_eq!(
            hex_to_bits("8DA41A02C17FDFF83B4235FFFFFFFF").unwrap().len(),
            25 + 120
        );
        assert!(hex_to_bits("ABC").is_none());
    }

    #[test]
    fn long_position_tail_detection() {
        assert!(!long_carries_position("8DA41A02C17FDFF83B4235FFFFFFFF"));
        assert!(!long_carries_position("8E8628D187874181D738F700000000"));
        assert!(long_carries_position("A3E7B10016150D364D8B3689C09437"));
    }
}
