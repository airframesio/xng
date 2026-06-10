//! Emit a generated IRA bit stream + our decode as JSON (oracle harness).

use xng_mode_iridium::{decode_bits, frame};

fn push_field(bits: &mut Vec<u8>, v: u32, n: usize) {
    for k in (0..n).rev() {
        bits.push(((v >> k) & 1) as u8);
    }
}

fn main() {
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
