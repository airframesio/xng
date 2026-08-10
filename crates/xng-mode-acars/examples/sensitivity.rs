//! Sensitivity sweep: a real ACARS burst at swept AWGN, decoded through the
//! **real** `AcarsChannelDecoder` (not a reimplemented chain), reporting
//! CRC-OK yield per noise sigma.
//!
//! This is the mandatory gate for any change that touches the demod. High-SNR
//! loopback tests do not catch demod regressions, and this is not hypothetical:
//! the unnormalised cross-product discriminator (i.e. `demod.rs` with the
//! `/ mag` deleted) passes **every test in the repository** while losing more
//! than half the marginal frames. Measured with this harness, 400 trials,
//! identical noise draws:
//!
//! | sigma | 0.18 | 0.20 | 0.22 |
//! |---|---|---|---|
//! | raw cross product | 315 | 153 | 44 |
//! | normalised (shipped) | 372 | 271 | 159 |
//!
//! ```bash
//! cargo run --release -p xng-mode-acars --example sensitivity
//! ```
//!
//! ## Read the columns, not the absolute numbers
//!
//! **A single row means very little on its own.** Absolute yield moves with the
//! noise seed: across five seeds with nothing else changed, the sigma = 0.18
//! column ranged 52-56 of 60 and sigma = 0.22 ranged 20-29. Any comparison has
//! to be paired — same seeds, same capture, one variable — and quoting one
//! run's row as a target for a later run invites chasing pure scatter.
//!
//! That is why `TRIALS` is 400 rather than 60. At 60 the run-to-run spread is
//! the same size as the effects this exists to detect, which made the tap table
//! that previously lived in `demod.rs` unsupportable as printed even though its
//! conclusion was right.
//!
//! Only sigma 0.18/0.20/0.22 discriminate: 0.15 is saturated and 0.25 is down
//! in the floor, so those two move for reasons unrelated to the change.
//!
//! ## The squelch control
//!
//! Both columns use the same capture, the same noise draws and the same
//! lead-in, and differ in exactly one bit: whether the squelch is active or
//! pinned open. Pinned open *is* the pre-squelch pipeline, so a consistent gap
//! between the columns is the gate's doing and nothing else's.
//!
//! The lead-in length is load-bearing for that control. At the 400 samples
//! this harness originally used, the squelch is still seeding its noise floor
//! (and inside the hangover that follows) when the burst arrives, so the gate
//! is already open and the comparison is vacuous: a deliberately deaf gate,
//! open threshold raised to 12x the floor, scored an unchanged full-marks row.
//! `LEAD` must stay well above the gate's seed plus hangover.

use num_complex::Complex;
use xng_mode_acars::modulate::{burst_iq, FrameSpec};
use xng_mode_acars::{AcarsChannelDecoder, CHANNEL_RATE};

const TEXT: &str = "SENSITIVITY SWEEP PAYLOAD";
/// 400, not 60 — see the scatter note in the module docs. ~7 s.
const TRIALS: u64 = 400;
const SIGMAS: [f32; 5] = [0.15, 0.18, 0.20, 0.22, 0.25];
/// Two seconds of silence ahead of the burst so the DC and noise EMAs are
/// fully converged when it arrives, which is the state a live receiver is in
/// essentially always. A short lead-in measures the settling transient as much
/// as it measures the demod.
const LEAD: usize = 48_000;

/// Box-Muller over xorshift — deterministic, no rand dependency, so a run is
/// reproducible and two builds see bit-identical noise.
struct Gauss(u64);

impl Gauss {
    fn u(&mut self) -> f64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        ((self.0 >> 11) as f64 + 1.0) / ((1u64 << 53) as f64 + 2.0)
    }
    fn z(&mut self) -> f32 {
        let (u1, u2) = (self.u(), self.u());
        ((-2.0 * u1.ln()).sqrt() * (std::f64::consts::TAU * u2).cos()) as f32
    }
}

fn spec() -> FrameSpec<'static> {
    FrameSpec {
        mode: '2',
        tail: "N471XG",
        ack: None,
        label: "H1",
        block_id: '3',
        msg_num: Some("M42A"),
        flight: Some("XG0042"),
        text: TEXT,
        etb: false,
    }
}

/// One noisy burst at `sigma`, decoded through the real channel decoder in
/// streaming chunks. `gated` selects the squelch; `false` pins the gate open,
/// which is the pre-squelch pipeline exactly. True when the exact payload came
/// back CRC-clean.
fn decodes(sigma: f32, trial: u64, gated: bool) -> bool {
    let mut g = Gauss(0xC0FF_EE00_1234_5678u64.wrapping_add(trial.wrapping_mul(0x9E37_79B9)));
    let burst = burst_iq(&spec(), CHANNEL_RATE, 0.0, 0.5);
    let mut iq = vec![Complex::new(0.0f32, 0.0f32); LEAD];
    iq.extend(burst);
    iq.extend(vec![Complex::new(0.0f32, 0.0f32); 400]);
    for s in &mut iq {
        *s += Complex::new(g.z() * sigma, g.z() * sigma);
    }

    let Ok(mut dec) = AcarsChannelDecoder::new(CHANNEL_RATE, 0.0) else {
        return false;
    };
    dec.hold_squelch_open(!gated);
    let mut frames = Vec::new();
    for chunk in iq.chunks(1024) {
        frames.extend(dec.process(chunk));
    }
    frames.iter().any(|f| f.crc_ok && f.text == TEXT)
}

fn main() {
    println!("ACARS sensitivity — {TRIALS} bursts per sigma, real decoder\n");
    println!("{:>7}  {:>12}  {:>12}", "sigma", "ungated", "gated");
    let (mut ungated_row, mut gated_row) = (Vec::new(), Vec::new());
    for &sigma in &SIGMAS {
        let ungated = (0..TRIALS).filter(|&t| decodes(sigma, t, false)).count();
        let gated = (0..TRIALS).filter(|&t| decodes(sigma, t, true)).count();
        // Flag only a gap wider than this harness produces by chance.
        let margin = (TRIALS / 50).max(2) as usize;
        let flag = if gated + margin < ungated { "  <-- gate cost" } else { "" };
        println!("{sigma:>7.2}  {ungated:>7}/{TRIALS}  {gated:>7}/{TRIALS}{flag}");
        ungated_row.push(ungated.to_string());
        gated_row.push(gated.to_string());
    }
    println!("\nungated (gate pinned open): {}", ungated_row.join(" / "));
    println!("gated   (squelch active)  : {}", gated_row.join(" / "));
    println!(
        "\nCompare the two columns against each other, NOT against numbers from\n\
         a previous run — absolute yield moves with the seed and with the host.\n\
         A consistent one-directional gap across the middle three sigmas is the\n\
         squelch eating marginal frames; scatter in both directions is not."
    );
}
