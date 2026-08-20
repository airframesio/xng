//! Per-stage CPU attribution for the ACARS decode pipeline.
//!
//! Regenerates the budget table the optimisation work is prioritised from.
//! Run this **before** deciding what to optimise — three confident hypotheses
//! about where the time went have already turned out wrong (bin-count pruning,
//! `atan2`, and "the LPF is over-specified"), each caught only by measuring.
//!
//! ```bash
//! cargo run --release -p xng-mode-acars --example cpu_budget
//! ```
//!
//! Absolute percentages are machine-specific; the **shares** are what transfer.
//! Numbers are "% of one core" to decode a 16-channel 2.4 MS/s stream in real
//! time, i.e. wall time / capture duration. Stages are measured in isolation
//! and will not sum exactly to the end-to-end figure (allocation, cache
//! behaviour, and the deframer/message path live in the difference).

use num_complex::Complex;
use std::time::Instant;
use xng_dsp::{lowpass_taps, ChannelizedDdc, Fir, Nco, PfbChannelizer};
use xng_mode_acars::demod::MskDemod;
use xng_mode_acars::{AcarsMultiChannelDecoder, CHANNEL_PASSBAND_HZ, CHANNEL_RATE};

const FS: f64 = 2_400_000.0;
const DUR_S: f64 = 4.0;
/// The real 16-channel US ACARS plan, around a 131.1375 MHz capture centre.
const CENTER_HZ: f64 = 131_137_500.0;
const PLAN_MHZ: [f64; 16] = [
    130.425, 130.45, 130.55, 130.825, 130.85, 131.125, 131.25, 131.425, 131.45, 131.475, 131.525,
    131.55, 131.65, 131.725, 131.825, 131.85,
];
/// Matches `ChannelizedDdc`'s own choice for this plan; see its `choose_num_bins`.
const PFB_BINS: usize = 48;
const PFB_TAPS_PER_BRANCH: usize = 8;

fn pct(ms: f64) -> f64 {
    ms / (DUR_S * 1000.0) * 100.0
}

/// Best-of-N wall time in ms, with one untimed warm-up.
fn best<F: FnMut()>(mut f: F) -> f64 {
    f();
    let mut b = f64::INFINITY;
    for _ in 0..5 {
        let t = Instant::now();
        f();
        let ms = t.elapsed().as_secs_f64() * 1e3;
        if ms < b {
            b = ms;
        }
    }
    b
}

fn line(label: &str, ms: f64) {
    println!("{label:<34}{ms:8.1} ms  {:5.2}%", pct(ms));
}

fn noise(n: usize) -> Vec<Complex<f32>> {
    let mut s = 0x2545_f491_4f6c_dd1du64;
    let mut nx = move || {
        s ^= s << 13;
        s ^= s >> 7;
        s ^= s << 17;
        (s as f32 / u64::MAX as f32) * 2.0 - 1.0
    };
    (0..n).map(|_| Complex::new(nx() * 0.01, nx() * 0.01)).collect()
}

fn main() {
    let n = (FS * DUR_S) as usize;
    let cap = noise(n);
    let offs: Vec<f64> = PLAN_MHZ.iter().map(|f| f * 1e6 - CENTER_HZ).collect();
    let nch = offs.len();
    println!("ACARS CPU budget — {nch} ch, {FS:.0} S/s, {DUR_S} s capture (noise)\n");

    let Ok(mut full) = AcarsMultiChannelDecoder::new(FS, &offs) else {
        eprintln!("could not build the multi-channel decoder for this plan");
        return;
    };
    let total = best(|| {
        full.process(&cap);
    });
    line("TOTAL end-to-end", total);

    let Ok(mut cd) = ChannelizedDdc::new(FS, CHANNEL_RATE, &offs, CHANNEL_PASSBAND_HZ) else {
        eprintln!("could not build the channelizer front end");
        return;
    };
    let mut chans: Vec<Vec<Complex<f32>>> = vec![Vec::new(); nch];
    let front = best(|| {
        cd.process(&cap, &mut chans);
    });
    line("  front end (ChannelizedDdc)", front);

    let mut pfb = PfbChannelizer::new(PFB_BINS, PFB_TAPS_PER_BRANCH);
    let mut bins: Vec<Vec<Complex<f32>>> = vec![Vec::new(); PFB_BINS];
    let pfb_ms = best(|| {
        for b in bins.iter_mut() {
            b.clear();
        }
        pfb.process(&cap, &mut bins);
    });
    line("    polyphase + FFT", pfb_ms);
    line("    per-channel finish", front - pfb_ms);
    line("  back end (demod + deframe)", total - front);

    // Demod internals, at the 24 kHz channel rate, x nch.
    let chan = chans.first().cloned().unwrap_or_default();
    if chan.is_empty() {
        eprintln!("no channel output to profile");
        return;
    }
    let mut demods: Vec<MskDemod> = (0..nch).map(|_| MskDemod::new()).collect();
    let mut bits = Vec::new();
    let demod_ms = best(|| {
        for d in demods.iter_mut() {
            bits.clear();
            d.process(&chan, &mut bits);
        }
    });
    line("    demod x nch", demod_ms);

    // Split the demod line into the cheap always-on part (envelope + EMAs) and
    // the expensive per-sample part (LPF + NCO). A presence gate would have to
    // pay the former and could skip the latter, so this split sizes that
    // opportunity — see the note printed at the end.
    let env_ms = best(|| {
        for _ in 0..nch {
            let (mut level, mut dc, mut nf) = (0.0f32, 0.0f32, 0.0f32);
            for x in &chan {
                let e = x.norm();
                let p = e * e;
                level += 0.005 * (p - level);
                dc += 0.005 * (e - dc);
                if nf == 0.0 {
                    nf = p;
                } else if p < nf * 8.0 {
                    nf += 0.002 * (p - nf);
                } else {
                    nf *= 1.000_02;
                }
            }
            std::hint::black_box((level, dc, nf));
        }
    });
    line("      envelope + EMAs (always on)", env_ms);

    let mut lpfs: Vec<Fir> =
        (0..nch).map(|_| Fir::new(lowpass_taps(1300.0 / CHANNEL_RATE, 121))).collect();
    let lpf_ms = best(|| {
        for f in lpfs.iter_mut() {
            let mut o = Vec::new();
            f.process(&chan, &mut o);
            std::hint::black_box(o.len());
        }
    });
    line("      audio LPF (gateable)", lpf_ms);

    let nco_ms = best(|| {
        for _ in 0..nch {
            let mut m = chan.clone();
            Nco::new(1800.0, CHANNEL_RATE).mix(&mut m);
            std::hint::black_box(m.len());
        }
    });
    line("      NCO mix (gateable)", nco_ms);

    println!(
        "\nACARS channels are idle 87-99% of the time (a 50-char message is\n\
         ~254 ms; 30 msg/min is 12.7% busy), so the two 'gateable' lines are\n\
         almost entirely spent on noise. Nothing in this branch acts on that;\n\
         the split is here to size the opportunity and to make the CPU claims\n\
         in the commit messages reproducible."
    );
}
