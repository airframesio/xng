//! CCIR 493 / ITU-R M.493 symbol level.
//!
//! DSC characters are 10-bit symbols: 7 information bits followed by a 3-bit
//! check sequence. The information bits are sent B1 (least significant) first.
//! The check bits carry a count of the number of "B" (binary 0) elements in
//! the 7 information bits, sent most-significant bit first. A correctly
//! received symbol therefore satisfies
//!
//! ```text
//! check == number_of_zero_bits_among_the_7_information_bits
//! ```
//!
//! which gives every symbol an inherent integrity check. Symbols are
//! transmitted twice with time diversity: the DX (data) stream carries each
//! character once, and the RX (repetition) stream repeats it 4 characters
//! later, so a symbol corrupted in one stream can be recovered from the
//! other. See PROVENANCE.md for the standards sourcing and the external
//! reference vectors that pin this layer.

/// A symbol that could not be recovered from either the DX or RX stream.
pub const ERASURE: i32 = -1;

/// Number of information bits in a DSC symbol (B1..B7).
pub const INFO_BITS: usize = 7;
/// Total bits per symbol (7 info + 3 check).
pub const SYMBOL_BITS: usize = 10;

/// Decodes one 10-bit symbol into its 7-bit information value, returning the
/// value and whether the embedded zero-count check passed.
///
/// `bits` are the 10 received bits in transmission order: B1..B7 (information,
/// LSB first) then the 3 check bits (MSB first). Values must be 0 or 1.
pub fn decode_symbol(bits: &[u8]) -> (u8, bool) {
    debug_assert!(bits.len() >= SYMBOL_BITS);
    let mut value: u8 = 0;
    for (j, &b) in bits.iter().take(INFO_BITS).enumerate() {
        value |= (b & 1) << j;
    }
    let received_check = ((bits[7] & 1) << 2) | ((bits[8] & 1) << 1) | (bits[9] & 1);
    let ok = received_check == zero_count(value);
    (value, ok)
}

/// Counts the "B" (zero) elements among the 7 information bits — the quantity
/// the 3 check bits of a DSC symbol encode.
pub fn zero_count(value: u8) -> u8 {
    let mut zeros = 0u8;
    for i in 0..INFO_BITS {
        if (value >> i) & 1 == 0 {
            zeros += 1;
        }
    }
    zeros
}

/// Verifies the 10-bit check on a symbol whose 7-bit value and 3 received
/// check bits are already separated. `check` is the integer value of the 3
/// check bits (0..=7).
pub fn check_ok(value: u8, check: u8) -> bool {
    zero_count(value) == (check & 0x7)
}

/// Decodes a contiguous bit stream into symbol values, 10 bits per symbol.
/// Symbols failing the zero-count check are emitted as [`ERASURE`].
pub fn decode_bitstream(bits: &[u8]) -> Vec<i32> {
    let mut out = Vec::with_capacity(bits.len() / SYMBOL_BITS);
    let mut i = 0;
    while i + SYMBOL_BITS <= bits.len() {
        let (value, ok) = decode_symbol(&bits[i..i + SYMBOL_BITS]);
        out.push(if ok { value as i32 } else { ERASURE });
        i += SYMBOL_BITS;
    }
    out
}

/// De-interleaves a received character stream into the recovered symbol
/// sequence using DX/RX time diversity.
///
/// Characters alternate DX, RX, DX, RX, ... (DX = even index, RX = odd). The
/// RX stream is the DX stream delayed by the diversity interval. Phasing
/// characters at the head of each stream are discarded; the data portion of
/// the DX stream is then taken character by character, falling back to the
/// corresponding (time-shifted) RX character whenever the DX character was
/// erased.
///
/// `dx_skip` is how many leading DX characters are phasing/format-setup
/// characters to drop before data begins; `rx_offset` is the position of the
/// RX repetition of a given data character relative to its DX index. These
/// follow the diversity geometry of the reference decoder (6 leading DX
/// phasing characters; the RX repeat trails by 2 positions in the RX stream).
pub fn deinterleave_dx_rx(chars: &[i32], dx_skip: usize, rx_offset: usize) -> Vec<i32> {
    let mut dx = Vec::new();
    let mut rx = Vec::new();
    for (i, &c) in chars.iter().enumerate() {
        if i % 2 == 0 {
            dx.push(c);
        } else {
            rx.push(c);
        }
    }

    let mut symbols = Vec::new();
    for (k, &d) in dx.iter().enumerate().skip(dx_skip) {
        if d != ERASURE {
            symbols.push(d);
        } else {
            let rx_idx = k + rx_offset;
            if rx_idx < rx.len() {
                symbols.push(rx[rx_idx]);
            } else {
                symbols.push(ERASURE);
            }
        }
    }
    symbols
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Bit-level oracle vectors taken verbatim from the external reference
    /// decoder's unit tests (TAOSW.DSC_Decoder, MIT licensed):
    /// `GMDSSDecoderTests.RetriveDataByteTest1..4`. Each is a 10-bit symbol
    /// with its expected 7-bit information value.
    #[test]
    fn reference_symbol_values() {
        // RetriveDataByteTest1 -> 2
        assert_eq!(decode_symbol(&[0, 1, 0, 0, 0, 0, 0, 1, 1, 0]).0, 2);
        // RetriveDataByteTest2 -> 122 (Acknowledge BQ)
        assert_eq!(decode_symbol(&[0, 1, 0, 1, 1, 1, 1, 0, 1, 0]).0, 122);
        // RetriveDataByteTest3 -> 127 (EOS other calls)
        assert_eq!(decode_symbol(&[1, 1, 1, 1, 1, 1, 1, 0, 0, 0]).0, 127);
        // RetriveDataByteTest4 -> 43
        assert_eq!(decode_symbol(&[1, 1, 0, 1, 0, 1, 0, 0, 1, 1]).0, 43);
    }

    /// The same vectors also satisfy the embedded zero-count check, proving
    /// the 3-bit check field is the count of zero information bits.
    #[test]
    fn reference_symbols_pass_check() {
        for bits in [
            [0, 1, 0, 0, 0, 0, 0, 1, 1, 0],
            [0, 1, 0, 1, 1, 1, 1, 0, 1, 0],
            [1, 1, 1, 1, 1, 1, 1, 0, 0, 0],
            [1, 1, 0, 1, 0, 1, 0, 0, 1, 1],
        ] {
            assert!(decode_symbol(&bits).1, "check failed for {bits:?}");
        }
    }

    /// A symbol with a deliberately wrong check field fails and decodes to
    /// an erasure through the stream decoder.
    #[test]
    fn corrupt_check_is_erasure() {
        // value 2 has six zero bits; advertise five (101) instead of six (110).
        let (_v, ok) = decode_symbol(&[0, 1, 0, 0, 0, 0, 0, 1, 0, 1]);
        assert!(!ok);
        let stream = decode_bitstream(&[0, 1, 0, 0, 0, 0, 0, 1, 0, 1]);
        assert_eq!(stream, vec![ERASURE]);
    }

    /// `zero_count` matches the reference `ComputeParity` (count of zero bits).
    #[test]
    fn zero_count_matches_reference() {
        assert_eq!(zero_count(0b0000010), 6); // value 2
        assert_eq!(zero_count(0b1111010), 2); // value 122
        assert_eq!(zero_count(0b1111111), 0); // value 127
        assert_eq!(zero_count(0b0101011), 3); // value 43
    }

    /// DX/RX diversity: when a DX data character is erased, the symbol is
    /// recovered from the time-shifted RX stream. The RX repeat of the dx
    /// character at dx-index k lives at rx-index k + rx_offset.
    #[test]
    fn dx_rx_recovers_erased_dx_from_rx() {
        // 12 dx chars: indices 0..5 phasing, 6 erased, 7..11 carry 71..75.
        // The erased dx[6] is recovered from rx[6 + 2] = rx[8].
        let mut dx = [0i32; 12];
        for d in dx.iter_mut().take(6) {
            *d = 125; // DX phasing
        }
        dx[6] = ERASURE;
        for (k, d) in dx.iter_mut().enumerate().take(12).skip(7) {
            *d = 70 + k as i32; // 77,78,79,80,81
        }
        let mut rx = [0i32; 12];
        rx[8] = 66; // recovered value for the erased dx[6]
        let mut chars = Vec::new();
        for i in 0..12 {
            chars.push(dx[i]);
            chars.push(rx[i]);
        }
        let syms = deinterleave_dx_rx(&chars, 6, 2);
        // First emitted data symbol is the recovered dx[6]; the rest are the
        // good dx data characters.
        assert_eq!(syms, vec![66, 77, 78, 79, 80, 81]);
    }

    /// When neither the DX character nor its RX repeat is available, the
    /// symbol is emitted as an erasure rather than guessed.
    #[test]
    fn dx_rx_unrecoverable_is_erasure() {
        // dx[6] erased and rx[8] also erased -> erasure out.
        let mut dx = [125i32; 9];
        dx[6] = ERASURE;
        dx[7] = 99;
        dx[8] = 100;
        let mut rx = [0i32; 9];
        rx[8] = ERASURE;
        let mut chars = Vec::new();
        for i in 0..9 {
            chars.push(dx[i]);
            chars.push(rx[i]);
        }
        let syms = deinterleave_dx_rx(&chars, 6, 2);
        assert_eq!(syms, vec![ERASURE, 99, 100]);
    }
}
