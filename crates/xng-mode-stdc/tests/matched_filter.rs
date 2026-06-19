//! External-oracle BER test for the RRC matched filter.
//!
//! This is a genuine modulate → AWGN → demod noise test (NOT a noiseless
//! loopback): a frame is modulated with this crate's `modulate` (TX RRC
//! half), complex Gaussian noise is added at a controlled SNR, and the
//! frame-recovery rate is measured with the matched filter ON vs OFF over
//! many seeds. The matched filter is the receive RRC half; the two halves
//! together form a raised-cosine Nyquist pulse, the textbook matched-filter
//! arrangement that maximises symbol SNR. The pass criterion is that the
//! matched-filter path recovers at least as many frames as the bare-lowpass
//! path at every SNR tested, and strictly more at the marginal SNR.

use num_complex::Complex;
use xng_mode_stdc::frame::encode_frame;
use xng_mode_stdc::modulate::modulate;
use xng_mode_stdc::packet::build_packet;
use xng_mode_stdc::StdcChannelDecoder;

/// Box-Muller complex AWGN with a deterministic xorshift core, so SNR is
/// well defined (each I/Q component ~ N(0, sigma^2)).
struct Awgn {
    state: u64,
    spare: Option<f32>,
}
impl Awgn {
    fn new(seed: u64) -> Self {
        Self { state: seed | 1, spare: None }
    }
    fn u01(&mut self) -> f32 {
        self.state ^= self.state << 13;
        self.state ^= self.state >> 7;
        self.state ^= self.state << 17;
        // (0,1]
        ((self.state >> 11) as f32 + 1.0) / ((1u64 << 53) as f32)
    }
    fn gauss(&mut self) -> f32 {
        if let Some(s) = self.spare.take() {
            return s;
        }
        let u1 = self.u01();
        let u2 = self.u01();
        let mag = (-2.0 * u1.ln()).sqrt();
        let z0 = mag * (std::f32::consts::TAU * u2).cos();
        let z1 = mag * (std::f32::consts::TAU * u2).sin();
        self.spare = Some(z1);
        z0
    }
}

fn frame_payload(text: &[u8]) -> Vec<u8> {
    let mut payload = Vec::new();
    payload.extend(build_packet(&[0x7D, 1, 0x03, 0xE8, 0, 0, 1, 0x10, 0, 0, 0, 0]));
    // 0xB0 EGC, service 0x31 (NAVAREA), safety priority.
    let mut body = vec![0xB0, 0u8, 0x31, (1 << 5) | 1];
    body.extend(881u16.to_be_bytes());
    body.push(1);
    body.push(0);
    body.extend([0x12, 0x34, 0x56, 0x78]);
    body.extend(text);
    body[1] = body.len() as u8;
    payload.extend(build_packet(&body));
    payload.resize(639, 0);
    payload
}

/// Run one trial: modulate `reps` copies of the frame (after a settling
/// preamble), add AWGN at `amp`/`noise_sigma`, decode, and return how many
/// of the repeated frames were recovered intact (correct EGC text). This
/// per-frame count (rather than a binary "any frame") exposes graceful
/// degradation near the noise cliff, where the matched-filter advantage is.
fn recovered_frames(
    text: &[u8],
    matched: bool,
    amp: f32,
    noise_sigma: f32,
    reps: usize,
    seed: u64,
) -> u32 {
    let symbols = encode_frame(&frame_payload(text));
    let mut all: Vec<u8> = (0..4000).map(|i| (i % 2) as u8).collect();
    for _ in 0..reps {
        all.extend(&symbols);
    }
    let mut iq = modulate(&all, 1200.0, 48_000.0, 230.0, amp);
    let mut n = Awgn::new(seed);
    for s in &mut iq {
        *s += Complex::new(n.gauss() * noise_sigma, n.gauss() * noise_sigma);
    }
    let mut dec = StdcChannelDecoder::with_matched_filter(48_000.0, 0.0, matched).unwrap();
    let want = std::str::from_utf8(text).unwrap();
    let mut count = 0u32;
    for chunk in iq.chunks(8192) {
        for e in dec.process(chunk) {
            if e.name == "egc-message" && e.text.as_deref() == Some(want) {
                count += 1;
            }
        }
    }
    count
}

/// Total frames recovered over `trials` seeds at a given SNR setting.
fn recovery_count(matched: bool, amp: f32, noise_sigma: f32, reps: usize, trials: u64) -> u32 {
    const TEXT: &[u8] = b"SECURITE NAVAREA XII TEST BUOY ADRIFT";
    (0..trials)
        .map(|t| {
            recovered_frames(
                TEXT,
                matched,
                amp,
                noise_sigma,
                reps,
                0x9e37_79b9_7f4a_0000 ^ (t * 2654435761),
            )
        })
        .sum()
}

#[test]
fn probe_sweep() {
    if std::env::var("STDC_PROBE").is_err() {
        return;
    }
    let trials: u64 = std::env::var("STDC_TRIALS").ok().and_then(|v| v.parse().ok()).unwrap_or(40);
    let amp = 0.5f32;
    let points: Vec<f32> = std::env::var("STDC_SIGMAS")
        .ok()
        .map(|v| v.split(',').filter_map(|s| s.trim().parse().ok()).collect())
        .unwrap_or_else(|| vec![4.0f32, 4.5, 5.0, 5.5, 6.0]);
    let reps: usize = std::env::var("STDC_REPS").ok().and_then(|v| v.parse().ok()).unwrap_or(8);
    let max = trials * reps as u64;
    for &sigma in &points {
        let on = recovery_count(true, amp, sigma, reps, trials);
        let off = recovery_count(false, amp, sigma, reps, trials);
        eprintln!(
            "sigma={sigma:.2}  on={on}/{max} ({:.1}%)  off={off}/{max} ({:.1}%)  delta={}",
            100.0 * on as f64 / max as f64,
            100.0 * off as f64 / max as f64,
            on as i64 - off as i64
        );
    }
}

#[test]
fn matched_filter_recovers_at_lower_snr() {
    // Genuine modulate -> AWGN -> demod noise test (the external oracle for
    // this crate). At a fixed signal amplitude we sweep the noise sigma into
    // the marginal-SNR cliff and count how many of the repeated frames decode
    // intact with the RRC matched filter ON vs OFF (bare anti-alias lowpass).
    //
    // Empirically measured gain curve (see the `probe_sweep` companion for
    // the full 30-trial x 8-frame sweep): the matched filter sharply extends
    // the usable SNR floor, e.g. at sigma=9.0 it recovers ~70-75% of frames
    // vs ~10-20% for the bare lowpass.
    //
    //   sigma   matched ON   matched OFF
    //   8.0     ~80%         ~62%
    //   8.5     ~77%         ~45%
    //   9.0     ~70%         ~10%
    //
    // The conservative pass criteria below leave ample margin around the
    // measured deltas so the test is stable across noise seeds.
    let trials = 10u64;
    let amp = 0.5f32;
    let reps = 6usize;
    let max_per_point = trials as u32 * reps as u32;

    // Marginal-SNR points where the matched filter advantage is large.
    let points = [8.0f32, 8.5, 9.0];

    let mut total_on = 0u32;
    let mut total_off = 0u32;
    for &sigma in &points {
        let on = recovery_count(true, amp, sigma, reps, trials);
        let off = recovery_count(false, amp, sigma, reps, trials);
        eprintln!(
            "sigma={sigma:.2}  matched_on={on}/{max_per_point}  matched_off={off}/{max_per_point}  delta={}",
            on as i64 - off as i64
        );
        // Matched filter must never recover fewer frames than the bare path.
        assert!(
            on >= off,
            "matched filter regressed at sigma={sigma}: on={on} off={off}"
        );
        total_on += on;
        total_off += off;
    }

    eprintln!("TOTAL matched_on={total_on} matched_off={total_off}");
    // Net, the matched filter must recover materially more frames across the
    // marginal-SNR sweep. Measured net delta ~+66/180 (on=136 off=70 at
    // 10 trials x 6 frames x 3 sigmas); require a robust fraction (~+33) so
    // seed jitter cannot make the test flap while still proving a large gain.
    let min_net_gain = (max_per_point * 9) / 16; // ~+33 of the measured +66
    assert!(
        total_on >= total_off + min_net_gain,
        "matched filter net gain too small (on={total_on} off={total_off}, \
         need >= +{min_net_gain})"
    );
}
