//! Long-burst regression (PERF-21): VDL2's N1 (maximum AVLC information
//! field length) defaults to 8312 bits, and a single burst transmission can
//! carry more than one frame, so a real downlink's header-declared
//! transmission length can legitimately run into the thousands of bits. A
//! fixed cap on the header-declared length (evaluated and rejected for
//! PERF-21: lowering it to bound a false lock's worst-case `Collect`
//! duration) would silently drop any real burst above the cap — this test
//! decodes two such bursts (TL close to the N1 default, and roughly half
//! that) at a clean and a noisy in-channel SNR, and must pass against any
//! demod change that keeps every plausible real TL decodable.
use num_complex::Complex;
use xng_mode_vdl2::avlc::{encode_address, AddressType};
use xng_mode_vdl2::demod::SYMBOL_RATE;
use xng_mode_vdl2::modulate::burst_iq_shaped;
use xng_mode_vdl2::Vdl2ChannelDecoder;

const FS: f64 = 100_000.0;
const LEAD: usize = 20_000;
const TAIL: usize = 150_000;
const CHUNK: usize = 2_730;
/// Fixed-seed uniform +/-1 noise (xorshift64), as in `tests/noise_floor.rs`.
struct Noise(u64);
impl Noise {
    fn next(&mut self) -> f32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 as f32 / u64::MAX as f32) * 2.0 - 1.0
    }
}

/// A single-frame UI burst whose AVLC bit stream (flags + stuffing + FCS)
/// comes out at `target_bits`-ish: a payload of filler octets, sized by
/// `info_len`, on top of a 9-octet AVLC header (2 addresses + control +
/// IPI).
fn long_frame(info_len: usize) -> Vec<u8> {
    let mut f = Vec::new();
    f.extend(encode_address(AddressType::GroundIcao, 0x10A234, false, false));
    f.extend(encode_address(AddressType::Aircraft, 0x800F5C, false, true));
    f.push(0x03); // UI
    f.push(0xFF); // AOA IPI (payload content doesn't need to parse as ACARS)
    f.extend((0..info_len).map(|i| (i as u8).wrapping_mul(173).wrapping_add(7)));
    f
}

/// `info_len` chosen so `xng_mode_vdl2::avlc::build`'s bit-stuffed length
/// (the demod's header-declared TL) lands at or just under the target:
/// 1009 octets of payload -> TL 8311 bits (N1's 8312-bit default, the
/// largest a single real AVLC I-frame is specified to carry); 601 octets ->
/// TL 4998 bits (roughly half, a plausible real long downlink/uplink).
const N1_INFO_LEN: usize = 1009;
const N1_TL_BITS: usize = 8311;
const HALF_INFO_LEN: usize = 601;
const HALF_TL_BITS: usize = 4998;

/// Fixed seeds known to decode both sizes at both SNRs (20 dB in-channel
/// SNR is noisy enough that not every seed survives a transmission this
/// long; these were checked to pass before being pinned here — see
/// `examples/vdl2_late.rs`'s module doc for the SNR convention).
const SEEDS: [u64; 3] = [1, 2, 3];

/// Decode `frame_bytes` as a lone burst at `snr_db` in-channel SNR (the
/// `examples/sensitivity_wide.rs` formula) and `seed`; true if the frame
/// came back byte-exact.
fn decodes(frame_bytes: &[u8], snr_db: f64, seed: u64) -> bool {
    let burst = burst_iq_shaped(std::slice::from_ref(&frame_bytes.to_vec()), FS, 0.0, 0.5);
    let sig_pow = burst.iter().map(|c| f64::from(c.norm_sqr())).sum::<f64>() / burst.len() as f64;
    let amp = (1.5 * sig_pow * FS / (SYMBOL_RATE * 10f64.powf(snr_db / 10.0))).sqrt() as f32;
    let mut iq = vec![Complex::new(0.0f32, 0.0); LEAD];
    iq.extend_from_slice(&burst);
    iq.resize(iq.len() + TAIL, Complex::new(0.0, 0.0));
    let mut noise = Noise((seed * 0x9e37_79b9_7f4a_7c15u64) | 1);
    for s in &mut iq {
        *s += Complex::new(noise.next() * amp, noise.next() * amp);
    }
    let mut dec = Vdl2ChannelDecoder::new(FS, 0.0).expect("build VDL2 channel decoder");
    iq.chunks(CHUNK).any(|c| {
        dec.process(c).iter().any(|f| {
            f.avlc.raw.len() == frame_bytes.len() + 2
                && f.avlc.raw[..frame_bytes.len()] == *frame_bytes
        })
    })
}

#[test]
fn n1_sized_burst_decodes_at_clean_and_noisy_snr() {
    let frame = long_frame(N1_INFO_LEN);
    assert_eq!(
        xng_mode_vdl2::avlc::build(&[frame.clone()]).len(),
        N1_TL_BITS,
        "fixture drifted off its target TL; re-bisect info_len"
    );
    for &seed in &SEEDS {
        assert!(
            decodes(&frame, 30.0, seed),
            "N1-sized burst (TL={N1_TL_BITS}) missed at 30 dB, seed {seed}"
        );
    }
    for &seed in &SEEDS {
        assert!(
            decodes(&frame, 20.0, seed),
            "N1-sized burst (TL={N1_TL_BITS}) missed at 20 dB, seed {seed}"
        );
    }
}

#[test]
fn half_n1_sized_burst_decodes_at_clean_and_noisy_snr() {
    let frame = long_frame(HALF_INFO_LEN);
    assert_eq!(
        xng_mode_vdl2::avlc::build(&[frame.clone()]).len(),
        HALF_TL_BITS,
        "fixture drifted off its target TL; re-bisect info_len"
    );
    for &seed in &SEEDS {
        assert!(
            decodes(&frame, 30.0, seed),
            "half-N1 burst (TL={HALF_TL_BITS}) missed at 30 dB, seed {seed}"
        );
    }
    for &seed in &SEEDS {
        assert!(
            decodes(&frame, 20.0, seed),
            "half-N1 burst (TL={HALF_TL_BITS}) missed at 20 dB, seed {seed}"
        );
    }
}
