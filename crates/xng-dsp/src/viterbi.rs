//! Soft-decision Viterbi decoder for rate-1/2 convolutional codes
//! (constraint length ≤ 8), used by Inmarsat Aero/STD-C and HFDL.
//! Textbook implementation (add-compare-select over a 2^(K-1) trellis).

/// Rate-1/2 convolutional encoder/decoder, polynomials given in the
/// common octal convention including the constraint-length bit
/// (e.g. K=7: 0o171, 0o133).
pub struct Viterbi {
    k: u32,
    states: usize,
    /// Output pair for (state, input bit): bit0 = g1 output, bit1 = g2.
    outputs: Vec<u8>,
}

impl Viterbi {
    pub fn new(k: u32, g1: u32, g2: u32) -> Self {
        assert!((3..=8).contains(&k));
        let states = 1usize << (k - 1);
        let mut outputs = vec![0u8; states * 2];
        for s in 0..states {
            for bit in 0..2u32 {
                // Shift register: newest bit in the MSB position.
                let reg = (bit << (k - 1)) | s as u32;
                let o1 = (reg & g1).count_ones() & 1;
                let o2 = (reg & g2).count_ones() & 1;
                outputs[s * 2 + bit as usize] = (o1 | (o2 << 1)) as u8;
            }
        }
        Self { k, states, outputs }
    }

    /// Standard NASA/CCSDS K=7 code (G1=0o171, G2=0o133).
    pub fn k7() -> Self {
        Self::new(7, 0o171, 0o133)
    }

    /// Encode `bits` (caller appends K-1 zero flush bits if desired).
    /// Output: g1 then g2 per input bit.
    pub fn encode(&self, bits: &[u8]) -> Vec<u8> {
        let mut state = 0usize;
        let mut out = Vec::with_capacity(bits.len() * 2);
        for &b in bits {
            let pair = self.outputs[state * 2 + b as usize];
            out.push(pair & 1);
            out.push((pair >> 1) & 1);
            state = ((b as usize) << (self.k - 2)) | (state >> 1);
        }
        out
    }

    /// Decode soft symbols: `soft` holds two values per data bit
    /// (g1, g2), each in -1.0..1.0 where positive means "bit 1 more
    /// likely" (hard decisions: ±1.0). Returns soft.len()/2 bits.
    pub fn decode(&self, soft: &[f32]) -> Vec<u8> {
        let nbits = soft.len() / 2;
        if nbits == 0 {
            return Vec::new();
        }
        const NEG: f32 = -1e30;
        let mask = self.states - 1;
        let mut metric = vec![NEG; self.states];
        metric[0] = 0.0;
        let mut next = vec![NEG; self.states];
        // Per step, bit `ns` of the decision word = low bit of the winning
        // predecessor of state ns (the input bit is ns >> (k-2)).
        let mut decisions: Vec<u64> = Vec::with_capacity(nbits);

        for t in 0..nbits {
            let s1 = soft[2 * t];
            let s2 = soft[2 * t + 1];
            next.iter_mut().for_each(|m| *m = NEG);
            let mut dec: u64 = 0;
            for s in 0..self.states {
                let m = metric[s];
                if m <= NEG {
                    continue;
                }
                for bit in 0..2usize {
                    let pair = self.outputs[s * 2 + bit];
                    // Branch metric: correlation with expected ±1 symbols.
                    let e1 = if pair & 1 == 1 { s1 } else { -s1 };
                    let e2 = if pair >> 1 == 1 { s2 } else { -s2 };
                    let cand = m + e1 + e2;
                    let ns = (bit << (self.k - 2)) | (s >> 1);
                    if cand > next[ns] {
                        next[ns] = cand;
                        if s & 1 == 1 {
                            dec |= 1 << ns;
                        } else {
                            dec &= !(1 << ns);
                        }
                    }
                }
            }
            decisions.push(dec);
            std::mem::swap(&mut metric, &mut next);
        }

        // Traceback from the best end state.
        let mut state = metric
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.partial_cmp(b.1).unwrap())
            .map(|(i, _)| i)
            .unwrap_or(0);
        let mut bits = vec![0u8; nbits];
        for t in (0..nbits).rev() {
            bits[t] = (state >> (self.k - 2)) as u8;
            let lost = (decisions[t] >> state) & 1;
            state = ((state << 1) | lost as usize) & mask;
        }
        bits
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn random_bits(n: usize, seed: u64) -> Vec<u8> {
        let mut s = seed;
        (0..n)
            .map(|_| {
                s ^= s << 13;
                s ^= s >> 7;
                s ^= s << 17;
                (s & 1) as u8
            })
            .collect()
    }

    #[test]
    fn clean_roundtrip() {
        let v = Viterbi::k7();
        let mut bits = random_bits(300, 1);
        bits.extend([0; 6]); // flush
        let coded = v.encode(&bits);
        let soft: Vec<f32> = coded.iter().map(|&b| if b == 1 { 1.0 } else { -1.0 }).collect();
        assert_eq!(v.decode(&soft), bits);
    }

    #[test]
    fn corrects_hard_bit_errors() {
        let v = Viterbi::k7();
        let mut bits = random_bits(400, 2);
        bits.extend([0; 6]);
        let coded = v.encode(&bits);
        let mut soft: Vec<f32> =
            coded.iter().map(|&b| if b == 1 { 1.0 } else { -1.0 }).collect();
        // Flip ~4% of coded symbols, spread out.
        for i in (7..soft.len()).step_by(25) {
            soft[i] = -soft[i];
        }
        assert_eq!(v.decode(&soft), bits, "Viterbi must fix sparse hard errors");
    }

    #[test]
    fn survives_soft_noise() {
        let v = Viterbi::k7();
        let mut bits = random_bits(400, 3);
        bits.extend([0; 6]);
        let coded = v.encode(&bits);
        let mut s = 0x1234_5678u64;
        let soft: Vec<f32> = coded
            .iter()
            .map(|&b| {
                s ^= s << 13;
                s ^= s >> 7;
                s ^= s << 17;
                let noise = (s as f32 / u64::MAX as f32) * 2.0 - 1.0;
                (if b == 1 { 1.0 } else { -1.0 }) + noise * 0.9
            })
            .collect();
        assert_eq!(v.decode(&soft), bits, "must decode at moderate SNR");
    }

    #[test]
    fn other_constraint_lengths() {
        // K=5 (e.g. some satcom modes): basic roundtrip.
        let v = Viterbi::new(5, 0o23, 0o35);
        let mut bits = random_bits(120, 4);
        bits.extend([0; 4]);
        let coded = v.encode(&bits);
        let soft: Vec<f32> = coded.iter().map(|&b| if b == 1 { 1.0 } else { -1.0 }).collect();
        assert_eq!(v.decode(&soft), bits);
    }
}
