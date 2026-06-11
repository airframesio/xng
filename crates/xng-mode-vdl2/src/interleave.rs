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
    bits.chunks(8)
        .map(|c| c.iter().enumerate().fold(0u8, |b, (i, &v)| b | (v << (7 - i))))
        .collect()
}

fn octets_to_bits(octets: &[u8], nbits: usize) -> Vec<u8> {
    octets
        .iter()
        .flat_map(|&o| (0..8).rev().map(move |i| (o >> i) & 1))
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
                if let Ok(fixed) = rs.correct(&mut attempt, &erasures) {
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
