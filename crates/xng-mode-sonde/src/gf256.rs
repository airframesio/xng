//! GF(2^8) arithmetic for the RS41 Reed-Solomon code.
//!
//! Field: GF(2)[x] / (x^8 + x^4 + x^3 + x^2 + 1) = 0x11D, generator
//! alpha = 0x02 (the primitive element X). This is the field the Vaisala
//! RS41 RS(255,231) code is defined over (rs1729/RS `rs41.txt`,
//! `bch_ecc_mod.c`: `GF256RS = { f: 0x11D, alpha: 0x02 }`).

/// Reducing polynomial for GF(2^8): x^8 + x^4 + x^3 + x^2 + 1.
pub const FIELD_POLY: u16 = 0x11D;
/// Primitive element alpha = X.
pub const ALPHA: u8 = 0x02;

/// Antilog (`exp[i] = alpha^i`) and log (`log[v] = i` s.t. `alpha^i = v`)
/// tables, generated once at construction.
#[derive(Clone)]
pub struct Gf256 {
    /// `exp[i] = alpha^i`, doubled to 512 so `exp[a+b]` never wraps.
    exp: [u8; 512],
    /// `log[v]` = discrete log of `v`. `log[0]` is unused.
    log: [u8; 256],
}

impl Gf256 {
    // Table generator: `i` indexes `exp` while `x` walks the field and
    // indexes `log`; an enumerate() rewrite would not be clearer.
    #[allow(clippy::needless_range_loop)]
    pub fn new() -> Self {
        let mut exp = [0u8; 512];
        let mut log = [0u8; 256];
        let mut x: u16 = 1;
        for i in 0..255usize {
            exp[i] = x as u8;
            log[x as usize] = i as u8;
            x <<= 1;
            if x & 0x100 != 0 {
                x ^= FIELD_POLY;
            }
        }
        for i in 255..512usize {
            exp[i] = exp[i - 255];
        }
        Gf256 { exp, log }
    }

    /// Field multiplication.
    #[inline]
    pub fn mul(&self, a: u8, b: u8) -> u8 {
        if a == 0 || b == 0 {
            0
        } else {
            self.exp[self.log[a as usize] as usize + self.log[b as usize] as usize]
        }
    }

    /// Multiplicative inverse (`a != 0`).
    #[inline]
    pub fn inv(&self, a: u8) -> u8 {
        debug_assert!(a != 0, "GF(256) inverse of zero");
        self.exp[(255 - self.log[a as usize] as usize) % 255]
    }

    /// `alpha^n` for any integer exponent `n` (reduced mod 255).
    #[inline]
    pub fn exp_alpha(&self, n: usize) -> u8 {
        self.exp[n % 255]
    }

    /// `a^n`.
    #[inline]
    pub fn pow(&self, a: u8, n: usize) -> u8 {
        if a == 0 {
            0
        } else {
            self.exp[(self.log[a as usize] as usize * n) % 255]
        }
    }

    /// Discrete log of `v` (`v != 0`): the `i` with `alpha^i = v`.
    #[inline]
    pub fn log_of(&self, v: u8) -> usize {
        debug_assert!(v != 0, "GF(256) log of zero");
        self.log[v as usize] as usize
    }
}

impl Default for Gf256 {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn log_exp_roundtrip() {
        let gf = Gf256::new();
        for v in 1u16..=255 {
            let v = v as u8;
            assert_eq!(gf.exp_alpha(gf.log[v as usize] as usize), v);
        }
    }

    #[test]
    fn inverse_is_inverse() {
        let gf = Gf256::new();
        for v in 1u16..=255 {
            let v = v as u8;
            assert_eq!(gf.mul(v, gf.inv(v)), 1);
        }
    }

    /// The RS41 RS generator polynomial is published in rs1729/RS rs41.txt:
    /// gen = 1 7a 76 a9 46 b2 ed d8 66 73 96 e5 49 82 48 3d 2b ce 01 ed f7
    /// 7f d9 90 75 (degree 24, roots alpha^0..alpha^23). Rebuilding it from
    /// the field is an external check that the field tables are correct.
    #[test]
    fn generator_poly_matches_oracle() {
        let gf = Gf256::new();
        let mut gen: Vec<u8> = vec![1];
        for i in 0..24usize {
            let mut ng = vec![0u8; gen.len() + 1];
            for j in 0..gen.len() {
                ng[j] ^= gen[j];
                ng[j + 1] ^= gf.mul(gen[j], gf.exp_alpha(i));
            }
            gen = ng;
        }
        let expected: [u8; 25] = [
            0x01, 0x7a, 0x76, 0xa9, 0x46, 0xb2, 0xed, 0xd8, 0x66, 0x73, 0x96, 0xe5, 0x49, 0x82,
            0x48, 0x3d, 0x2b, 0xce, 0x01, 0xed, 0xf7, 0x7f, 0xd9, 0x90, 0x75,
        ];
        assert_eq!(gen.as_slice(), &expected[..]);
    }
}
