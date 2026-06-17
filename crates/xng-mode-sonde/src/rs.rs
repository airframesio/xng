//! Interleaved Reed-Solomon RS(255,231) error correction for the RS41.
//!
//! The RS41 protects every frame with two interleaved RS(255,231) codewords
//! over GF(2^8) (0x11D), 24 parity bytes each, correcting up to 12 byte
//! errors per codeword. The codeword is *systematic with the parity first*:
//! `c[0..24]` are the parity symbols and `c[24..255]` the message symbols,
//! with `c[n]` the coefficient of `x^n`, so the 24 roots are
//! `alpha^0 .. alpha^23` (`b = 0`).
//!
//! The frame interleaves the two codewords:
//! - parity: frame byte `8 + i` to codeword1 parity `[0..24]`,
//!   frame byte `8 + 24 + i` to codeword2 parity `[0..24]`.
//! - message: frame byte `56 + 2*i` to codeword1 message `[24..255]`,
//!   frame byte `56 + 2*i + 1` to codeword2 message `[24..255]`.
//!
//! Short (320-byte) frames are zero-padded out to the full 462 message bytes
//! (2 x 231) before decoding — exactly as the reference does.
//!
//! Decoder: syndromes -> Berlekamp-Massey -> Chien search -> Forney.
//! Verified end-to-end against the rs1729/RS `rs41.txt` worked example
//! (`tests/`): codeword1 = 0 errors, codeword2 = 2 errors corrected.

use crate::gf256::Gf256;

/// Codeword length n = 255.
pub const N: usize = 255;
/// Parity symbols (n - k) = 24.
pub const R: usize = 24;
/// Message symbols k = 231.
pub const K: usize = N - R;

/// Offset of the RS-protected region inside the de-whitened frame: 8-byte
/// header, then 48 parity bytes (2 x 24), then the interleaved message.
pub const PARITY_POS: usize = 8;
/// First interleaved message byte (header + 48 parity).
pub const MSG_POS: usize = PARITY_POS + 2 * R; // 56

/// Outcome of decoding one interleaved frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RsResult {
    /// Errors corrected in codeword 1 (`None` = uncorrectable).
    pub errors1: Option<usize>,
    /// Errors corrected in codeword 2 (`None` = uncorrectable).
    pub errors2: Option<usize>,
}

impl RsResult {
    /// True when both codewords decoded cleanly.
    pub fn ok(&self) -> bool {
        self.errors1.is_some() && self.errors2.is_some()
    }

    /// Total corrected errors across both codewords (uncorrectable counted
    /// as 0).
    pub fn total_corrected(&self) -> usize {
        self.errors1.unwrap_or(0) + self.errors2.unwrap_or(0)
    }
}

/// The RS41 Reed-Solomon engine. Holds the GF(2^8) tables.
pub struct Rs41Rs {
    gf: Gf256,
}

impl Rs41Rs {
    pub fn new() -> Self {
        Rs41Rs { gf: Gf256::new() }
    }

    /// Correct the two interleaved codewords in place inside a de-whitened
    /// frame. `frame` must be at least [`MSG_POS`] long; a short frame is
    /// internally zero-padded for the missing message tail.
    ///
    /// On a correctable codeword the corrected bytes are written back to
    /// their interleaved frame positions.
    pub fn correct_frame(&self, frame: &mut [u8]) -> RsResult {
        let mut cw1 = self.gather(frame, 0);
        let mut cw2 = self.gather(frame, 1);
        let errors1 = self.decode_codeword(&mut cw1);
        let errors2 = self.decode_codeword(&mut cw2);
        if errors1.is_some() {
            self.scatter(frame, 0, &cw1);
        }
        if errors2.is_some() {
            self.scatter(frame, 1, &cw2);
        }
        RsResult { errors1, errors2 }
    }

    /// Read interleaved codeword `which` (0 or 1) out of the frame. Bytes
    /// past the end of a short frame are treated as zero.
    // `i` maps a codeword symbol to its interleaved frame position; the
    // index is the point, so an iterator rewrite would obscure it.
    #[allow(clippy::needless_range_loop)]
    fn gather(&self, frame: &[u8], which: usize) -> [u8; N] {
        let mut cw = [0u8; N];
        let at = |i: usize| -> u8 { frame.get(i).copied().unwrap_or(0) };
        for i in 0..R {
            cw[i] = at(PARITY_POS + which * R + i);
        }
        for i in 0..K {
            cw[R + i] = at(MSG_POS + 2 * i + which);
        }
        cw
    }

    /// Write a corrected codeword `which` back into the frame, skipping
    /// positions that fall past the (short) frame end.
    #[allow(clippy::needless_range_loop)]
    fn scatter(&self, frame: &mut [u8], which: usize, cw: &[u8; N]) {
        let len = frame.len();
        for i in 0..R {
            let p = PARITY_POS + which * R + i;
            if p < len {
                frame[p] = cw[i];
            }
        }
        for i in 0..K {
            let p = MSG_POS + 2 * i + which;
            if p < len {
                frame[p] = cw[R + i];
            }
        }
    }

    /// Syndromes `S[k] = c(alpha^k)` for `k = 0..R`, with `c[n]` the
    /// coefficient of `x^n`.
    fn syndromes(&self, cw: &[u8; N]) -> [u8; R] {
        let mut s = [0u8; R];
        for (k, sk) in s.iter_mut().enumerate() {
            let mut acc = 0u8;
            for (n, &cn) in cw.iter().enumerate() {
                if cn != 0 {
                    acc ^= self.gf.exp_alpha(self.gf.log_of(cn) + n * k);
                }
            }
            *sk = acc;
        }
        s
    }

    /// Decode one codeword in place. Returns `Some(error_count)` on success
    /// (0 = already valid), `None` if uncorrectable.
    fn decode_codeword(&self, cw: &mut [u8; N]) -> Option<usize> {
        let synd = self.syndromes(cw);
        if synd.iter().all(|&x| x == 0) {
            return Some(0);
        }

        // Berlekamp-Massey: find the error-locator polynomial C(x).
        let mut c = vec![0u8; R + 1];
        let mut b = vec![0u8; R + 1];
        c[0] = 1;
        b[0] = 1;
        let mut l = 0usize;
        let mut m = 1usize;
        let mut bb = 1u8;
        for n in 0..R {
            let mut delta = synd[n];
            for i in 1..=l {
                delta ^= self.gf.mul(c[i], synd[n - i]);
            }
            if delta == 0 {
                m += 1;
            } else if 2 * l <= n {
                let t = c.clone();
                let coef = self.gf.mul(delta, self.gf.inv(bb));
                for i in 0..=(R - m) {
                    c[i + m] ^= self.gf.mul(coef, b[i]);
                }
                l = n + 1 - l;
                b = t;
                bb = delta;
                m = 1;
            } else {
                let coef = self.gf.mul(delta, self.gf.inv(bb));
                for i in 0..=(R - m) {
                    c[i + m] ^= self.gf.mul(coef, b[i]);
                }
                m += 1;
            }
        }

        // Chien search: roots of C give error positions. A root alpha^-i
        // means an error at coefficient x^i.
        let mut err_pos = Vec::with_capacity(l);
        for i in 0..N {
            let xinv = self.gf.exp_alpha((255 - (i % 255)) % 255);
            let mut v = 0u8;
            let mut acc = 1u8;
            for &ci in c.iter().take(l + 1) {
                v ^= self.gf.mul(ci, acc);
                acc = self.gf.mul(acc, xinv);
            }
            if v == 0 {
                err_pos.push(i);
            }
        }
        if err_pos.len() != l {
            return None; // degree/roots mismatch -> uncorrectable
        }

        // Forney: error-evaluator Omega(x) = S(x) * C(x) mod x^R.
        let mut omega = vec![0u8; R];
        for (i, oi) in omega.iter_mut().enumerate() {
            let mut acc = 0u8;
            for j in 0..=i {
                if j < R {
                    acc ^= self.gf.mul(synd[j], c[i - j]);
                }
            }
            *oi = acc;
        }

        for &pos in &err_pos {
            let xi = self.gf.exp_alpha(pos); // error location value alpha^pos
            let xinv = self.gf.inv(xi);
            // Omega(Xinv)
            let mut omv = 0u8;
            let mut acc = 1u8;
            for &om in &omega {
                omv ^= self.gf.mul(om, acc);
                acc = self.gf.mul(acc, xinv);
            }
            // Formal derivative C'(Xinv): odd-index terms only (char 2).
            let mut der = 0u8;
            for i in (1..=l).step_by(2) {
                der ^= self.gf.mul(c[i], self.gf.pow(xinv, i - 1));
            }
            if der == 0 {
                return None;
            }
            // magnitude = Xi^(1-b) * Omega/C' = Xi * Omega/C' (b = 0).
            let mag = self.gf.mul(self.gf.mul(xi, omv), self.gf.inv(der));
            cw[pos] ^= mag;
        }

        // Verify the correction actually produced a valid codeword.
        let synd2 = self.syndromes(cw);
        if synd2.iter().any(|&x| x != 0) {
            return None;
        }
        Some(err_pos.len())
    }
}

impl Default for Rs41Rs {
    fn default() -> Self {
        Self::new()
    }
}
