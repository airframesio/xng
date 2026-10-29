//! Per-stage CPU attribution for the 7-channel live VDL2 workload.
//!
//! Regenerates the budget table VDL2 optimisation work is prioritised from:
//! the real 7-channel plan inside one 2.4 MS/s capture, each channel a
//! `Vdl2ChannelDecoder` with its own full-rate DDC, exactly as the runtime
//! builds it. Measure before deciding what to optimise. The `multi` rows run
//! the same plan through one `Vdl2MultiChannelDecoder` per front end.
//!
//! ```bash
//! cargo run --release -p xng-mode-vdl2 --example vdl2_cpu_budget
//! ```
//!
//! Absolute percentages are machine-specific; the **shares** are what transfer.
//! Numbers are "% of one core" to decode the 7-channel 2.4 MS/s stream in real
//! time, i.e. wall time / capture duration. Stages are measured in isolation
//! and will not sum exactly to the end-to-end figure.

use num_complex::Complex;
use std::time::Instant;
use xng_dsp::Ddc;
use xng_mode_vdl2::avlc::{encode_address, AddressType};
use xng_mode_vdl2::demod::Vdl2Demod;
use xng_mode_vdl2::modulate::burst_iq_shaped;
use xng_mode_vdl2::{
    interleave, Vdl2ChannelDecoder, Vdl2MultiChannelDecoder, CHANNEL_PASSBAND_HZ, CHANNEL_RATE_HI,
};

const FS: f64 = 2_400_000.0;
const DUR_S: f64 = 4.0;
const CENTER_HZ: f64 = 136_812_500.0;
const PLAN_HZ: [f64; 7] = [
    136_650_000.0,
    136_725_000.0,
    136_775_000.0,
    136_825_000.0,
    136_875_000.0,
    136_925_000.0,
    136_975_000.0,
];
/// The runtime's capture read size (`READ_CHUNK` in src/runtime.rs).
const READ_CHUNK: usize = 65_536;

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
    (0..n)
        .map(|_| Complex::new(nx() * 0.01, nx() * 0.01))
        .collect()
}

fn aoa_frame(chan: usize) -> Vec<u8> {
    let mut f = Vec::new();
    f.extend(encode_address(
        AddressType::GroundIcao,
        0x10A234,
        false,
        false,
    ));
    f.extend(encode_address(
        AddressType::Aircraft,
        0xA0_0000 + chan as u32,
        false,
        true,
    ));
    f.push(0x03); // UI
    f.push(0xFF); // AOA IPI
    f.extend(xng_acars::block::build(
        '2',
        &format!("N70{chan}XG"),
        None,
        "H1",
        '3',
        Some("M42A"),
        Some(&format!("XG070{chan}")),
        "CPU BUDGET BUSY-CHANNEL PAYLOAD",
        false,
    ));
    f
}

/// Back-to-back RC-shaped bursts on every channel, staggered so they do not
/// all start together. Returns the capture and the number of bursts placed.
fn all_channels_busy(n: usize, offs: &[f64]) -> (Vec<Complex<f32>>, usize) {
    let mut cap = noise(n);
    let mut placed = 0;
    for (c, &off) in offs.iter().enumerate() {
        let burst = burst_iq_shaped(&[aoa_frame(c)], FS, off, 0.3);
        let mut at = c * 4_096;
        while at + burst.len() <= n {
            for (d, s) in cap[at..at + burst.len()].iter_mut().zip(&burst) {
                *d += s;
            }
            placed += 1;
            at += burst.len();
        }
    }
    (cap, placed)
}

fn decoders(offs: &[f64]) -> Vec<Vdl2ChannelDecoder> {
    offs.iter()
        .map(|&off| Vdl2ChannelDecoder::new(FS, off).expect("build VDL2 channel decoder"))
        .collect()
}

/// Feed the capture through every decoder in runtime-sized chunks.
fn run_all(decs: &mut [Vdl2ChannelDecoder], cap: &[Complex<f32>]) -> usize {
    let mut frames = 0;
    for chunk in cap.chunks(READ_CHUNK) {
        for d in decs.iter_mut() {
            frames += d.process(chunk).len();
        }
    }
    frames
}

/// Which multi-channel front end to build.
#[derive(Clone, Copy)]
enum MultiFront {
    Channelized,
    Shared,
}

fn multi(offs: &[f64], front: MultiFront) -> Vdl2MultiChannelDecoder {
    match front {
        MultiFront::Channelized => Vdl2MultiChannelDecoder::new_channelized(FS, offs),
        MultiFront::Shared => Vdl2MultiChannelDecoder::new_shared(FS, offs),
    }
    .expect("build VDL2 multi-channel decoder")
}

/// Feed the capture through a multi-channel decoder in runtime-sized chunks.
fn run_multi(dec: &mut Vdl2MultiChannelDecoder, cap: &[Complex<f32>]) -> usize {
    let mut frames = 0;
    for chunk in cap.chunks(READ_CHUNK) {
        for (_, f) in dec.process(chunk) {
            frames += f.len();
        }
    }
    frames
}

fn main() {
    let n = (FS * DUR_S) as usize;
    let cap = noise(n);
    let offs: Vec<f64> = PLAN_HZ.iter().map(|f| f - CENTER_HZ).collect();
    let nch = offs.len();
    println!("VDL2 CPU budget — {nch} ch, {FS:.0} S/s, {DUR_S} s capture\n");

    // Decoders, DDCs, demods and their output buffers are all built before
    // the timed closures: construction (tap design, buffer growth) is a
    // one-off at startup in the runtime, and timing it would charge setup to
    // whichever stage happened to allocate.
    let mut decs = decoders(&offs);
    let total = best(|| {
        run_all(&mut decs, &cap);
    });
    line("TOTAL end-to-end (idle)", total);

    // At 2.4 MS/s `Vdl2ChannelDecoder::new` picks CHANNEL_RATE_HI: 2.4 MS/s is
    // not a multiple of the 105 kS/s native rate but is of 100 kS/s (see
    // lib.rs), and with a non-zero offset it always builds exactly this DDC.
    let mut ddcs: Vec<Ddc> = offs
        .iter()
        .map(|&off| Ddc::new(FS, CHANNEL_RATE_HI, off, CHANNEL_PASSBAND_HZ).expect("build DDC"))
        .collect();
    let mut ddc_out: Vec<Complex<f32>> = Vec::with_capacity(READ_CHUNK);
    let front = best(|| {
        for chunk in cap.chunks(READ_CHUNK) {
            for d in ddcs.iter_mut() {
                ddc_out.clear();
                d.process(chunk, &mut ddc_out);
            }
        }
    });
    line("  front end: 7 × Ddc", front);
    line("  back end (total − front)", total - front);

    // One channel's DDC output, kept in per-READ_CHUNK pieces so the demod is
    // fed the same block sizes it sees live.
    let mut ddc0 = Ddc::new(FS, CHANNEL_RATE_HI, offs[0], CHANNEL_PASSBAND_HZ).expect("build DDC");
    let chan_blocks: Vec<Vec<Complex<f32>>> = cap
        .chunks(READ_CHUNK)
        .map(|chunk| {
            let mut out = Vec::new();
            ddc0.process(chunk, &mut out);
            out
        })
        .collect();
    let rs = interleave::vdl2_rs();
    let mut demods: Vec<Vdl2Demod> = (0..nch).map(|_| Vdl2Demod::new(CHANNEL_RATE_HI)).collect();
    let demod_ms = best(|| {
        for block in &chan_blocks {
            for d in demods.iter_mut() {
                std::hint::black_box(d.process(block, &rs).len());
            }
        }
    });
    line("    demod only: 7 × Vdl2Demod", demod_ms);

    let (busy_cap, placed) = all_channels_busy(n, &offs);
    let mut busy_decs = decoders(&offs);
    let busy = best(|| {
        run_all(&mut busy_decs, &busy_cap);
    });
    line("TOTAL end-to-end (busy)", busy);

    let fronts = [
        (MultiFront::Channelized, "channelized"),
        (MultiFront::Shared, "shared"),
    ];
    let mut multi_frames = Vec::new();
    for (front, name) in fronts {
        let mut dec = multi(&offs, front);
        let idle = best(|| {
            run_multi(&mut dec, &cap);
        });
        line(&format!("TOTAL multi ({name}, idle)"), idle);
        let mut dec = multi(&offs, front);
        let busy = best(|| {
            run_multi(&mut dec, &busy_cap);
        });
        line(&format!("TOTAL multi ({name}, busy)"), busy);
        multi_frames.push((name, run_multi(&mut multi(&offs, front), &busy_cap)));
    }

    // A timing alone cannot tell a fast pipeline from a deaf one, so decode
    // the busy capture once with fresh decoders and count.
    let frames = run_all(&mut decoders(&offs), &busy_cap);
    println!("\nframes decoded on the busy capture: {frames} of {placed} bursts placed");
    if frames == 0 {
        println!("*** BROKEN: busy capture decoded 0 frames");
    }
    for (name, n) in multi_frames {
        println!("frames decoded by multi ({name}): {n} of {placed} bursts placed");
        if n == 0 {
            println!("*** BROKEN: multi ({name}) decoded 0 frames");
        }
    }
}
