//! Exponential ("variable-resolution") encoding used by the ADS-L
//! iConspicuity payload for ground speed, altitude and vertical rate.
//!
//! Per EASA ADS-L 4 SRD860 §G.1.6 the two leading bits of the encoded
//! field are a scaling exponent `e ∈ {0,1,2,3}`; the remaining `N` bits
//! are the base. The decoded magnitude is:
//!
//! ```text
//! value = 2^e * (2^N + base) − 2^N
//! ```
//!
//! Signed fields (vertical rate) prepend a sign bit above the exponent.
//!
//! This is the *spec* form (matches every worked example in §G.1.7–G.1.9
//! exactly). SoftRF's `UnsVRdecode` template uses a quantization-midpoint
//! variant that adds small +1/+2/+4 biases in the upper exponent ranges
//! and so diverges from the published examples at the high end (e.g. it
//! decodes ground-speed 0xFF to 239 m/s where the spec says 238 m/s); we
//! follow the spec. See PROVENANCE.md.

/// Decode an unsigned exponential field of `n` base bits (total `n + 2`).
pub fn uns_decode(value: u32, n: u32) -> u32 {
    let exp = (value >> n) & 0x3;
    let base = value & ((1 << n) - 1);
    let thres = 1u32 << n;
    // 2^exp * (2^n + base) − 2^n
    ((thres + base) << exp) - thres
}

/// Decode a signed exponential field of `n` base bits. The sign bit sits
/// at position `n + 2`; the low `n + 2` bits are the unsigned magnitude.
pub fn sign_decode(value: u32, n: u32) -> i32 {
    let sign_mask = 1u32 << (n + 2);
    let mag = uns_decode(value & (sign_mask - 1), n) as i32;
    if value & sign_mask != 0 {
        -mag
    } else {
        mag
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // EASA ADS-L 4 SRD860 §G.1.7 Altitude (N=12, −320 m offset applied by
    // the caller) — the spec's worked-example table.
    #[test]
    fn altitude_spec_examples() {
        assert_eq!(uns_decode(0x0000, 12) as i32 - 320, -320);
        assert_eq!(uns_decode(0x0140, 12) as i32 - 320, 0);
        assert_eq!(uns_decode(0x0528, 12) as i32 - 320, 1000);
        assert_eq!(uns_decode(0x3fff, 12) as i32 - 320, 61112);
    }

    // §G.1.8 Ground Speed (N=6, 0.25 m/s LSB).
    #[test]
    fn ground_speed_spec_examples() {
        assert_eq!(uns_decode(0x01, 6), 1); // 0.25 m/s
        assert_eq!(uns_decode(0x03, 6), 3); // 0.75 m/s
        assert_eq!(uns_decode(0xc4, 6), 480); // 120 m/s
        assert_eq!(uns_decode(0xff, 6), 952); // 238 m/s
    }

    // §G.1.9 Vertical Rate (N=6, sign bit @ bit 8, 0.125 m/s LSB).
    #[test]
    fn vertical_rate_spec_examples() {
        assert_eq!(sign_decode(0x000, 6), 0); // 0
        assert_eq!(sign_decode(0x001, 6), 1); // +0.125
        assert_eq!(sign_decode(0x101, 6), -1); // −0.125
        assert_eq!(sign_decode(0x048, 6), 80); // +10 m/s
        assert_eq!(sign_decode(0x148, 6), -80); // −10 m/s
        assert_eq!(sign_decode(0x0ff, 6), 952); // +119 m/s
        assert_eq!(sign_decode(0x1ff, 6), -952); // −119 m/s
    }
}
