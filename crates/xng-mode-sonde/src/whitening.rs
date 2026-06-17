//! RS41 data whitening / de-scrambling.
//!
//! Vaisala scrambles every byte after the 8-byte header against a fixed
//! 64-byte XOR mask: `frame[pos] = xframe[pos] ^ MASK[pos % 64]`, where
//! `xframe` is the on-air (whitened) byte stream. The mask is the
//! published LFSR sequence from rs1729/RS (`rs41mod.c`, `mask[MASK_LEN]`),
//! also derivable from the data-whitening notes in `rs41.txt`.
//!
//! The de-whitened header is the constant `86 35 F4 40 93 DF 1A 60`.

/// The 64-byte RS41 whitening mask (rs1729/RS `rs41mod.c`).
pub const MASK: [u8; 64] = [
    0x96, 0x83, 0x3E, 0x51, 0xB1, 0x49, 0x08, 0x98, 0x32, 0x05, 0x59, 0x0E, 0xF9, 0x44, 0xC6, 0x26,
    0x21, 0x60, 0xC2, 0xEA, 0x79, 0x5D, 0x6D, 0xA1, 0x54, 0x69, 0x47, 0x0C, 0xDC, 0xE8, 0x5C, 0xF1,
    0xF7, 0x76, 0x82, 0x7F, 0x07, 0x99, 0xA2, 0x2C, 0x93, 0x7C, 0x30, 0x63, 0xF5, 0x10, 0x2E, 0x61,
    0xD0, 0xBC, 0xB4, 0xB6, 0x06, 0xAA, 0xF4, 0x23, 0x78, 0x6E, 0x3B, 0xAE, 0xBF, 0x7B, 0x4C, 0xC1,
];

/// The 8-byte de-whitened frame header (RS41 sync constant).
pub const HEADER: [u8; 8] = [0x86, 0x35, 0xF4, 0x40, 0x93, 0xDF, 0x1A, 0x60];

/// XOR the whitening mask across a buffer (in place). The transform is its
/// own inverse, so the same call whitens or de-whitens.
///
/// `start` is the absolute frame offset of `buf[0]`, so the mask phase is
/// `(start + i) % 64`. The RS41 leaves the 8-byte header un-whitened; pass
/// a slice starting at offset 8 with `start = 8` to de-whiten the body.
pub fn xor_mask(buf: &mut [u8], start: usize) {
    for (i, b) in buf.iter_mut().enumerate() {
        *b ^= MASK[(start + i) % MASK.len()];
    }
}

/// De-whiten a whole on-air frame: the first 8 header bytes pass through
/// unchanged, the rest are XORed with the mask at their natural phase.
pub fn dewhiten_frame(frame: &mut [u8]) {
    if frame.len() > HEADER.len() {
        let start = HEADER.len();
        xor_mask(&mut frame[start..], start);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The published whitened header `10 B6 CA 11 22 96 12 F8` de-whitens to
    /// the RS41 sync constant (rs1729/RS rs41.txt). This anchors the mask
    /// against real on-air bytes, not self-consistency.
    #[test]
    fn dewhiten_published_header() {
        let mut xframe = [0x10, 0xB6, 0xCA, 0x11, 0x22, 0x96, 0x12, 0xF8];
        xor_mask(&mut xframe, 0);
        assert_eq!(xframe, HEADER);
    }

    #[test]
    fn xor_mask_is_involution() {
        let mut buf: Vec<u8> = (0..200u32).map(|x| (x * 37) as u8).collect();
        let orig = buf.clone();
        xor_mask(&mut buf, 8);
        assert_ne!(buf, orig);
        xor_mask(&mut buf, 8);
        assert_eq!(buf, orig);
    }
}
