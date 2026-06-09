//! CRC variants used across the decode cores.
//!
//! - ACARS (ARINC 618) block check: CRC-16/KERMIT (CCITT poly 0x1021,
//!   reflected, init 0).
//! - HDLC/AVLC FCS (VDL2, AIS): CRC-16/X-25 (poly 0x1021, reflected,
//!   init 0xFFFF, xorout 0xFFFF).
//! - CRC-16/CCITT-FALSE kept for modes that use the unreflected variant.

use crc::{Crc, CRC_16_IBM_SDLC, CRC_16_KERMIT, CRC_16_IBM_3740};

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
    fn hdlc_frame_verification() {
        let mut frame = CHECK.to_vec();
        let fcs = hdlc_fcs(CHECK);
        frame.extend_from_slice(&fcs.to_le_bytes());
        assert!(hdlc_frame_ok(&frame));
        frame[0] ^= 0x01;
        assert!(!hdlc_frame_ok(&frame));
    }
}
