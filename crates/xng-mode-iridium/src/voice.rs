//! Voice-channel (LCW ft=0) burst classification, following the
//! iridium-toolkit ladder (validated against it as a decode oracle):
//!
//! 1. CRC24 over the per-byte bit-reversed payload passes → **VDA**
//!    (data riding the voice channel, IIP-framed)
//! 2. RS(52,42) over GF(64) accepts the 6-bit symbol view → **VO6**
//! 3. RS over GF(256) (31 data + 8 checks + 8 erased) accepts the byte
//!    view → **VOD** (voice-channel data)
//! 4. Trailing zeros and byte-sum ≡ 0 (mod 256) → **VOZ** (zero-padded)
//! 5. Otherwise → **VOC**: an AMBE voice frame (codec is proprietary;
//!    bytes are surfaced for external decoding)

use serde_json::{json, Value};
use xng_dsp::rs::ReedSolomon;

/// CRC24 used by Iridium IIP/VDA frames (GSM 04.64 polynomial family):
/// reflected, poly 0x1BBA1B5 (reversed: 0xAD85DD), init 0xFFFFFF,
/// xor-out 0x0C91B6. A frame with a valid trailing CRC sums to 0.
pub fn iip_crc24(data: &[u8]) -> u32 {
    const RPOLY: u32 = 0xAD85DD;
    let mut crc: u32 = 0xFF_FFFF;
    for &b in data {
        crc ^= b as u32;
        for _ in 0..8 {
            crc = if crc & 1 != 0 { (crc >> 1) ^ RPOLY } else { crc >> 1 };
        }
    }
    crc ^ 0x0C91B6
}

/// GF(2^6) arithmetic for the RS(52,42) voice code (prim poly 0x43,
/// generator α = 2).
struct Gf64 {
    exp: [u8; 126],
    log: [u8; 64],
}

impl Gf64 {
    fn new() -> Self {
        let mut exp = [0u8; 126];
        let mut log = [0u8; 64];
        let mut x: u16 = 1;
        for i in 0..63 {
            exp[i] = x as u8;
            log[x as usize] = i as u8;
            x <<= 1;
            if x & 0x40 != 0 {
                x ^= 0x43;
            }
        }
        for i in 63..126 {
            exp[i] = exp[i - 63];
        }
        Self { exp, log }
    }

    fn mul(&self, a: u8, b: u8) -> u8 {
        if a == 0 || b == 0 {
            return 0;
        }
        self.exp[(self.log[a as usize] as usize) + (self.log[b as usize] as usize)]
    }

    fn div(&self, a: u8, b: u8) -> u8 {
        if a == 0 {
            return 0;
        }
        let d = (self.log[a as usize] as i32 - self.log[b as usize] as i32).rem_euclid(63);
        self.exp[d as usize]
    }

    /// α^(base_log · e)
    fn pow(&self, base_log: u32, e: u32) -> u8 {
        self.exp[((base_log * e) % 63) as usize]
    }
}

pub(crate) const RS6_N: usize = 52;
const RS6_NPAR: usize = 10;
const RS6_FCR: u32 = 54;

/// Errors-only RS decode of the 52×6-bit voice codeword. Returns the
/// number of corrected symbols, or Err if the word is uncorrectable.
pub(crate) fn rs6_correct(cw: &mut [u8; RS6_N]) -> Result<usize, ()> {
    let gf = Gf64::new();
    let n = RS6_N;

    // Syndromes S_j = c(α^(fcr+j)), cw[0] = highest-degree coefficient.
    let mut synd = [0u8; RS6_NPAR];
    let mut all_zero = true;
    for (j, s) in synd.iter_mut().enumerate() {
        let root_log = (RS6_FCR + j as u32) % 63;
        let mut acc = 0u8;
        for (i, &c) in cw.iter().enumerate() {
            if c != 0 {
                acc ^= gf.mul(c, gf.pow(root_log, (n - 1 - i) as u32));
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

    // Berlekamp-Massey for the error locator Λ.
    let mut lambda = [0u8; RS6_NPAR + 1];
    let mut prev = [0u8; RS6_NPAR + 1];
    lambda[0] = 1;
    prev[0] = 1;
    let (mut l, mut m, mut b) = (0usize, 1usize, 1u8);
    for r in 0..RS6_NPAR {
        let mut delta = synd[r];
        for i in 1..=l {
            delta ^= gf.mul(lambda[i], synd[r - i]);
        }
        if delta == 0 {
            m += 1;
        } else if 2 * l <= r {
            let t = lambda;
            let scale = gf.div(delta, b);
            for i in 0..=RS6_NPAR - m {
                lambda[i + m] ^= gf.mul(scale, prev[i]);
            }
            prev = t;
            l = r + 1 - l;
            b = delta;
            m = 1;
        } else {
            let scale = gf.div(delta, b);
            for i in 0..=RS6_NPAR - m {
                lambda[i + m] ^= gf.mul(scale, prev[i]);
            }
            m += 1;
        }
    }
    if l > RS6_NPAR / 2 {
        return Err(());
    }
    let lam_deg = (0..=RS6_NPAR).rev().find(|&i| lambda[i] != 0).unwrap_or(0);

    // Evaluator Ω = [S·Λ] mod x^npar.
    let mut omega = [0u8; RS6_NPAR];
    for j in 0..RS6_NPAR {
        let mut acc = 0u8;
        for k in 0..=j.min(lam_deg) {
            acc ^= gf.mul(lambda[k], synd[j - k]);
        }
        omega[j] = acc;
    }

    // Chien search + Forney correction.
    let mut corrected = 0usize;
    for i in 0..n {
        let deg = (n - 1 - i) as u32;
        let xinv_log = (63 - (deg % 63)) % 63;
        let mut val = 0u8;
        for (k, &p) in lambda.iter().enumerate() {
            if p != 0 {
                val ^= gf.mul(p, gf.pow(xinv_log, k as u32));
            }
        }
        if val != 0 {
            continue;
        }
        let mut om = 0u8;
        for (k, &o) in omega.iter().enumerate() {
            if o != 0 {
                om ^= gf.mul(o, gf.pow(xinv_log, k as u32));
            }
        }
        let mut dlam = 0u8;
        for (k, &p) in lambda.iter().enumerate() {
            if k % 2 == 1 && p != 0 {
                dlam ^= gf.mul(p, gf.pow(xinv_log, (k - 1) as u32));
            }
        }
        if dlam == 0 {
            return Err(());
        }
        let mut mag = gf.div(om, dlam);
        // first_root b ≠ 1 adjustment: e = X^(1-b) · Ω(Xinv)/Λ'(Xinv).
        let adj = (deg as i64 * (1 - RS6_FCR as i64)).rem_euclid(63) as usize;
        mag = gf.mul(mag, gf.exp[adj]);
        cw[i] ^= mag;
        corrected += 1;
    }
    if corrected != lam_deg {
        return Err(());
    }

    // Verify by recomputing syndromes.
    for j in 0..RS6_NPAR {
        let root_log = (RS6_FCR + j as u32) % 63;
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

/// GF(256) RS for the VOD byte view: 31 data + 8 transmitted checks +
/// 8 untransmitted (erased) checks, fcr 0, prim 0x11d.
pub(crate) fn vod_correct(payload: &[u8; 39]) -> Option<[u8; 31]> {
    let rs = ReedSolomon::new(0x11d, 16, 0);
    // Embed as the tail of a full 255-symbol codeword; the 8 missing
    // check octets are erasures at the end.
    let mut cw = [0u8; 255];
    cw[208..247].copy_from_slice(payload);
    let erasures: Vec<usize> = (247..255).collect();
    rs.correct(&mut cw, &erasures).ok()?;
    let mut out = [0u8; 31];
    out.copy_from_slice(&cw[208..239]);
    Some(out)
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Classify a voice-channel payload (the post-LCW bits of an ft=0
/// burst) and return structured details. Needs at least 312 bits.
pub fn classify_voice(payload_bits: &[u8]) -> Option<Value> {
    if payload_bits.len() < 312 {
        return None;
    }
    let nbytes = payload_bits.len() / 8;
    let mut payload_f = Vec::with_capacity(nbytes); // straight bytes
    let mut payload_r = Vec::with_capacity(nbytes); // per-byte bit-reversed
    for c in payload_bits.chunks(8).take(nbytes) {
        let f = c.iter().fold(0u8, |v, &b| (v << 1) | b);
        payload_f.push(f);
        payload_r.push(f.reverse_bits());
    }

    // 1. VDA: data frame with a valid CRC24 — an IIP frame riding the
    // voice channel; parse its ARQ structure.
    if iip_crc24(&payload_r) == 0 {
        let mut v = crate::iip::parse_iip_frame(&payload_r);
        v["voice_type"] = json!("VDA");
        return Some(v);
    }

    // 2. VO6: 52 six-bit symbols form an RS(52,42) codeword.
    let mut cw6 = [0u8; RS6_N];
    for (i, c) in payload_bits[..312].chunks(6).enumerate() {
        cw6[i] = c.iter().fold(0u8, |v, &b| (v << 1) | b);
    }
    if let Ok(fixed) = rs6_correct(&mut cw6) {
        let bits: String = cw6[..42].iter().map(|s| format!("{s:06b}")).collect();
        return Some(json!({
            "voice_type": "VO6",
            "data_bits": bits,
            "rs_corrected": fixed,
        }));
    }

    // 3. VOD: byte view forms the shortened GF(256) codeword.
    if payload_f.len() >= 39 {
        let mut p = [0u8; 39];
        p.copy_from_slice(&payload_f[..39]);
        if let Some(data) = vod_correct(&p) {
            return Some(json!({
                "voice_type": "VOD",
                "data_hex": hex(&data),
            }));
        }
    }

    // 4. VOZ: zero-padded frame whose bytes sum to 0 mod 256 (the
    // trailing-zero check is the toolkit's false-match heuristic).
    let n = payload_f.len();
    if n >= 4
        && payload_f[n - 4..n - 1].iter().all(|&x| x == 0)
        && payload_f.iter().fold(0u8, |a, &x| a.wrapping_add(x)) == 0
    {
        let end = payload_f[..n - 1]
            .iter()
            .rposition(|&x| x != 0)
            .map_or(1, |i| i + 1);
        return Some(json!({
            "voice_type": "VOZ",
            "data_hex": hex(&payload_f[..end]),
        }));
    }

    // 5. VOC: assume AMBE voice (no codec-level sanity check exists).
    Some(json!({
        "voice_type": "VOC",
        "ambe_hex": hex(&payload_f),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bits_of_hex(h: &str) -> Vec<u8> {
        h.as_bytes()
            .chunks(2)
            .flat_map(|c| {
                let b = u8::from_str_radix(std::str::from_utf8(c).unwrap(), 16).unwrap();
                (0..8).rev().map(move |i| (b >> i) & 1)
            })
            .collect()
    }

    // Vectors generated with iridium-toolkit's own rs/rs6/crcmod code
    // (oracle), one per ladder stage; see the PR notes.
    const VDA: &str = "a5b25318a40cddb8b6c8347b6bc4de749b78fc4ef8d3988ee822296b923cb93a2c067d8c904549";
    const VO6: &str = "2076bfda8efaba67d77ca9bfaf99093f556b4fed45268aecffa20b8bc2079f93f5a9dbd45d79e1";
    const VOD: &str = "91c5b10becb5563bfc1e6f93427ecbc8fe2955e5cd8e46dc8ed4b7c2764d2ac749977139180ede";
    const VOD_ERR: &str =
        "91c5b10becef563bfc1e6f93427ecbc8fe2955e5cd8e46dc8ed4b7c2764d2ac749977139180ede";
    const VOD_MSG: &str = "91c5b10becb5563bfc1e6f93427ecbc8fe2955e5cd8e46dc8ed4b7c2764d2a";
    const VOZ: &str = "5a4d767706f85d8690024ad6bda3401be9c8cbccc935f6cd1f61226ae15338ae1a34a100000000";
    const VOC: &str = "004d33ba0d246ac04c81b1baf23e3bf9eef5f79f2b4934af87f5520b69b94b0d982e85bb55b672";

    #[test]
    fn crc24_check_value() {
        // crcmod oracle: iip_crc24(b"123456789") == 0xbde882.
        assert_eq!(iip_crc24(b"123456789"), 0xbde882);
        assert_eq!(iip_crc24(b""), 0xf36e49);
    }

    #[test]
    fn ladder_classifies_each_stage() {
        for (hexstr, want) in [
            (VDA, "VDA"),
            (VO6, "VO6"),
            (VOD, "VOD"),
            (VOZ, "VOZ"),
            (VOC, "VOC"),
        ] {
            let v = classify_voice(&bits_of_hex(hexstr)).unwrap();
            assert_eq!(v["voice_type"], want, "payload {hexstr}");
        }
    }

    #[test]
    fn vod_corrects_single_byte_error() {
        let v = classify_voice(&bits_of_hex(VOD_ERR)).unwrap();
        assert_eq!(v["voice_type"], "VOD");
        assert_eq!(v["data_hex"], VOD_MSG);
    }

    #[test]
    fn vo6_recovers_oracle_message() {
        // First 42 corrected symbols must match the toolkit's encoding
        // input (vo6_msg6 from the oracle run).
        let msg6: [u8; 42] = [
            8, 7, 26, 63, 54, 40, 59, 58, 46, 38, 31, 23, 31, 10, 38, 63, 43, 57, 36, 9, 15,
            53, 21, 43, 19, 62, 53, 5, 9, 40, 43, 44, 63, 58, 8, 11, 34, 60, 8, 7, 39, 57,
        ];
        let bits = bits_of_hex(VO6);
        let mut cw6 = [0u8; RS6_N];
        for (i, c) in bits[..312].chunks(6).enumerate() {
            cw6[i] = c.iter().fold(0u8, |v, &b| (v << 1) | b);
        }
        assert_eq!(rs6_correct(&mut cw6), Ok(0));
        assert_eq!(&cw6[..42], &msg6);

        // And with a symbol error injected, it corrects.
        let mut cw6e = cw6;
        cw6e[7] ^= 0x15;
        assert_eq!(rs6_correct(&mut cw6e), Ok(1));
        assert_eq!(&cw6e[..42], &msg6);
    }

    #[test]
    fn short_payload_rejected() {
        assert!(classify_voice(&[0u8; 200]).is_none());
    }
}
