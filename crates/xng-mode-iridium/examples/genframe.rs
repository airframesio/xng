//! Emit a generated IRA bit stream + our decode as JSON (oracle harness).

use xng_mode_iridium::{decode_bits, frame};

fn push_field(bits: &mut Vec<u8>, v: u32, n: usize) {
    for k in (0..n).rev() {
        bits.push(((v >> k) & 1) as u8);
    }
}

fn main() {
    if std::env::args().nth(1).as_deref() == Some("ims") {
        // Single-part ASCII page (matches the e2e test vector).
        let ric: u32 = 1234567;
        let text = "CALL OPS +14155550100";
        let mut rest: Vec<u8> = Vec::new();
        for k in 0..22 {
            rest.push(((ric >> k) & 1) as u8);
        }
        push_field(&mut rest, 5, 5);
        push_field(&mut rest, 7, 6);
        push_field(&mut rest, 0, 4);
        push_field(&mut rest, 0, 6);
        push_field(&mut rest, 0, 4);
        rest.push(0);
        rest.push(0);
        push_field(&mut rest, 0, 7);
        for c in text.bytes() {
            push_field(&mut rest, c as u32, 7);
        }
        push_field(&mut rest, 3, 7);
        let mut blocks: Vec<Vec<u8>> = Vec::new();
        for chunk in rest.chunks(20) {
            let mut b = vec![0u8];
            b.extend_from_slice(chunk);
            b.resize(21, 0);
            blocks.push(b);
        }
        let total_halves = 1 + blocks.len();
        let bch_blocks = (total_halves + 1) / 2;
        let mut h = Vec::new();
        h.push(0);
        push_field(&mut h, 0, 4);
        push_field(&mut h, 3, 4);
        push_field(&mut h, 9, 6);
        push_field(&mut h, bch_blocks as u32, 4);
        push_field(&mut h, 1, 2);
        let mut all = vec![h];
        all.extend(blocks);
        if all.len() % 2 == 1 {
            all.push(vec![1u8; 21]);
        }
        let enc: Vec<Vec<u8>> =
            all.iter().map(|d| frame::bch_encode(frame::MESSAGING_BCH_POLY, d)).collect();
        let mut bits: Vec<u8> = frame::ACCESS_DL.to_vec();
        bits.extend(frame::HEADER_MESSAGING.iter().copied());
        for pair in enc.chunks_exact(2) {
            bits.extend(frame::interleave2(&pair[0], &pair[1]));
        }
        let bitstr: String = bits.iter().map(|&b| char::from(b'0' + b)).collect();
        let f = decode_bits(&bits).expect("decodes");
        println!("{bitstr}");
        println!("{}", f.details);
        return;
    }
    if std::env::args().nth(1).as_deref() == Some("da") {
        let mut payload = [0u8; 20];
        for (i, b) in payload.iter_mut().enumerate() {
            *b = (i as u8) * 11 + 5;
        }
        let mut bits: Vec<u8> = frame::ACCESS_DL.to_vec();
        bits.extend(frame::encode_lcw(2, 0, 0x1FF));
        bits.extend(frame::encode_da_payload(&frame::build_da_bits(false, 0, 20, &payload)));
        let bitstr: String = bits.iter().map(|&b| char::from(b'0' + b)).collect();
        let (da, _) = xng_mode_iridium::decode_da_bits(&bits).expect("decodes");
        println!("{bitstr}");
        println!("{{\"ctr\":{},\"len\":{},\"crc_ok\":{},\"data\":\"{}\"}}",
            da.ctr, da.len, da.crc_ok,
            da.data.iter().map(|b| format!("{b:02x}")).collect::<String>());
        return;
    }
    let mut d = Vec::new();
    push_field(&mut d, 75, 7); // sat
    push_field(&mut d, 21, 6); // beam
    for v in [-1411i32, 263, 1602] {
        let sign = if v < 0 { 1 } else { 0 };
        let mag = if v < 0 { v + (1 << 11) } else { v } as u32;
        push_field(&mut d, sign, 1);
        push_field(&mut d, mag, 11);
    }
    push_field(&mut d, 33, 7);
    push_field(&mut d, 0, 1);
    push_field(&mut d, 1, 1);
    push_field(&mut d, 22, 5);
    for tmsi in [0xCAFED00Du32, 0x00C0FFEE] {
        push_field(&mut d, tmsi, 32);
        push_field(&mut d, 0, 2);
        push_field(&mut d, 7, 5);
        push_field(&mut d, 0, 3);
    }
    d.extend(std::iter::repeat(1).take(42));

    let mut padded = d.clone();
    while padded.len() % 21 != 0 {
        padded.push(0);
    }
    let mut nblk = padded.len() / 21;
    while (nblk - 3) % 2 != 0 {
        padded.extend(std::iter::repeat(0).take(21));
        nblk += 1;
    }
    let blocks: Vec<Vec<u8>> = padded
        .chunks_exact(21)
        .map(|x| frame::bch_encode(frame::RINGALERT_BCH_POLY, x))
        .collect();
    let mut bits: Vec<u8> = frame::ACCESS_DL.to_vec();
    bits.extend(frame::interleave3(&blocks[0], &blocks[1], &blocks[2]));
    for pair in blocks[3..].chunks_exact(2) {
        bits.extend(frame::interleave2(&pair[0], &pair[1]));
    }

    let bitstr: String = bits.iter().map(|&b| char::from(b'0' + b)).collect();
    let ours = decode_bits(&bits).expect("decodes");
    println!("{bitstr}");
    println!("{}", serde_json::to_string(&ours.details).unwrap());
}
