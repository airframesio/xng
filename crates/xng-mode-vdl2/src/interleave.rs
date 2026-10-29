//! VDL2 FEC blocking + interleaving (Annex 10 §6.4.3.1.2–6.4.3.1.3).
//!
//! Data bits pack into octets (first bit = MSB), octets fill a table of
//! c rows × 255 columns row-major (c = ⌈TL/1992⌉; short final row is
//! virtually zero-filled to 249 data octets). RS(255,249) check octets
//! occupy columns 250–255 per row, with shortening: rows of ≤2 real
//! octets transmit no checks, 3–30 transmit the first 2 (rest erased),
//! 31–67 the first 4, ≥68 all 6. Transmission reads column-by-column,
//! skipping virtual fill and non-transmitted checks.

use xng_dsp::rs::ReedSolomon;

const ROW_DATA_OCTETS: usize = 249;
const ROW_DATA_BITS: usize = ROW_DATA_OCTETS * 8;
const NPAR: usize = 6;

pub fn vdl2_rs() -> ReedSolomon {
    ReedSolomon::new(0x187, NPAR, 120)
}

/// Check octets transmitted for a row with `n` real data octets.
fn checks_for(n: usize) -> usize {
    match n {
        0..=2 => 0,
        3..=30 => 2,
        31..=67 => 4,
        _ => NPAR,
    }
}

pub struct Layout {
    /// Real data octets per row.
    pub rows: Vec<usize>,
    /// Transmitted check octets per row.
    pub checks: Vec<usize>,
    /// Total transmitted bits after the header.
    pub total_tx_bits: usize,
}

/// Compute the burst layout from the 17-bit transmission length.
pub fn layout(tl_bits: usize) -> Option<Layout> {
    if tl_bits == 0 || tl_bits > 131_071 {
        return None;
    }
    let total_octets = tl_bits.div_ceil(8);
    let c = tl_bits.div_ceil(ROW_DATA_BITS);
    let mut rows = Vec::with_capacity(c);
    let mut remaining = total_octets;
    for _ in 0..c {
        let n = remaining.min(ROW_DATA_OCTETS);
        rows.push(n);
        remaining -= n;
    }
    let checks: Vec<usize> = rows.iter().map(|&n| checks_for(n)).collect();
    let total_tx_bits = rows.iter().zip(&checks).map(|(d, k)| (d + k) * 8).sum();
    Some(Layout { rows, checks, total_tx_bits })
}

fn bits_to_octets(bits: &[u8]) -> Vec<u8> {
    // LSB-first: HDLC wire order. The RS code operates on octets
    // assembled this way (verified bit-true against dumpvdl2 on the
    // off-air GSIF bursts — MSB packing makes the RS reject perfect
    // codewords).
    bits.chunks(8)
        .map(|c| c.iter().enumerate().fold(0u8, |b, (i, &v)| b | (v << i)))
        .collect()
}

fn octets_to_bits(octets: &[u8], nbits: usize) -> Vec<u8> {
    octets
        .iter()
        .flat_map(|&o| (0..8).map(move |i| (o >> i) & 1))
        .take(nbits)
        .collect()
}

/// TX: data bits → transmitted post-header bit stream (RS-encoded,
/// interleaved).
pub fn interleave(data_bits: &[u8], rs: &ReedSolomon) -> Vec<u8> {
    let lay = layout(data_bits.len()).expect("valid TL");
    let octets = bits_to_octets(data_bits);
    // Build rows: data + virtual fill + all 6 checks.
    let mut grid: Vec<Vec<u8>> = Vec::with_capacity(lay.rows.len());
    let mut off = 0;
    for &n in &lay.rows {
        let mut row = vec![0u8; ROW_DATA_OCTETS];
        row[..n].copy_from_slice(&octets[off..off + n]);
        off += n;
        let checks = rs.encode(&row);
        row.extend(checks);
        grid.push(row);
    }
    // Column-major readout, skipping fill and untransmitted checks.
    let mut out_octets = Vec::new();
    for col in 0..255 {
        for (r, row) in grid.iter().enumerate() {
            let n = lay.rows[r];
            let k = lay.checks[r];
            let transmitted =
                (col < n) || (col >= ROW_DATA_OCTETS && col < ROW_DATA_OCTETS + k);
            if transmitted {
                out_octets.push(row[col]);
            }
        }
    }
    octets_to_bits(&out_octets, out_octets.len() * 8)
}

/// RX: transmitted post-header bits → corrected data bits (TL of them).
/// Returns (data_bits, rs_corrected_octets) or None if uncorrectable.
/// Hard-decision deinterleave (kept for loopback tests and as the
/// zero-confidence fallback).
pub fn deinterleave(tx_bits: &[u8], tl_bits: usize, rs: &ReedSolomon) -> Option<(Vec<u8>, usize)> {
    deinterleave_soft(tx_bits, &[], 0, tl_bits, rs).map(|(b, c, _)| (b, c))
}

/// Deinterleave with soft-decision erasure retries: each RS row is
/// first tried as-is; on failure, the transmitted octets with the
/// lowest decision confidence are marked as erasures and the row is
/// retried (RS(255,249) trades one error of budget for two erasures:
/// 2·errors + erasures ≤ 6, with untransmitted check octets already
/// consuming part of the budget). The AVLC FCS downstream rejects any
/// miscorrection this invites.
///
/// `sym_conf` holds |phase residual| per burst symbol (3 bits each);
/// `bit_offset` is `tx_bits[0]`'s position in the burst bit stream
/// (the header is not symbol-aligned). Empty conf = hard decisions.
pub fn deinterleave_soft(
    tx_bits: &[u8],
    sym_conf: &[f32],
    bit_offset: usize,
    tl_bits: usize,
    rs: &ReedSolomon,
) -> Option<(Vec<u8>, usize, bool)> {
    let lay = layout(tl_bits)?;
    if tx_bits.len() < lay.total_tx_bits {
        return None;
    }
    let octets = bits_to_octets(&tx_bits[..lay.total_tx_bits]);
    // Worst-bit confidence per TX octet (symbols are 3 bits; an octet
    // spans 2-4 symbols — take the largest residual touching it).
    let octet_conf: Vec<f32> = (0..octets.len())
        .map(|o| {
            let first_sym = (bit_offset + o * 8) / 3;
            let last_sym = (bit_offset + o * 8 + 7) / 3;
            (first_sym..=last_sym)
                .map(|s| sym_conf.get(s).copied().unwrap_or(0.0))
                .fold(0.0f32, f32::max)
        })
        .collect();

    let c = lay.rows.len();
    let mut grid: Vec<Vec<u8>> = vec![vec![0u8; 255]; c];
    let mut cgrid: Vec<Vec<f32>> = vec![vec![0.0f32; 255]; c];
    let mut it = octets.iter().zip(&octet_conf);
    for col in 0..255 {
        for r in 0..c {
            let n = lay.rows[r];
            let k = lay.checks[r];
            let transmitted =
                (col < n) || (col >= ROW_DATA_OCTETS && col < ROW_DATA_OCTETS + k);
            if transmitted {
                let (&o, &cf) = it.next()?;
                grid[r][col] = o;
                cgrid[r][col] = cf;
            }
        }
    }

    let mut corrected = 0usize;
    let mut soft_assisted = false;
    let mut data_octets = Vec::new();
    for (r, row) in grid.iter_mut().enumerate() {
        let n = lay.rows[r];
        let k = lay.checks[r];
        if k > 0 {
            // Untransmitted check octets are always erasures.
            let base: Vec<usize> = (ROW_DATA_OCTETS + k..255).collect();
            let budget = 6usize.saturating_sub(base.len());
            // Transmitted positions ranked least-confident first.
            let mut ranked: Vec<usize> = (0..255)
                .filter(|&col| (col < n) || (col >= ROW_DATA_OCTETS && col < ROW_DATA_OCTETS + k))
                .collect();
            ranked.sort_by(|&a, &b| {
                cgrid[r][b].partial_cmp(&cgrid[r][a]).unwrap_or(std::cmp::Ordering::Equal)
            });

            let mut done = false;
            // One erasure rung only: erasing the two least-confident
            // octets keeps a two-error margin (2e + f ≤ 6). Wider rungs
            // measurably hallucinate codewords (see demod notes).
            for extra in [0usize, 2] {
                if extra > budget {
                    break;
                }
                let mut attempt = row.clone();
                let mut erasures = base.clone();
                if extra > 0 {
                    // Only erase genuinely doubtful decisions (residual
                    // beyond ~half the decision region).
                    if ranked.len() < extra
                        || ranked.iter().take(extra).any(|&p| cgrid[r][p] < 0.20)
                    {
                        break;
                    }
                    erasures.extend(ranked.iter().take(extra).copied());
                }
                // Shortened code: columns n..249 are virtual zero fill and
                // must stay zero. A "correction" landing there is not a
                // valid shortened codeword — accepting it turns ~16% of
                // random rows into RS passes (false locks).
                let result = rs
                    .correct(&mut attempt, &erasures)
                    .ok()
                    .filter(|_| attempt[n..ROW_DATA_OCTETS].iter().all(|&o| o == 0));
                if let Some(fixed) = result {
                    corrected += fixed.saturating_sub(base.len());
                    if extra > 0 {
                        soft_assisted = true;
                    }
                    *row = attempt;
                    done = true;
                    break;
                }
            }
            if !done {
                return None;
            }
        }
        data_octets.extend_from_slice(&row[..n]);
    }
    Some((octets_to_bits(&data_octets, tl_bits), corrected, soft_assisted))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pattern_bits(n: usize, seed: u64) -> Vec<u8> {
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
    fn roundtrip_various_lengths() {
        let rs = vdl2_rs();
        // Cover all shortening regimes and multi-row interleaving:
        for tl in [10usize, 100, 600, 1500, 1992, 1993, 5000, 8000] {
            let data = pattern_bits(tl, tl as u64 + 1);
            let tx = interleave(&data, &rs);
            let lay = layout(tl).unwrap();
            assert_eq!(tx.len(), lay.total_tx_bits, "tl={tl}");
            let (back, fixed) = deinterleave(&tx, tl, &rs).expect("roundtrip");
            assert_eq!(back, data, "tl={tl}");
            assert_eq!(fixed, 0);
        }
    }

    #[test]
    fn corrects_octet_errors() {
        let rs = vdl2_rs();
        let data = pattern_bits(5000, 99);
        let mut tx = interleave(&data, &rs);
        // Corrupt a full octet worth of bits mid-stream (one symbol error
        // in one RS row after deinterleaving).
        for b in &mut tx[1000..1008] {
            *b ^= 1;
        }
        let (back, fixed) = deinterleave(&tx, 5000, &rs).expect("must correct");
        assert_eq!(back, data);
        assert!(fixed >= 1);
    }

    /// Single-row TX bits: for one row, column-major readout is just
    /// the real octets followed by the transmitted checks.
    fn single_row_tx(real: &[u8], checks: &[u8]) -> Vec<u8> {
        let k = checks_for(real.len());
        let mut tx = real.to_vec();
        tx.extend_from_slice(&checks[..k]);
        octets_to_bits(&tx, tx.len() * 8)
    }

    fn pattern_octets(n: usize, seed: u64) -> Vec<u8> {
        bits_to_octets(&pattern_bits(n * 8, seed))
    }

    #[test]
    fn rejects_correction_into_virtual_fill() {
        let rs = vdl2_rs();
        // Deterministic: a full-length codeword whose fill column n is
        // non-zero. RX zero-fills that column, so the received row is one
        // symbol away from this codeword (and ≥6 from any other); the
        // decoder "fixes" the fill octet. That is not a valid shortened
        // codeword and must be rejected.
        let n = 100;
        let real = pattern_octets(n, 7);
        let mut full = vec![0u8; ROW_DATA_OCTETS];
        full[..n].copy_from_slice(&real);
        full[n] = 0x5a;
        let checks = rs.encode(&full);
        let tx = single_row_tx(&real, &checks);
        assert!(deinterleave(&tx, n * 8, &rs).is_none());

        // Random sweep. With all 6 checks sent (n ≥ 68) and t = 3
        // corrections confined to the n+6 real positions, a random row
        // passes with probability ≈ C(n+6,3)·255³/256⁶ (≈0.6% at n=80,
        // ≈1.1% at n=100, ≈2.3% at n=127). Before the fix any of the 255
        // positions could be "corrected": ~16%.
        let mut s = 0x9e37_79b9_7f4a_7c15u64;
        let mut next = move || {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            s
        };
        let mut passes = 0;
        let mut total = 0;
        for n in [80usize, 100, 127] {
            let tl = n * 8;
            let nbits = layout(tl).unwrap().total_tx_bits;
            for _ in 0..1000 {
                let tx: Vec<u8> = (0..nbits).map(|_| (next() & 1) as u8).collect();
                total += 1;
                if deinterleave(&tx, tl, &rs).is_some() {
                    passes += 1;
                }
            }
        }
        assert!(
            passes * 100 <= total * 3,
            "random pass rate {passes}/{total}"
        );

        // Short rows (n=30: 2 checks sent, 4 erased → 1 correction left):
        // the [32,30] effective code accepts ≈ 32·255/256² ≈ 12.5% of
        // random rows inherently; unrestricted it was ≈ 98%.
        let mut passes = 0;
        let nbits = layout(30 * 8).unwrap().total_tx_bits;
        for _ in 0..1000 {
            let tx: Vec<u8> = (0..nbits).map(|_| (next() & 1) as u8).collect();
            if deinterleave(&tx, 30 * 8, &rs).is_some() {
                passes += 1;
            }
        }
        assert!(passes <= 200, "n=30 random pass rate {passes}/1000");
    }

    #[test]
    fn still_corrects_real_errors() {
        let rs = vdl2_rs();
        let n = 100;
        let real = pattern_octets(n, 11);
        let mut full = vec![0u8; ROW_DATA_OCTETS];
        full[..n].copy_from_slice(&real);
        let checks = rs.encode(&full);
        let want = octets_to_bits(&real, n * 8);

        // Three octet errors in real data positions: plain path.
        let mut bad = real.clone();
        for p in [3usize, 40, 99] {
            bad[p] ^= 0xa5;
        }
        let tx = single_row_tx(&bad, &checks);
        let (back, fixed) = deinterleave(&tx, n * 8, &rs).expect("3 errors correctable");
        assert_eq!(back, want);
        assert_eq!(fixed, 3);

        // Four errors (beyond t=3); two flagged low-confidence → the
        // erasure rung (2 erasures + 2 errors = 6) recovers the row.
        let mut bad = real.clone();
        for p in [10usize, 50, 70, 90] {
            bad[p] ^= 0x3c;
        }
        let tx = single_row_tx(&bad, &checks);
        assert!(deinterleave(&tx, n * 8, &rs).is_none());
        let mut conf = vec![0.0f32; tx.len() / 3 + 1];
        for o in [10usize, 50] {
            // A symbol wholly inside octet o, so neighbours stay at 0.
            conf[(o * 8 + 3) / 3] = 0.5;
        }
        let (back, _, soft) =
            deinterleave_soft(&tx, &conf, 0, n * 8, &rs).expect("erasure-assisted");
        assert_eq!(back, want);
        assert!(soft);
    }

    #[test]
    fn shortening_rules() {
        assert_eq!(layout(16).unwrap().checks, vec![0]); // 2 octets
        assert_eq!(layout(17).unwrap().checks, vec![2]); // 3 octets
        assert_eq!(layout(30 * 8).unwrap().checks, vec![2]);
        assert_eq!(layout(31 * 8).unwrap().checks, vec![4]);
        assert_eq!(layout(67 * 8).unwrap().checks, vec![4]);
        assert_eq!(layout(68 * 8).unwrap().checks, vec![6]);
        let l = layout(1992 + 24).unwrap(); // 2 rows: 249 + 3 octets
        assert_eq!(l.rows, vec![249, 3]);
        assert_eq!(l.checks, vec![6, 2]);
    }
}
