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
pub fn deinterleave(tx_bits: &[u8], tl_bits: usize, rs: &ReedSolomon) -> Option<(Vec<u8>, usize)> {
    let lay = layout(tl_bits)?;
    if tx_bits.len() < lay.total_tx_bits {
        return None;
    }
    let octets = bits_to_octets(&tx_bits[..lay.total_tx_bits]);

    let c = lay.rows.len();
    let mut grid: Vec<Vec<u8>> = vec![vec![0u8; 255]; c];
    let mut it = octets.iter();
    for col in 0..255 {
        for r in 0..c {
            let n = lay.rows[r];
            let k = lay.checks[r];
            let transmitted =
                (col < n) || (col >= ROW_DATA_OCTETS && col < ROW_DATA_OCTETS + k);
            if transmitted {
                grid[r][col] = *it.next()?;
            }
        }
    }

    let mut corrected = 0usize;
    let mut data_octets = Vec::new();
    for (r, row) in grid.iter_mut().enumerate() {
        let n = lay.rows[r];
        let k = lay.checks[r];
        if k > 0 {
            // Untransmitted check octets are erasures.
            let erasures: Vec<usize> = (ROW_DATA_OCTETS + k..255).collect();
            match rs.correct(row, &erasures) {
                Ok(fixed) => corrected += fixed.saturating_sub(erasures.len()),
                Err(()) => return None,
            }
        }
        data_octets.extend_from_slice(&row[..n]);
    }
    Some((octets_to_bits(&data_octets, tl_bits), corrected))
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
