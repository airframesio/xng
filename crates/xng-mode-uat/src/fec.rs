//! UAT Reed-Solomon FEC (DO-282B §2.4.4).
//!
//! UAT uses RS over GF(2^8) with the primitive polynomial p(x)=0x187 and a
//! generator whose first consecutive root is α^120 — identical field/root
//! parameters to those FlightAware's dump978 passes to libfec
//! (`init_rs_char(8, 0x187, 120, 1, nroots, pad)`; see dump978 `fec.cc` /
//! `uat_protocol.h`). The three code lengths are *shortened* RS codes:
//!
//! | message          | code        | data | parity (nroots) |
//! |------------------|-------------|------|-----------------|
//! | downlink short   | RS(30, 18)  | 18   | 12              |
//! | downlink long    | RS(48, 34)  | 34   | 14              |
//! | uplink block     | RS(92, 72)  | 72   | 20              |
//!
//! An uplink frame is six RS(92,72) blocks byte-interleaved
//! (block-of-six), so byte `i*6 + b` of the 552-byte raw frame belongs to
//! block `b` (DO-282B §2.4.4.2 / dump978 `FEC::CorrectUplink`).
//!
//! The codec itself is [`xng_dsp::rs::ReedSolomon`], which implements the
//! full 255-symbol code; a shortened code is the same code with the leading
//! (high-degree) symbols held at zero, so for *encoding* we feed only the
//! real data bytes (leading zeros never change the systematic remainder)
//! and for *correcting* we virtual-zero-fill the front to 255.

use xng_dsp::rs::ReedSolomon;

/// p(x) = x^8 + x^7 + x^2 + x + 1.
pub const POLY: u16 = 0x187;
/// First consecutive generator root exponent (α^120).
pub const FIRST_ROOT: u32 = 120;

pub const DOWNLINK_SHORT_DATA: usize = 18;
pub const DOWNLINK_SHORT_PARITY: usize = 12;
pub const DOWNLINK_SHORT_BLOCK: usize = DOWNLINK_SHORT_DATA + DOWNLINK_SHORT_PARITY; // 30

pub const DOWNLINK_LONG_DATA: usize = 34;
pub const DOWNLINK_LONG_PARITY: usize = 14;
pub const DOWNLINK_LONG_BLOCK: usize = DOWNLINK_LONG_DATA + DOWNLINK_LONG_PARITY; // 48

pub const UPLINK_BLOCK_DATA: usize = 72;
pub const UPLINK_BLOCK_PARITY: usize = 20;
pub const UPLINK_BLOCK: usize = UPLINK_BLOCK_DATA + UPLINK_BLOCK_PARITY; // 92
pub const UPLINK_BLOCKS_PER_FRAME: usize = 6;
pub const UPLINK_FRAME_BYTES: usize = UPLINK_BLOCK * UPLINK_BLOCKS_PER_FRAME; // 552
pub const UPLINK_DATA_BYTES: usize = UPLINK_BLOCK_DATA * UPLINK_BLOCKS_PER_FRAME; // 432

/// Systematically RS-encode a shortened UAT block: returns the `nparity`
/// check octets for `data` (transmission order, highest-degree first).
///
/// Leading high-degree symbols of a shortened code are held at zero and
/// have no effect on the remainder, so the parity over the full 255-symbol
/// code equals the parity over just `data`.
fn encode_short(data: &[u8], nparity: usize) -> Vec<u8> {
    let rs = ReedSolomon::new(POLY, nparity, FIRST_ROOT);
    rs.encode(data)
}

pub fn encode_downlink_short(payload: &[u8]) -> Vec<u8> {
    assert_eq!(payload.len(), DOWNLINK_SHORT_DATA);
    encode_short(payload, DOWNLINK_SHORT_PARITY)
}

pub fn encode_downlink_long(payload: &[u8]) -> Vec<u8> {
    assert_eq!(payload.len(), DOWNLINK_LONG_DATA);
    encode_short(payload, DOWNLINK_LONG_PARITY)
}

pub fn encode_uplink_block(payload: &[u8]) -> Vec<u8> {
    assert_eq!(payload.len(), UPLINK_BLOCK_DATA);
    encode_short(payload, UPLINK_BLOCK_PARITY)
}

/// Correct one shortened RS block in place. `block` is `data || parity`
/// of length `data_len + nparity`. Returns the number of corrected symbols,
/// or `Err` if uncorrectable. The block is mapped to a full 255-symbol
/// codeword by virtual zero-fill of the leading `255 - block.len()`
/// positions (the high-degree pad of a shortened code).
fn correct_block(block: &mut [u8], data_len: usize, nparity: usize) -> Result<usize, ()> {
    let block_len = data_len + nparity;
    debug_assert_eq!(block.len(), block_len);
    let pad = 255 - block_len;
    let mut cw = vec![0u8; 255];
    cw[pad..].copy_from_slice(block);
    let rs = ReedSolomon::new(POLY, nparity, FIRST_ROOT);
    let n = rs.correct(&mut cw, &[])?;
    block.copy_from_slice(&cw[pad..]);
    Ok(n)
}

/// Decode result for a downlink frame.
pub struct DownlinkCorrection {
    /// Corrected payload (18 or 34 bytes), parity stripped.
    pub payload: Vec<u8>,
    /// Number of RS symbols corrected.
    pub errors: usize,
}

/// Correct a downlink frame given the full transmitted block
/// (`data || parity`, 30 or 48 bytes). The caller already knows the length;
/// UAT distinguishes short vs long by the MDB type in the header
/// (`payload[0] >> 3`: 0 ⇒ short), but at the FEC layer we go by length.
pub fn correct_downlink(block: &[u8]) -> Result<DownlinkCorrection, ()> {
    match block.len() {
        DOWNLINK_SHORT_BLOCK => {
            let mut buf = block.to_vec();
            let errors = correct_block(&mut buf, DOWNLINK_SHORT_DATA, DOWNLINK_SHORT_PARITY)?;
            buf.truncate(DOWNLINK_SHORT_DATA);
            Ok(DownlinkCorrection { payload: buf, errors })
        }
        DOWNLINK_LONG_BLOCK => {
            let mut buf = block.to_vec();
            let errors = correct_block(&mut buf, DOWNLINK_LONG_DATA, DOWNLINK_LONG_PARITY)?;
            buf.truncate(DOWNLINK_LONG_DATA);
            Ok(DownlinkCorrection { payload: buf, errors })
        }
        _ => Err(()),
    }
}

/// Correct an uplink frame: 552 interleaved bytes ⇒ 432 data bytes.
///
/// Deinterleave the six byte-interleaved RS(92,72) blocks, correct each,
/// and concatenate the 72-byte data sections in block order.
pub fn correct_uplink(frame: &[u8]) -> Result<(Vec<u8>, usize), ()> {
    if frame.len() != UPLINK_FRAME_BYTES {
        return Err(());
    }
    let mut data = Vec::with_capacity(UPLINK_DATA_BYTES);
    let mut total_errors = 0usize;
    for b in 0..UPLINK_BLOCKS_PER_FRAME {
        let mut block = vec![0u8; UPLINK_BLOCK];
        for (i, slot) in block.iter_mut().enumerate() {
            *slot = frame[i * UPLINK_BLOCKS_PER_FRAME + b];
        }
        let n = correct_block(&mut block, UPLINK_BLOCK_DATA, UPLINK_BLOCK_PARITY)?;
        total_errors += n;
        data.extend_from_slice(&block[..UPLINK_BLOCK_DATA]);
    }
    Ok((data, total_errors))
}

/// Byte-interleave six RS(92,72) blocks (each `data || parity`) into a
/// 552-byte uplink frame — the inverse of [`correct_uplink`]'s
/// deinterleave. Used to build test frames from clean payloads.
pub fn interleave_uplink(blocks: &[[u8; UPLINK_BLOCK]; UPLINK_BLOCKS_PER_FRAME]) -> Vec<u8> {
    let mut frame = vec![0u8; UPLINK_FRAME_BYTES];
    for (b, block) in blocks.iter().enumerate() {
        for (i, &byte) in block.iter().enumerate() {
            frame[i * UPLINK_BLOCKS_PER_FRAME + b] = byte;
        }
    }
    frame
}
