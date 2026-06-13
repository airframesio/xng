//! IDA/SBD → ACARS chain: build DA fragments carrying an SBD-framed
//! ACARS block, run them through the frame decode and reassembler, and
//! get the ACARS message out.

use xng_mode_iridium::frame;
use xng_mode_iridium::sbd::SbdReassembler;

/// Build the full transmitted bit stream of one DA burst.
fn da_burst_bits(cont: bool, ctr: u8, len: u8, payload: &[u8; 20]) -> Vec<u8> {
    let mut bits: Vec<u8> = frame::ACCESS_DL.to_vec();
    bits.extend(frame::encode_lcw(2, 0, 0x1FF));
    bits.extend(frame::encode_da_payload(&frame::build_da_bits(cont, ctr, len, payload)));
    bits
}

#[test]
fn da_roundtrip() {
    let mut payload = [0u8; 20];
    for (i, b) in payload.iter_mut().enumerate() {
        *b = (i as u8) * 7 + 3;
    }
    let bits = da_burst_bits(true, 3, 20, &payload);
    let (da, _) = xng_mode_iridium::decode_da_bits(&bits).expect("DA decodes");
    assert!(da.continuation);
    assert_eq!(da.ctr, 3);
    assert_eq!(da.len, 20);
    assert_eq!(da.data, payload);
    assert!(da.crc_ok, "CRC must verify");
}

#[test]
fn da_decodes_with_lcw_bit_errors() {
    // Real off-air LCWs carry a few bit errors. The LCW BCH corrects them,
    // so decode_lcw_bits must accept the burst rather than gate on the
    // strict zero-syndrome classify() (which drops it as Unknown). Without
    // the tolerant gate this burst does not decode at all.
    let mut payload = [0u8; 20];
    for (i, b) in payload.iter_mut().enumerate() {
        *b = (i as u8) * 5 + 1;
    }
    let mut bits = da_burst_bits(false, 1, 12, &payload);
    // Flip two bits inside the 46-bit LCW (data positions 0..46, i.e. bit
    // indices 24..70) — within the BCH's correction reach.
    bits[24 + 2] ^= 1;
    bits[24 + 40] ^= 1;
    let (da, _) =
        xng_mode_iridium::decode_da_bits(&bits).expect("DA with LCW bit errors still decodes");
    assert_eq!(da.ctr, 1);
    assert_eq!(da.len, 12);
    assert_eq!(da.data, payload);
    assert!(da.crc_ok, "payload CRC is unaffected by the LCW errors");
}

#[test]
fn sbd_acars_end_to_end() {
    // A short ACARS block (standard SOH..DEL, built by xng-acars).
    let block = xng_acars::block::build(
        '2', "N321AB", None, "Q0", '5', Some("M01A"), Some("UA1234"), "", false,
    );
    // SBD transport: 0x0600 HELLO type with the 0x20 29-byte prehdr,
    // then the ACARS payload.
    let mut l2: Vec<u8> = vec![0x06, 0x00];
    let mut prehdr = vec![0u8; 29];
    prehdr[0] = 0x20;
    prehdr[15] = 1; // msgcnt
    l2.extend_from_slice(&prehdr);
    l2.extend_from_slice(&block);

    // Fragment into 20-byte DA frames.
    let mut reasm = SbdReassembler::new();
    let chunks: Vec<&[u8]> = l2.chunks(20).collect();
    let mut result = None;
    for (i, chunk) in chunks.iter().enumerate() {
        let mut payload = [0u8; 20];
        payload[..chunk.len()].copy_from_slice(chunk);
        let cont = i + 1 < chunks.len();
        let bits = da_burst_bits(cont, (i % 8) as u8, chunk.len() as u8, &payload);
        let (da, _) = xng_mode_iridium::decode_da_bits(&bits).expect("fragment decodes");
        assert!(da.crc_ok);
        if let Some(msg) = reasm.push(&da, i as f64 * 0.09) {
            result = Some(msg);
        }
    }
    let msg = result.expect("SBD message assembled");
    let acars = msg.acars.expect("ACARS extracted");
    assert!(acars.crc_ok);
    assert_eq!(acars.core.tail.as_deref(), Some("N321AB"));
    assert_eq!(acars.core.label, "Q0");
    assert_eq!(acars.core.flight.as_deref(), Some("UA1234"));
}

#[test]
fn oracle_validated_da_vector() {
    // iridium-toolkit's bitsparser decodes this exact bit stream as:
    //   IDA: ... LCW(2,T:maint,...) cont=0 ctr=000 len=20
    //        [05.10.1b.26.31.3c.47.52.5d.68.73.7e.89.94.9f.aa.b5.c0.cb.d6]
    //        7b3a/0000 CRC:OK
    let bitstr = "0011000000110000111100111100110011001100100000000001001100000010001100101110110001100111001001000000000100010100000001000100000000010010011110001010000001101000101111001001001010010000111111010000010100110001001011001000001010111101110111110101100001101101011010010011100001001011001111101001100011100110010101100101011011010101001000111000001100010001101111101010010110101100110000";
    let bits: Vec<u8> = bitstr.bytes().map(|b| b - b'0').collect();
    let (da, _) = xng_mode_iridium::decode_da_bits(&bits).expect("decodes");
    assert!(!da.continuation);
    assert_eq!(da.ctr, 0);
    assert_eq!(da.len, 20);
    assert!(da.crc_ok);
    let hex: String = da.data.iter().map(|b| format!("{b:02x}")).collect();
    assert_eq!(hex, "05101b26313c47525d68737e89949faab5c0cbd6");
}
