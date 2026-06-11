//! Validate a dumped RX burst row directly against our RS math.
use xng_mode_vdl2::interleave;

fn main() {
    let rx_path = std::env::args().nth(1).expect("rx file");
    let tl: usize = std::env::args().nth(2).expect("tl").parse().unwrap();
    let rx = std::fs::read(&rx_path).expect("rx");
    let msb = std::env::args().nth(3).map(|a| a == "msb").unwrap_or(false);
    let to_octets = move |bits: &[u8]| -> Vec<u8> {
        bits.chunks(8)
            .map(|c| {
                c.iter().enumerate().fold(0u8, |b, (i, &v)| {
                    b | (v << if msb { 7 - i } else { i })
                })
            })
            .collect()
    };
    let octets = to_octets(&rx);
    let data_octets = tl.div_ceil(8);
    eprintln!("rx {} bits -> {} octets ({} data + {} checks)",
        rx.len(), octets.len(), data_octets, octets.len() - data_octets);
    let rs = interleave::vdl2_rs();
    // Build the padded 255-octet codeword like deinterleave does.
    let mut cw = vec![0u8; 255];
    cw[..data_octets].copy_from_slice(&octets[..data_octets]);
    let k = octets.len() - data_octets;
    cw[249..249 + k].copy_from_slice(&octets[data_octets..]);
    let erasures: Vec<usize> = (249 + k..255).collect();
    match rs.correct(&mut cw, &erasures) {
        Ok(fixed) => eprintln!("RS VALID with {fixed} corrections"),
        Err(()) => eprintln!("RS INVALID under our convention"),
    }
}
