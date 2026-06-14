//! Iridium ITL ("TL", Time-Location) payload decode: satellite, orbital
//! plane, and message type from the descrambled PRS-coded payload.
//!
//! Method (iridium-toolkit `bitsparser`/`itl.py`, facts reimplemented):
//! the 768-bit payload after the 96-bit `11`+0… header is read as 384
//! DQPSK symbols, split into an I and a Q channel (gray map 0→00, 1→10,
//! 2→11, 3→01). The I channel carries a 128-bit PRS header (the protocol
//! version) then a 256-bit PRS plane code; the Q channel carries four
//! 96-bit PRS message codes. Each field is matched to the nearest known
//! PRS sequence (Hamming), tolerating off-air bit errors since the
//! sequences are pseudo-random and far apart. `map_sat` resolves the
//! first message code to a satellite / message label.
//!
//! Our demodulator differential-decodes to bits (`DQPSK_MAP`); ITL needs
//! the absolute symbols, recovered here by inverting that map and
//! integrating — validated against a real off-air frame whose I header
//! matched `PRS_HDR[2]` exactly and whose plane + all four message codes
//! resolved (sat S09, plane 2, M04).

use crate::itl_tables::{PRS_HDR, PRS_LIST, PRS_PLANES};

/// Inverse of demod `DQPSK_MAP = [0,2,3,1]` (mapped symbol → differential).
const INV_DQPSK: [u8; 4] = [0, 3, 1, 2];

/// One decoded ITL frame.
#[derive(Debug, Clone, PartialEq)]
pub struct ItlFrame {
    pub version: u8,
    pub plane: Option<u8>,
    pub sat: Option<String>,
    pub msg_type: Option<String>,
    /// The four PRS message codes (0–127) and their types (0–3).
    pub msg: [Option<u8>; 4],
    pub types: [Option<u8>; 4],
}

/// Pack an MSB-first bit slice (≤256 bits) into (high128, low128).
fn pack(bits: &[u8]) -> (u128, u128) {
    let n = bits.len();
    let (mut hi, mut lo) = (0u128, 0u128);
    for (i, &b) in bits.iter().enumerate() {
        if b == 0 {
            continue;
        }
        let pos = n - 1 - i;
        if pos >= 128 {
            hi |= 1u128 << (pos - 128);
        } else {
            lo |= 1u128 << pos;
        }
    }
    (hi, lo)
}

fn hamming(a: (u128, u128), b: (u128, u128)) -> u32 {
    (a.0 ^ b.0).count_ones() + (a.1 ^ b.1).count_ones()
}

/// Index of the nearest table entry within `max_dist` bits, if any.
fn nearest(val: (u128, u128), table: &[(u128, u128)], max_dist: u32) -> Option<usize> {
    let mut best = (u32::MAX, 0usize);
    for (i, &e) in table.iter().enumerate() {
        let d = hamming(val, e);
        if d < best.0 {
            best = (d, i);
        }
    }
    (best.0 <= max_dist).then_some(best.1)
}

/// Map a PRS message code + protocol version to (satellite, message)
/// labels (iridium-toolkit `itl.map_sat`).
fn map_sat(num: u8, version: u8) -> Option<(String, String)> {
    let n = num as i32;
    match version {
        2 => Some(if n == 77 {
            ("---".into(), "M08".into())
        } else if n < 66 {
            (format!("S{:02}", n % 11 + 1), format!("M{:02}", n / 11 + 1))
        } else if (82..=84).contains(&n) {
            (format!("R{:02}", (n - 82) % 3 + 1), "N01".into())
        } else if (85..=95).contains(&n) {
            (format!("S{:02}", n - 84), "N02".into())
        } else if (96..=107).contains(&n) {
            (format!("R{:02}", (n - 96) % 3 + 1), format!("N{:02}", (n - 96) / 3 + 3))
        } else if n == 108 {
            ("---".into(), "SSS".into())
        } else if n == 111 {
            ("---".into(), "N08".into())
        } else {
            ("---".into(), format!("{:03}", n))
        }),
        1 if n < 88 => Some((format!("S{:02}", n % 11 + 1), format!("M{:02}", n / 11 + 1))),
        _ => None,
    }
}

/// Decode the ITL payload (the ≥768 bits after the 96-bit header).
pub fn decode_itl(payload: &[u8]) -> Option<ItlFrame> {
    if payload.len() < 768 {
        return None;
    }
    let p = &payload[..768];
    // Recover the 384 absolute symbols from our differential bits.
    let mut ich = [0u8; 384];
    let mut qch = [0u8; 384];
    let mut acc = 0u8;
    for k in 0..384 {
        let m = (p[2 * k] << 1) | p[2 * k + 1];
        acc = (acc + INV_DQPSK[m as usize]) % 4;
        // split_qpsk gray map: 0→(0,0) 1→(1,0) 2→(1,1) 3→(0,1)
        let (i, q) = match acc {
            0 => (0, 0),
            1 => (1, 0),
            2 => (1, 1),
            _ => (0, 1),
        };
        ich[k] = i;
        qch[k] = q;
    }
    // Protocol version from the 128-bit I header; reject version 0 (the
    // all-zero PRS_HDR — an idle/placeholder frame, no satellite).
    let version = nearest(pack(&ich[0..128]), &PRS_HDR, 40)? as u8;
    if version == 0 {
        return None;
    }
    let plane = nearest(pack(&ich[128..384]), &PRS_PLANES, 64).map(|i| (i + 1) as u8);
    let mut msg = [None; 4];
    let mut types = [None; 4];
    for k in 0..4 {
        if let Some(j) = nearest(pack(&qch[96 * k..96 * k + 96]), &PRS_LIST, 24) {
            msg[k] = Some((j % 128) as u8);
            types[k] = Some((j / 128) as u8);
        }
    }
    let (sat, msg_type) = match msg[0] {
        Some(m) => match map_sat(m, version) {
            Some((s, t)) => (Some(s), Some(t)),
            None => (None, None),
        },
        None => (None, None),
    };
    Some(ItlFrame { version, plane, sat, msg_type, msg, types })
}
