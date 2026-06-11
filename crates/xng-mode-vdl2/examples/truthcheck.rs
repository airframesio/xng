use xng_mode_vdl2::avlc;
fn main() {
    for p in std::env::args().skip(1) {
        let bits = std::fs::read(&p).unwrap();
        let frames = avlc::scan(&bits);
        println!("{p}: {} AVLC FCS-valid frames", frames.len());
        for f in &frames {
            println!("  {:?} -> {:?} len {}", f.src.addr, f.dst.addr, f.raw.len());
        }
    }
}
