//! Bit-string helpers and the two BCH error-correcting codes used by the
//! COSPAS-SARSAT First-Generation Beacon (C/S T.001).
//!
//! The whole decoder works on a "bit string" indexed exactly like C/S T.001
//! numbers the message bits: bit *N* of the standard lives at index *N* of the
//! string. To make that line up, [`hex_to_bits`] prepends 25 placeholder bits
//! for the bit-sync / frame-sync field (T.001 bits 1-24 are the sync pattern,
//! bit 25 is the format flag) so that the first hex character lands on bit 26.
//! See PROVENANCE.md for the sourcing of the generator polynomials and offsets.

/// Expand a hex beacon string to a T.001-indexed bit string.
///
/// * 15 hex chars (60 bits) — short message. Bit 25 (format flag) is unknown
///   from the 15-hex form, so it is represented by a `?` placeholder; the 60
///   protocol/ID bits begin at index 26.
/// * 30 hex chars (120 bits) — long message. The 120 transmitted bits begin at
///   index 25 (the format flag).
///
/// Returns `None` for any other length. This mirrors `Conversions.hexToBinary`
/// in amsa-code/fgb-decoder.
pub fn hex_to_bits(hex: &str) -> Option<String> {
    let hex = hex.trim();
    if hex.len() != 15 && hex.len() != 30 {
        return None;
    }
    let mut bits = String::with_capacity(hex.len() * 4);
    for c in hex.chars() {
        let v = c.to_digit(16)?;
        bits.push_str(&format!("{v:04b}"));
    }
    // 25 leading sync placeholders so index N == T.001 bit N.
    let mut out = String::with_capacity(26 + bits.len());
    out.push_str(&"0".repeat(25));
    if hex.len() == 15 {
        out.push('?');
    }
    out.push_str(&bits);
    Some(out)
}

/// Interpret a run of `'0'`/`'1'` as an unsigned big-endian integer.
pub fn bits_to_u64(bits: &str) -> u64 {
    let mut v = 0u64;
    for c in bits.chars() {
        v <<= 1;
        if c == '1' {
            v |= 1;
        }
    }
    v
}

/// Pack a `'0'`/`'1'` run into uppercase hex (4 bits per nibble, MSB first).
/// Length must be a multiple of 4.
pub fn bits_to_hex(bits: &str) -> String {
    let mut out = String::with_capacity(bits.len() / 4);
    let b: Vec<char> = bits.chars().collect();
    let mut i = 0;
    while i + 4 <= b.len() {
        let nibble: String = b[i..i + 4].iter().collect();
        let v = u8::from_str_radix(&nibble, 2).unwrap_or(0);
        out.push(std::char::from_digit(v as u32, 16).unwrap().to_ascii_uppercase());
        i += 4;
    }
    out
}

/// Pack into octal (3 bits per digit), leading zeros stripped.
pub fn bits_to_octal(bits: &str) -> String {
    let b: Vec<char> = bits.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i + 3 <= b.len() {
        let tri: String = b[i..i + 3].iter().collect();
        let v = u8::from_str_radix(&tri, 2).unwrap_or(0);
        out.push(std::char::from_digit(v as u32, 8).unwrap());
        i += 3;
    }
    out.trim_start_matches('0').to_string()
}

fn remove_leading_zeros(s: &str) -> String {
    s.trim_start_matches('0').to_string()
}

fn xor_strip(a: &str, b: &str) -> String {
    let r: String = a
        .chars()
        .zip(b.chars())
        .map(|(x, y)| if x == y { '0' } else { '1' })
        .collect();
    remove_leading_zeros(&r)
}

/// Polynomial long-division remainder, ported faithfully from
/// `BeaconProtocol.calcBCHCODE` in amsa-code/fgb-decoder. `bit_code` already
/// has the parity field zero-padded on the right; the returned remainder is the
/// expected parity, left zero-padded to `gen.len() - 1` bits.
fn calc_bch(bit_code: &str, gen: &str) -> String {
    let b = gen.len();
    let mut result = remove_leading_zeros(&bit_code[..b]);
    // Short protocols drop a leading 0 in calcBCHCODE; even the ledger.
    let bit_code: &str = bit_code.strip_prefix('0').unwrap_or(bit_code);
    let chars: Vec<char> = bit_code.chars().collect();
    let mut i = b - 1;
    while i < chars.len() {
        if result.len() < b {
            result.push(chars[i]);
        }
        if result.len() == b {
            result = xor_strip(&result, gen);
        }
        i += 1;
    }
    while result.len() < b - 1 {
        result.insert(0, '0');
    }
    result
}

/// BCH(21,15) PDF-1 generator polynomial g(x) = x^21 + x^17 + x^16 + x^15 +
/// x^14 + x^11 + x^10 + x^8 + x^7 + x^6 + x^5 + x^1 + 1 (C/S T.001).
const GEN1: &str = "1001101101100111100011";
/// BCH(12,7) PDF-2 generator polynomial g(x) = x^12 + x^10 + x^8 + x^5 + x^4 +
/// x^3 + 1 (C/S T.001).
const GEN2: &str = "1010100111001";

/// Compute the expected PDF-1 parity (BCH(21,15)) over the protected field
/// (T.001 bits 25-85) of `bits`.
pub fn expected_bch1(bits: &str) -> String {
    // Protected data: bits 25..=85 (61 bits). Pad to 61 then append 21 parity
    // zeros — exactly BeaconProtocol.bch1.
    let data = &bits[25..86.min(bits.len())];
    let mut code = data.to_string();
    while code.len() < 61 {
        code.push('0');
    }
    code.push_str(&"0".repeat(21));
    calc_bch(&code, GEN1)
}

/// The PDF-1 parity field as transmitted (T.001 bits 86-106).
pub fn transmitted_bch1(bits: &str) -> Option<&str> {
    bits.get(86..107)
}

/// Compute the expected PDF-2 parity (BCH(12,7)) over the protected field
/// (T.001 bits 107-132) of `bits`.
pub fn expected_bch2(bits: &str) -> String {
    let data = &bits[107..133.min(bits.len())];
    let mut code = data.to_string();
    while code.len() < 26 {
        code.push('0');
    }
    code.push_str(&"0".repeat(12));
    calc_bch(&code, GEN2)
}

/// The PDF-2 parity field as transmitted (T.001 bits 133-144).
pub fn transmitted_bch2(bits: &str) -> Option<&str> {
    bits.get(133..145)
}
