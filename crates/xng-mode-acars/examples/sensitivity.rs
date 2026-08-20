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

/// What the channel looks like before the burst arrives.
///
/// A single burst after a clean noise lead-in is the easy case, and testing
/// only that shape is how a whole class of gate defects reached review: the
/// gate went deaf under a DC pedestal and latched shut on a cold start with
/// signal already on air, and `Clean` could not see either. Both are ordinary
/// on real hardware — `AcarsChannelDecoder::new(rate, 0.0)` puts the channel
/// at the capture centre, which is exactly where LO leakage lands.
#[derive(Clone, Copy, PartialEq)]
enum Channel {
    /// Noise, then the burst.
    Clean,
    /// Noise plus a steady DC offset 15.6 dB above the burst — LO leakage, or
    /// a co-channel carrier. The demod's DC blocker removes it entirely, so
    /// decode must be unaffected; a gate that detects on carrier-inclusive
    /// power instead sees level and floor rise together, goes deaf, and drops
    /// every burst.
    Pedestal,
    /// Carrier already on air when the decoder starts, so the noise floor is
    /// seeded from a signal rather than from silence.
    MidSignalStart,
}

impl Channel {
    fn label(self) -> &'static str {
        match self {
            Self::Clean => "clean",
            Self::Pedestal => "dc pedestal",
            Self::MidSignalStart => "mid-signal start",
        }
    }
}

/// One noisy burst at `sigma`, decoded through the real channel decoder in
/// streaming chunks. `gated` selects the squelch; `false` pins the gate open,
/// which is the pre-squelch pipeline exactly. True when the exact payload came
/// back CRC-clean.
fn decodes(sigma: f32, trial: u64, gated: bool, chan: Channel) -> bool {
    let mut g = Gauss(0xC0FF_EE00_1234_5678u64.wrapping_add(trial.wrapping_mul(0x9E37_79B9)));
    let burst = burst_iq(&spec(), CHANNEL_RATE, 0.0, 0.5);
    let mut iq = vec![Complex::new(0.0f32, 0.0f32); LEAD];
    if chan == Channel::MidSignalStart {
        // Unmodulated carrier already up when the process starts, stopping
        // shortly before the burst keys up.
        for (k, s) in iq.iter_mut().enumerate().take(LEAD - 4_800) {
            let ph = std::f64::consts::TAU * 1_800.0 * k as f64 / CHANNEL_RATE;
            *s += Complex::new(ph.cos() as f32, ph.sin() as f32) * 0.5;
        }
    }
    iq.extend(burst);
    iq.extend(vec![Complex::new(0.0f32, 0.0f32); 400]);
    // A pedestal 15.6 dB above the burst. Sized deliberately: for a detector
    // that squares the raw envelope, level/floor is ((dc + a) / dc)^2, which
    // is 1.36 here — below OPEN_RATIO, so such a detector goes deaf and this
    // arm fails. At 9.5 dB it would be 1.78, still above the threshold, and
    // the arm would pass against the very defect it exists to catch.
    let dc = if chan == Channel::Pedestal { 3.0 } else { 0.0 };
    for s in &mut iq {
        *s += Complex::new(g.z() * sigma + dc, g.z() * sigma);
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
    println!("ACARS sensitivity — {TRIALS} bursts per sigma, real decoder");
    // Flag only a gap wider than this harness produces by chance.
    let margin = (TRIALS / 50).max(2) as usize;
    let mut worst: Vec<String> = Vec::new();

    for chan in [Channel::Clean, Channel::Pedestal, Channel::MidSignalStart] {
        println!("\n-- {} --\n", chan.label());
        println!("{:>7}  {:>12}  {:>12}", "sigma", "ungated", "gated");
        let (mut ungated_row, mut gated_row) = (Vec::new(), Vec::new());
        let mut flagged = false;
        for &sigma in &SIGMAS {
            let ungated = (0..TRIALS).filter(|&t| decodes(sigma, t, false, chan)).count();
            let gated = (0..TRIALS).filter(|&t| decodes(sigma, t, true, chan)).count();
            let bad = gated + margin < ungated;
            flagged |= bad;
            let flag = if bad { "  <-- gate cost" } else { "" };
            println!("{sigma:>7.2}  {ungated:>7}/{TRIALS}  {gated:>7}/{TRIALS}{flag}");
            ungated_row.push(ungated.to_string());
            gated_row.push(gated.to_string());
        }
        println!("  ungated: {}", ungated_row.join(" / "));
        println!("  gated  : {}", gated_row.join(" / "));
        if flagged {
            worst.push(chan.label().to_string());
        }
    }

    println!();
    if worst.is_empty() {
        println!("No scenario shows a gate cost beyond harness scatter.");
    } else {
        println!("GATE COST in: {}", worst.join(", "));
    }
    println!(
        "\nCompare the two columns within a block against each other, NOT\n\
         against numbers from a previous run — absolute yield moves with the\n\
         seed and with the host. A consistent one-directional gap across the\n\
         middle three sigmas is the squelch eating marginal frames; scatter in\n\
         both directions is not."
    );
}
