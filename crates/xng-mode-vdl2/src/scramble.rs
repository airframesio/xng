//! VDL2 scrambler (ICAO Annex 10 Vol III §6.4.3.1.4): the shared 15-stage
//! LFSR (see xng_dsp::scramble), applied to everything after the unique
//! word.

pub use xng_dsp::scramble::Lfsr15 as Scrambler;

#[cfg(test)]
mod tests {
    use super::Scrambler;

    #[test]
    fn first_three_bits_keep_reserved_symbol_zero() {
        let mut s = Scrambler::new();
        let mut reserved = [0u8, 0, 0];
        s.apply(&mut reserved);
        assert_eq!(reserved, [0, 0, 0]);
    }
}
