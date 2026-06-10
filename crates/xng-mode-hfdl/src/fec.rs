//! HFDL sequences, settings, scrambler application, interleaver, and
//! coded-payload framing (docs/notes/HFDL.md).

use xng_dsp::scramble::Lfsr15;

/// A1/A2 acquisition sequence, 127 chips (0 = +1 / 0°).
pub const A_BITS: &str = "0101101110111100011101000101011100000011110110011000100100111001111100100000100011010101001101101001010000101100001100101111111";
/// M base sequence, 127 chips.
pub const M_BITS: &str = "0111011011110100010110010111110001000000110011011000111001110101110000100110000010101011010010010100111100100011010100001111111";
/// T training segment, 15 chips.
pub const T_BITS: &str = "000100110101111";

pub fn bits_of(s: &str) -> Vec<u8> {
    s.bytes().map(|b| b - b'0').collect()
}

/// Burst settings in M1-shift index order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Setting {
    pub m1_shift: usize,
    pub bps: u32,
    /// Bits per symbol (1=BPSK, 2=4PSK, 3=8PSK).
    pub bps_per_sym: u32,
    pub rate_quarter: bool,
    pub double_slot: bool,
}

pub const SETTINGS: [Setting; 8] = [
    Setting { m1_shift: 72, bps: 300, bps_per_sym: 1, rate_quarter: true, double_slot: false },
    Setting { m1_shift: 82, bps: 600, bps_per_sym: 1, rate_quarter: false, double_slot: false },
    Setting { m1_shift: 113, bps: 1200, bps_per_sym: 2, rate_quarter: false, double_slot: false },
    Setting { m1_shift: 123, bps: 1800, bps_per_sym: 3, rate_quarter: false, double_slot: false },
    Setting { m1_shift: 61, bps: 300, bps_per_sym: 1, rate_quarter: true, double_slot: true },
    Setting { m1_shift: 103, bps: 600, bps_per_sym: 1, rate_quarter: false, double_slot: true },
    Setting { m1_shift: 93, bps: 1200, bps_per_sym: 2, rate_quarter: false, double_slot: true },
    Setting { m1_shift: 9, bps: 1800, bps_per_sym: 3, rate_quarter: false, double_slot: true },
];

impl Setting {
    pub fn data_segments(&self) -> usize {
        if self.double_slot { 168 } else { 72 }
    }
    /// Coded chips on air (data symbols × bits/symbol).
    pub fn chips(&self) -> usize {
        self.data_segments() * 30 * self.bps_per_sym as usize
    }
    /// Decoded payload bits.
    pub fn payload_bits(&self) -> usize {
        if self.rate_quarter { self.chips() / 4 } else { self.chips() / 2 }
    }
    pub fn payload_bytes(&self) -> usize {
        self.payload_bits() / 8 // 300 bps: 540/8 = 67 full bytes (rest pad)
    }
    fn col_shift(&self) -> usize {
        if self.double_slot { 23 } else { 17 }
    }
    pub fn cols(&self) -> usize {
        self.chips() / 40
    }
}

/// Generate the interleaver write (push) position sequence.
fn push_indices(chips: usize, cols: usize, shift: usize) -> Vec<usize> {
    let mut out = Vec::with_capacity(chips);
    let (mut row, mut col) = (0usize, 0usize);
    for _ in 0..chips {
        out.push(row * cols + col);
        row += 1;
        if row == 40 {
            row = 0;
            col = (col + 1) % cols;
        }
        col = (col + cols - shift % cols) % cols;
    }
    out
}

/// Generate the read (pop) position sequence.
fn pop_indices(chips: usize, cols: usize) -> Vec<usize> {
    let mut out = Vec::with_capacity(chips);
    let (mut row, mut col) = (0usize, 0usize);
    for _ in 0..chips {
        out.push(row * cols + col);
        row = (row + 9) % 40;
        if row == 0 {
            col = (col + 1) % cols;
        }
    }
    out
}

/// Deinterleave received soft chips (air order) into decoder order.
pub fn deinterleave(soft: &[f32], s: &Setting) -> Vec<f32> {
    let chips = s.chips();
    debug_assert_eq!(soft.len(), chips);
    let push = push_indices(chips, s.cols(), s.col_shift());
    let pop = pop_indices(chips, s.cols());
    let mut table = vec![0.0f32; chips];
    for (k, &p) in push.iter().enumerate() {
        table[p] = soft[k];
    }
    pop.iter().map(|&p| table[p]).collect()
}

/// Interleave coded chips (decoder/pop order) into air order (TX).
pub fn interleave(chips_in: &[u8], s: &Setting) -> Vec<u8> {
    let chips = s.chips();
    debug_assert_eq!(chips_in.len(), chips);
    let push = push_indices(chips, s.cols(), s.col_shift());
    let pop = pop_indices(chips, s.cols());
    let mut table = vec![0u8; chips];
    for (k, &p) in pop.iter().enumerate() {
        table[p] = chips_in[k];
    }
    push.iter().map(|&p| table[p]).collect()
}

/// Per-data-symbol scrambler flips: LFSR15 truncated to a 120-bit cycle.
pub fn scramble_flips(n: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(n);
    'outer: loop {
        let mut lfsr = Lfsr15::new();
        for _ in 0..120 {
            out.push(lfsr.next_bit());
            if out.len() == n {
                break 'outer;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sequences_are_127_and_15() {
        assert_eq!(bits_of(A_BITS).len(), 127);
        assert_eq!(bits_of(M_BITS).len(), 127);
        assert_eq!(bits_of(T_BITS).len(), 15);
    }

    #[test]
    fn interleaver_indices_are_permutations() {
        for s in &SETTINGS {
            let chips = s.chips();
            for idx in [push_indices(chips, s.cols(), s.col_shift()), pop_indices(chips, s.cols())] {
                let mut sorted = idx.clone();
                sorted.sort_unstable();
                assert!(sorted.iter().enumerate().all(|(i, &v)| i == v),
                    "not a permutation at {} bps double={}", s.bps, s.double_slot);
            }
        }
    }

    #[test]
    fn interleave_roundtrip() {
        for s in &SETTINGS {
            let chips: Vec<u8> = (0..s.chips()).map(|i| (i % 2) as u8 ^ ((i / 7) % 2) as u8).collect();
            let air = interleave(&chips, s);
            let soft: Vec<f32> = air.iter().map(|&b| if b == 1 { 1.0 } else { -1.0 }).collect();
            let back = deinterleave(&soft, s);
            let hard: Vec<u8> = back.iter().map(|&v| (v > 0.0) as u8).collect();
            assert_eq!(hard, chips, "{} bps double={}", s.bps, s.double_slot);
        }
    }

    #[test]
    fn scrambler_tiles_120() {
        let f = scramble_flips(240);
        assert_eq!(&f[..120], &f[120..240]);
    }
}
