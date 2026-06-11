//! Error-position forensics: expected TX bits (ground-truth data
//! through RS+interleave) vs dumped received bits.
use xng_mode_vdl2::interleave;

fn main() {
    let truth_path = std::env::args().nth(1).expect("truth file");
    let rx_path = std::env::args().nth(2).expect("rx file");
    let truth = std::fs::read(&truth_path).expect("truth");
    let rx = std::fs::read(&rx_path).expect("rx");
    let rs = interleave::vdl2_rs();
    let tx = interleave::interleave(&truth, &rs);
    let n = tx.len().min(rx.len());
    eprintln!("tx {} bits, rx {} bits", tx.len(), rx.len());
    let mut errs = Vec::new();
    for i in 0..n {
        if tx[i] != rx[i] {
            errs.push(i);
        }
    }
    eprintln!("{} bit errors of {}", errs.len(), n);
    // Map to symbols (3 bits each) and report position + bit-in-symbol.
    let mut last = -10i64;
    for &e in &errs {
        let sym = e / 3;
        let bit = e % 3;
        let gap = e as i64 - last;
        eprintln!("  bit {e:5} sym {sym:4}.{bit} (gap {gap})");
        last = e as i64;
    }
    // Check-octet comparison: the transmitted tail past the data bits.
    let data_bits = truth.len().div_ceil(8) * 8; // octet-aligned
    let to_octets = |bits: &[u8]| -> Vec<u8> {
        bits.chunks(8)
            .map(|c| c.iter().enumerate().fold(0u8, |b, (i, &v)| b | (v << (7 - i))))
            .collect()
    };
    let our_checks = to_octets(&tx[data_bits..]);
    let rx_checks = to_octets(&rx[data_bits..n]);
    eprintln!("our checks: {our_checks:02x?}");
    eprintln!("rx  checks: {rx_checks:02x?}");
}
