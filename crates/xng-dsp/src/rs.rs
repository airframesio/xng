//! Reed-Solomon over GF(2^8) with errors-and-erasures decoding
//! (textbook Berlekamp-Massey + Forney), parameterized by field polynomial
//! and generator roots — VDL2 uses p(x)=0x187 with roots α^120..α^125.

/// GF(2^8) arithmetic tables for a given primitive polynomial.
pub struct Gf256 {
    exp: [u8; 512],
    log: [u8; 256],
}

impl Gf256 {
    /// `prim_poly` includes the x^8 term, e.g. 0x187 for VDL2,
    /// 0x11D for the common CCSDS/QR field.
    pub fn new(prim_poly: u16) -> Self {
        let mut exp = [0u8; 512];
        let mut log = [0u8; 256];
        let mut x: u16 = 1;
        for i in 0..255 {
            exp[i] = x as u8;
            log[x as usize] = i as u8;
            x <<= 1;
            if x & 0x100 != 0 {
                x ^= prim_poly;
            }
        }
        for i in 255..512 {
            exp[i] = exp[i - 255];
        }
        Self { exp, log }
    }

    #[inline]
    pub fn mul(&self, a: u8, b: u8) -> u8 {
        if a == 0 || b == 0 {
            0
        } else {
            self.exp[self.log[a as usize] as usize + self.log[b as usize] as usize]
        }
    }

    #[inline]
    pub fn div(&self, a: u8, b: u8) -> u8 {
        debug_assert!(b != 0);
        if a == 0 {
            0
        } else {
            self.exp[255 + self.log[a as usize] as usize - self.log[b as usize] as usize]
        }
    }

    #[inline]
    pub fn pow(&self, base_log: u32, e: u32) -> u8 {
        self.exp[((base_log * e) % 255) as usize]
    }

    #[inline]
    pub fn inv(&self, a: u8) -> u8 {
        debug_assert!(a != 0);
        self.exp[255 - self.log[a as usize] as usize]
    }
}

/// RS(255, 255-NPAR) codec; `first_root` is the exponent b such that the
/// generator is ∏ (x − α^(b+i)) for i in 0..NPAR.
pub struct ReedSolomon {
    gf: Gf256,
    npar: usize,
    first_root: u32,
    /// Generator polynomial, ascending degree, gen[npar] = 1.
    gen: Vec<u8>,
}

impl ReedSolomon {
    pub fn new(prim_poly: u16, npar: usize, first_root: u32) -> Self {
        let gf = Gf256::new(prim_poly);
        // gen = ∏ (x - α^(b+i))
        let mut gen = vec![0u8; npar + 1];
        gen[0] = 1;
        for i in 0..npar {
            let root = gf.pow(1, first_root + i as u32);
            // multiply gen by (x + root): new[j] = old[j-1] + root*old[j]
            for j in (0..=i + 1).rev() {
                let lower = if j == 0 { 0 } else { gen[j - 1] };
                gen[j] = lower ^ gf.mul(root, gen[j]);
            }
        }
        Self { gf, npar, first_root, gen }
    }

    pub fn npar(&self) -> usize {
        self.npar
    }

    /// Systematic encode: returns the `npar` check octets for `data`
    /// (data length ≤ 255-npar; treated as the highest-degree
    /// coefficients, first octet = highest degree). Check octets are
    /// returned highest degree first (transmission order).
    pub fn encode(&self, data: &[u8]) -> Vec<u8> {
        // Polynomial long division of data(x)·x^npar by gen(x).
        let mut rem = vec![0u8; self.npar];
        for &d in data {
            let coef = d ^ rem[0];
            rem.rotate_left(1);
            rem[self.npar - 1] = 0;
            if coef != 0 {
                for (j, r) in rem.iter_mut().enumerate() {
                    *r ^= self.gf.mul(coef, self.gen[self.npar - 1 - j]);
                }
            }
        }
        rem
    }

    /// Correct a full 255-octet codeword in place (data + checks, first
    /// octet = highest-degree coefficient). `erasures` are indices into
    /// `cw` of symbols known to be unreliable/missing. Returns the number
    /// of corrected symbols, or Err if uncorrectable.
    pub fn correct(&self, cw: &mut [u8], erasures: &[usize]) -> Result<usize, ()> {
        assert_eq!(cw.len(), 255);
        let gf = &self.gf;
        let n = 255usize;

        // Syndromes S_j = c(α^(b+j)).
        let mut synd = vec![0u8; self.npar];
        let mut all_zero = true;
        for (j, s) in synd.iter_mut().enumerate() {
            let root_log = (self.first_root + j as u32) % 255;
            let mut acc = 0u8;
            for (i, &c) in cw.iter().enumerate() {
                // coefficient degree of cw[i] is n-1-i
                if c != 0 {
                    let deg = (n - 1 - i) as u32;
                    acc ^= gf.mul(c, gf.pow(root_log, deg));
                }
            }
            *s = acc;
            if acc != 0 {
                all_zero = false;
            }
        }
        if all_zero {
            return Ok(0);
        }
        if erasures.len() > self.npar {
            return Err(());
        }

        // Erasure locator Γ(x) = ∏ (1 - α^deg_k · x), ascending degree.
        let mut gamma = vec![0u8; self.npar + 1];
        gamma[0] = 1;
        let mut gamma_deg = 0usize;
        for &pos in erasures {
            let xk = gf.pow(1, (n - 1 - pos) as u32);
            for j in (1..=gamma_deg + 1).rev() {
                gamma[j] ^= gf.mul(xk, gamma[j - 1]);
            }
            gamma_deg += 1;
        }

        // Forney syndromes: T(x) = [S(x)·Γ(x)] mod x^npar.
        let mut fsynd = vec![0u8; self.npar];
        for j in 0..self.npar {
            let mut acc = 0u8;
            for k in 0..=j.min(gamma_deg) {
                acc ^= gf.mul(gamma[k], synd[j - k]);
            }
            fsynd[j] = acc;
        }

        // Berlekamp-Massey on the Forney syndromes for the error locator.
        // With f erasures, BM runs over the npar-f syndromes starting at
        // index f.
        let f = erasures.len();
        let max_errors = (self.npar - f) / 2;
        let mut lambda = vec![0u8; self.npar + 1];
        let mut prev = vec![0u8; self.npar + 1];
        lambda[0] = 1;
        prev[0] = 1;
        let mut l = 0usize;
        let mut m = 1usize;
        let mut b = 1u8;
        for r in 0..self.npar - f {
            let mut delta = fsynd[r + f];
            for i in 1..=l {
                delta ^= gf.mul(lambda[i], fsynd[r + f - i]);
            }
            if delta == 0 {
                m += 1;
            } else if 2 * l <= r {
                let t = lambda.clone();
                let scale = gf.div(delta, b);
                for i in 0..=self.npar - m {
                    lambda[i + m] ^= gf.mul(scale, prev[i]);
                }
                prev = t;
                l = r + 1 - l;
                b = delta;
                m = 1;
            } else {
                let scale = gf.div(delta, b);
                for i in 0..=self.npar - m {
                    lambda[i + m] ^= gf.mul(scale, prev[i]);
                }
                m += 1;
            }
        }
        if l > max_errors {
            return Err(());
        }

        // Errata locator Ψ = Λ·Γ.
        let mut psi = vec![0u8; lambda.len() + gamma.len()];
        for (i, &la) in lambda.iter().enumerate() {
            if la == 0 {
                continue;
            }
            for (j, &ga) in gamma.iter().enumerate() {
                psi[i + j] ^= gf.mul(la, ga);
            }
        }
        while psi.len() > 1 && *psi.last().unwrap() == 0 {
            psi.pop();
        }
        let psi_deg = psi.len() - 1;

        // Errata evaluator Ω = [S·Ψ] mod x^npar.
        let mut omega = vec![0u8; self.npar];
        for j in 0..self.npar {
            let mut acc = 0u8;
            for k in 0..=j.min(psi_deg) {
                acc ^= gf.mul(psi[k], synd[j - k]);
            }
            omega[j] = acc;
        }

        // Chien search over all positions; Forney value correction.
        let mut corrected = 0usize;
        for i in 0..n {
            let deg = (n - 1 - i) as u32;
            let xinv_log = (255 - (deg % 255)) % 255; // log of α^-deg
            // Ψ(α^-deg)
            let mut val = 0u8;
            for (k, &p) in psi.iter().enumerate() {
                if p != 0 {
                    val ^= gf.mul(p, gf.pow(xinv_log, k as u32));
                }
            }
            if val != 0 {
                continue;
            }
            // Root found: magnitude = Ω(Xinv) / Ψ'(Xinv), with the
            // first_root offset: e = X^(1-b) · Ω(Xinv) / Ψ'(Xinv).
            let mut om = 0u8;
            for (k, &o) in omega.iter().enumerate() {
                if o != 0 {
                    om ^= gf.mul(o, gf.pow(xinv_log, k as u32));
                }
            }
            // Formal derivative of Ψ at Xinv: odd-power terms.
            let mut dpsi = 0u8;
            for (k, &p) in psi.iter().enumerate() {
                if k % 2 == 1 && p != 0 {
                    dpsi ^= gf.mul(p, gf.pow(xinv_log, (k - 1) as u32));
                }
            }
            if dpsi == 0 {
                return Err(());
            }
            let mut mag = gf.div(om, dpsi);
            // Adjust for first_root b ≠ 1: multiply by X^(1-b) = α^(deg·(1-b)).
            let adj = (deg as i64 * (1 - self.first_root as i64)).rem_euclid(255) as u32;
            mag = gf.mul(mag, self.gf.exp[adj as usize]);
            cw[i] ^= mag;
            corrected += 1;
        }
        if corrected != psi_deg {
            return Err(()); // locator degree and found roots disagree
        }

        // Verify: recompute syndromes.
        for j in 0..self.npar {
            let root_log = (self.first_root + j as u32) % 255;
            let mut acc = 0u8;
            for (i, &c) in cw.iter().enumerate() {
                if c != 0 {
                    acc ^= gf.mul(c, gf.pow(root_log, (n - 1 - i) as u32));
                }
            }
            if acc != 0 {
                return Err(());
            }
        }
        Ok(corrected)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vdl2_rs() -> ReedSolomon {
        ReedSolomon::new(0x187, 6, 120)
    }

    fn make_codeword(rs: &ReedSolomon, seed: u64) -> Vec<u8> {
        let mut state = seed;
        let mut data: Vec<u8> = (0..249)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                state as u8
            })
            .collect();
        let checks = rs.encode(&data);
        data.extend(checks);
        data
    }

    #[test]
    fn clean_codeword_has_zero_syndromes() {
        let rs = vdl2_rs();
        let mut cw = make_codeword(&rs, 42);
        assert_eq!(rs.correct(&mut cw, &[]), Ok(0));
    }

    #[test]
    fn corrects_up_to_three_errors() {
        let rs = vdl2_rs();
        let clean = make_codeword(&rs, 7);
        for nerr in 1..=3 {
            let mut cw = clean.clone();
            for k in 0..nerr {
                cw[40 + 60 * k] ^= 0x5A + k as u8;
            }
            assert_eq!(rs.correct(&mut cw, &[]), Ok(nerr), "nerr={nerr}");
            assert_eq!(cw, clean);
        }
    }

    #[test]
    fn four_errors_fail() {
        let rs = vdl2_rs();
        let clean = make_codeword(&rs, 9);
        let mut cw = clean.clone();
        for k in 0..4 {
            cw[10 + 50 * k] ^= 0xA5;
        }
        assert!(rs.correct(&mut cw, &[]).is_err() || cw != clean);
    }

    #[test]
    fn erasures_plus_errors() {
        let rs = vdl2_rs();
        let clean = make_codeword(&rs, 11);

        // 4 erasures (zeroed) + 1 error: 2·1 + 4 = 6 ≤ npar.
        let mut cw = clean.clone();
        let erasures = [251usize, 252, 253, 254];
        for &e in &erasures {
            cw[e] = 0;
        }
        cw[100] ^= 0x77;
        let fixed = rs.correct(&mut cw, &erasures).expect("must correct");
        assert!(fixed >= 1);
        assert_eq!(cw, clean);

        // 2 erasures + 2 errors: 2·2 + 2 = 6 ≤ npar.
        let mut cw = clean.clone();
        let erasures = [253usize, 254];
        for &e in &erasures {
            cw[e] = 0;
        }
        cw[5] ^= 0x01;
        cw[200] ^= 0xFF;
        rs.correct(&mut cw, &erasures).expect("must correct");
        assert_eq!(cw, clean);
    }

    #[test]
    fn shortened_virtual_fill() {
        // Short block: 40 real octets + zero fill to 249.
        let rs = vdl2_rs();
        let mut data = vec![0u8; 249];
        for (i, d) in data.iter_mut().take(40).enumerate() {
            *d = (i as u8).wrapping_mul(17).wrapping_add(3);
        }
        let checks = rs.encode(&data);
        let mut cw = data.clone();
        cw.extend(&checks);
        cw[10] ^= 0x42; // error inside real data
        assert_eq!(rs.correct(&mut cw, &[]), Ok(1));
        assert_eq!(&cw[..40], &data[..40]);
    }
}
