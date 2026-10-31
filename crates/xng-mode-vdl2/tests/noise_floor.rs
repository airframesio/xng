//! VDL2 hunt noise floor: a decoder that starts cold on noise must not hold
//! back the first burst, and one that starts on (or sees a long stretch of)
//! continuous signal must not be left deaf once idle gaps appear.
use num_complex::Complex;
use xng_mode_vdl2::avlc::{encode_address, AddressType};
use xng_mode_vdl2::Vdl2ChannelDecoder;

struct Noise(u64);
impl Noise {
    fn next(&mut self) -> f32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 as f32 / u64::MAX as f32) * 2.0 - 1.0
    }
}

fn aoa_frame() -> Vec<u8> {
    let mut f = Vec::new();
    f.extend(encode_address(AddressType::GroundIcao, 0x10A234, false, false));
    f.extend(encode_address(AddressType::Aircraft, 0x800F5C, false, true));
    f.push(0x03);
    f.push(0xFF);
    f.extend(xng_acars::block::build(
        '2',
        "VT-ANB",
        None,
        "B6",
        '4',
        Some("M11A"),
        Some("AI0142"),
        "/BOMASAI.ADS.VT-ANB072501A070A988CA73248F0E5DC10200000F5EE1ABC000102B885E0A19F5",
        false,
    ));
    f
}

/// A decoder that starts cold on noise must emit a strong burst as soon as
/// the burst ends. The hunt's noise floor used to start at 1e-6, so for the
/// first seconds nearly every noise sample passed the energy gate; the
/// resulting false UW locks (a few passing the header FEC) sent collection
/// off for ~0.4 s of garbage before the RS failure rewound to the real
/// burst. Before the floor was seeded from the input, 12/40 seeds here were
/// held back 118–446 ms past the burst end (most by ~200 ms).
#[test]
fn burst_after_cold_start_not_held_back() {
    use xng_mode_vdl2::demod::SYMBOL_RATE;
    use xng_mode_vdl2::modulate::burst_iq_shaped;
    const FS: f64 = 100_000.0;
    const SNR_DB: f64 = 30.0;
    const SEEDS: u64 = 40;
    const LEAD: usize = 20_000;
    const TAIL: usize = 100_000;
    const CHUNK: usize = 2_730;
    const MAX_LATE: usize = 3_000;

    let frame = aoa_frame();
    let burst = burst_iq_shaped(std::slice::from_ref(&frame), FS, 0.0, 0.5);
    // In-channel SNR as in examples/sensitivity_wide.rs.
    let sig_pow = burst.iter().map(|c| f64::from(c.norm_sqr())).sum::<f64>() / burst.len() as f64;
    let amp = (1.5 * sig_pow * FS / (SYMBOL_RATE * 10f64.powf(SNR_DB / 10.0))).sqrt() as f32;
    let burst_end = LEAD + burst.len();

    let mut late = Vec::new();
    for t in 0..SEEDS {
        let mut iq = vec![Complex::new(0.0f32, 0.0); LEAD];
        iq.extend_from_slice(&burst);
        iq.resize(iq.len() + TAIL, Complex::new(0.0, 0.0));
        let mut noise = Noise(0x9e37_79b9_7f4a_7c15u64.wrapping_mul(t + 1) | 1);
        for s in &mut iq {
            *s += Complex::new(noise.next() * amp, noise.next() * amp);
        }
        let mut dec = Vdl2ChannelDecoder::new(FS, 0.0).unwrap();
        let mut fed = 0usize;
        let mut emitted = None;
        for c in iq.chunks(CHUNK) {
            fed += c.len();
            let hit = dec.process(c).iter().any(|f| {
                f.avlc.raw.len() == frame.len() + 2 && f.avlc.raw[..frame.len()] == frame[..]
            });
            if hit {
                emitted = Some(fed);
                break;
            }
        }
        late.push((t, emitted.map(|e| e as i64 - burst_end as i64)));
    }
    let bad: Vec<_> = late.iter().filter(|(_, l)| l.is_none_or(|l| l > MAX_LATE as i64)).collect();
    assert!(
        bad.is_empty(),
        "{}/{SEEDS} seeds missed or emitted > {MAX_LATE} samples after the burst end \
         (seed, samples late): {bad:?}",
        bad.len()
    );
}

/// Decode a 100 kS/s stream of short, distinct UI-frame bursts at 30 dB
/// in-channel SNR: `continuous` samples of back-to-back bursts from sample
/// 0, then `post` bursts each followed by `gap` samples of idle noise.
/// Returns (indices of post-phase bursts that did not decode, sample offset
/// after the continuous phase at which the first post-phase burst was
/// emitted).
fn occupancy_trial(continuous: usize, post: usize, gap: usize) -> (Vec<usize>, Option<usize>) {
    use xng_mode_vdl2::demod::SYMBOL_RATE;
    use xng_mode_vdl2::modulate::burst_iq_shaped;
    const FS: f64 = 100_000.0;
    const SNR_DB: f64 = 30.0;
    const CHUNK: usize = 2_730;
    let frame = |i: u32| -> Vec<u8> {
        let mut f = Vec::new();
        f.extend(encode_address(AddressType::GroundIcao, 0x10A234, false, false));
        f.extend(encode_address(AddressType::Aircraft, 0x80_0000 + i, false, true));
        f.push(0x03); // UI
        f.extend((0..32u8).map(|b| b.wrapping_mul(37).wrapping_add(i as u8)));
        f
    };
    let mut frames: Vec<Vec<u8>> = Vec::new();
    let mut iq = Vec::new();
    let add = |frames: &mut Vec<Vec<u8>>, iq: &mut Vec<Complex<f32>>| {
        frames.push(frame(frames.len() as u32));
        iq.extend(burst_iq_shaped(std::slice::from_ref(frames.last().unwrap()), FS, 0.0, 0.5));
    };
    while iq.len() < continuous {
        add(&mut frames, &mut iq);
    }
    let pre = frames.len();
    let post_start = iq.len();
    for _ in 0..post {
        iq.resize(iq.len() + gap, Complex::new(0.0, 0.0));
        add(&mut frames, &mut iq);
    }
    iq.resize(iq.len() + 30_000, Complex::new(0.0, 0.0));
    let reference = burst_iq_shaped(&frames[..1], FS, 0.0, 0.5);
    let sig_pow =
        reference.iter().map(|c| f64::from(c.norm_sqr())).sum::<f64>() / reference.len() as f64;
    let amp = (1.5 * sig_pow * FS / (SYMBOL_RATE * 10f64.powf(SNR_DB / 10.0))).sqrt() as f32;
    let mut noise = Noise(0x0dd1_5eed_c0ff_ee01);
    for s in &mut iq {
        *s += Complex::new(noise.next() * amp, noise.next() * amp);
    }

    let mut dec = Vdl2ChannelDecoder::new(FS, 0.0).unwrap();
    let mut decoded = vec![false; frames.len()];
    let mut first_post = None;
    let mut fed = 0usize;
    for c in iq.chunks(CHUNK) {
        fed += c.len();
        for f in dec.process(c) {
            if let Some(i) = frames.iter().position(|t| f.avlc.raw.starts_with(t)) {
                decoded[i] = true;
                if i >= pre && first_post.is_none() {
                    first_post = Some(fed - post_start);
                }
            }
        }
    }
    let missed = (pre..frames.len()).filter(|&i| !decoded[i]).collect();
    (missed, first_post)
}

/// A capture that starts mid-transmission (0.1 s of back-to-back bursts
/// from sample 0) followed by ≈45% occupancy (20 ms idle gaps): the
/// channel must not stay deaf — every burst after the first gap decodes.
/// With a floor seeded from the first 41 ms, the below-gate EMA settled at
/// occupancy × burst power and every one of these bursts was lost.
#[test]
fn floor_seeded_on_signal_recovers_at_first_gap() {
    let (missed, first) = occupancy_trial(10_000, 8, 2_000);
    assert!(missed.is_empty(), "post-gap bursts not decoded: {missed:?} (first at {first:?})");
}

/// Long continuous signal (3 s of back-to-back bursts, no idle at all) and
/// then ≈45% occupancy: the floor must not stay locked onto signal. The
/// gated-EMA estimator's up-creep reached signal level within the
/// continuous phase and then held there.
#[test]
fn floor_survives_long_continuous_signal() {
    let (missed, first) = occupancy_trial(300_000, 8, 2_000);
    assert!(missed.is_empty(), "post-gap bursts not decoded: {missed:?} (first at {first:?})");
}
