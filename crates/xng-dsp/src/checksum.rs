//! CRC variants used across the decode cores.
//!
//! - ACARS (ARINC 618) block check: CRC-16/KERMIT (CCITT poly 0x1021,
//!   reflected, init 0).
//! - HDLC/AVLC FCS (VDL2, AIS): CRC-16/X-25 (poly 0x1021, reflected,
//!   init 0xFFFF, xorout 0xFFFF).
//! - CRC-16/CCITT-FALSE kept for modes that use the unreflected variant.

use crc::{Crc, CRC_16_IBM_SDLC, CRC_16_KERMIT, CRC_16_IBM_3740};

/// Mode S CRC-24 (ICAO Annex 10 Vol IV), generator 0xFFF409, MSB-first,
/// no init/xorout. A valid frame whose parity field is not overlaid with
/// an address leaves remainder 0; address-overlaid frames leave the
/// interrogator/aircraft address as the remainder.
pub fn mode_s_crc(data: &[u8]) -> u32 {
    const POLY: u32 = 0xFF_F409;
    let mut crc: u32 = 0;
    for &b in data {
        crc ^= (b as u32) << 16;
        for _ in 0..8 {
            crc <<= 1;
            if crc & 0x100_0000 != 0 {
                crc ^= POLY;
            }
        }
    }
    crc & 0xFF_FFFF
}

pub const ACARS_CRC: Crc<u16> = Crc::<u16>::new(&CRC_16_KERMIT);
pub const HDLC_FCS: Crc<u16> = Crc::<u16>::new(&CRC_16_IBM_SDLC);
pub const CCITT_FALSE: Crc<u16> = Crc::<u16>::new(&CRC_16_IBM_3740);

pub fn acars_crc(data: &[u8]) -> u16 {
    ACARS_CRC.checksum(data)
}

pub fn hdlc_fcs(data: &[u8]) -> u16 {
    HDLC_FCS.checksum(data)
}

/// Verify an HDLC frame whose last two bytes are the transmitted FCS
/// (little-endian on the wire).
pub fn hdlc_frame_ok(frame_with_fcs: &[u8]) -> bool {
    if frame_with_fcs.len() < 3 {
        return false;
    }
    let (payload, fcs) = frame_with_fcs.split_at(frame_with_fcs.len() - 2);
    hdlc_fcs(payload) == u16::from_le_bytes([fcs[0], fcs[1]])
}

#[cfg(test)]
mod tests {
    use super::*;

    const CHECK: &[u8] = b"123456789";

    #[test]
    fn known_check_values() {
        // Catalogue check values for "123456789"
        assert_eq!(acars_crc(CHECK), 0x2189); // KERMIT
        assert_eq!(hdlc_fcs(CHECK), 0x906E); // X-25 / IBM-SDLC
        assert_eq!(CCITT_FALSE.checksum(CHECK), 0x29B1);
    }

    #[test]
    fn arinc_618_worked_example() {
        // ARINC 618 §2.2.10: the string "K7" with odd parity = octets
        // 0xCB 0x37; the spec gives the BCS as 3E 6B (MSB-first), i.e.
        // CRC value 0x6B3E, low byte transmitted first.
        assert_eq!(acars_crc(&[0xCB, 0x37]), 0x6B3E);
        // Verification residue: CRC over message + BCS bytes (low first) == 0
        assert_eq!(acars_crc(&[0xCB, 0x37, 0x3E, 0x6B]), 0x0000);
    }

    #[test]
    fn mode_s_crc_on_published_frames() {
        // Extended squitter examples from the open Mode S literature
        // ("The 1090 MHz Riddle"): parity is the last 3 bytes, so the
        // remainder over the full frame is 0.
        let id_frame: [u8; 14] = [
            0x8D, 0x48, 0x40, 0xD6, 0x20, 0x2C, 0xC3, 0x71, 0xC3, 0x2C, 0xE0, 0x57, 0x60, 0x98,
        ];
        assert_eq!(mode_s_crc(&id_frame), 0);
        let pos_frame: [u8; 14] = [
            0x8D, 0x40, 0x62, 0x1D, 0x58, 0xC3, 0x82, 0xD6, 0x90, 0xC8, 0xAC, 0x28, 0x63, 0xA7,
        ];
        assert_eq!(mode_s_crc(&pos_frame), 0);
        // Corruption must produce a nonzero remainder.
        let mut bad = id_frame;
        bad[5] ^= 0x20;
        assert_ne!(mode_s_crc(&bad), 0);
    }

    #[test]
    fn hdlc_frame_verification() {
        let mut frame = CHECK.to_vec();
        let fcs = hdlc_fcs(CHECK);
        frame.extend_from_slice(&fcs.to_le_bytes());
        assert!(hdlc_frame_ok(&frame));
        frame[0] ^= 0x01;
        assert!(!hdlc_frame_ok(&frame));
    }
}
