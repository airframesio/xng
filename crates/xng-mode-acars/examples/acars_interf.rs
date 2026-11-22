//! ACARS multi-channel front-end robustness harness (PERF-20): how well each
//! [`AcarsMultiChannelDecoder`] front end keeps a weak channel decodable next
//! to a strong neighbour, and how that changes when the capture center is off
//! the 25 kHz / 8.33 kHz raster (so channels land off their bin centers).
//!
//! Every front end is selected explicitly through the public constructors —
//! no environment toggles:
//!
//! | column     | constructor                                                  |
//! |------------|--------------------------------------------------------------|
//! | `default`  | `AcarsMultiChannelDecoder::new` (what the daemon builds)     |
//! | `critical` | `new_channelized` (critically sampled channelizer)           |
//! | `x2`       | `new_channelized_with_oversample(.., Oversample::Twice)`     |
//! | `shared`   | `new_shared` (shared decimation)                             |
//!
//! Plans: the 8-channel plan at centers 131.500 / 131.505 / 131.5125 MHz and
//! the US 16-channel plan at 131.1375 / 131.1425 MHz, all at 2.4 MS/s.
//!
//! ```bash
//! cargo run --release -p xng-mode-acars --example acars_interf -- <mode> [trials] [snr_db]
//! ```
//!
//! Modes:
//!
//! * `describe` — the front end and bank geometry each constructor builds per
//!   plan (or why it refused).
//! * `leak` — worst-case in-band (|f| < 5 kHz) leakage of a unit tone at
//!   weak ± 16.67 / 20 / 25 kHz into the weak channel, dB relative to the same
//!   tone on-channel, over every channel of the plan (channel index in
//!   parentheses). Front end only, no demod.
//! * `sweep` — no interferer, weak channel 131.525 MHz, SNR 6–14 dB.
//! * `interf` — weak channel at `snr_db` against one interferer at a time:
//!   back-to-back ACARS bursts at ±25 kHz (+20/+30/+40 dB), band-limited
//!   multitone AM "voice" (8 tones 300–3000 Hz, 4 Hz syllabic envelope,
//!   m = 0.8) at ±16.67 / ±20 kHz (+30/+40/+50 dB), and an AM 1 kHz tone
//!   (m = 0.8) at ±20 kHz (+40 dB).
//! * `base` — no interferer, weak 131.525 MHz, 8 noise seeds, totals per plan.
//! * `cpu` — idle-noise CPU (% of one core), end-to-end decoder / front end
//!   only, minimum over `trials` repetitions.
//!
//! `trials` is the number of weak bursts per case (default 100) or, for
//! `cpu`, the repetition count (default 7). `snr_db` defaults to 9.
//!
//! SNR definition: weak AM carrier power over the complex AWGN power in a
//! 10 kHz (±5 kHz) channel bandwidth, i.e. `A² / (2σ² · 10 kHz / fs)`.
//! Interferer levels are carrier power relative to the weak carrier.
//! Decode counts are CRC-OK weak bursts with the expected payload text.

use num_complex::Complex;
use std::f64::consts::TAU;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::Instant;
use xng_dsp::{lowpass_taps, ChannelizedDdc, Fir, Oversample, SharedDdc};
use xng_mode_acars::modulate::{burst_iq, FrameSpec};
use xng_mode_acars::{AcarsMultiChannelDecoder, CHANNEL_PASSBAND_HZ, CHANNEL_RATE};

const FS: f64 = 2_400_000.0;
const CHUNK: usize = 65_536;
const SIGMA: f32 = 0.01;
const WEAK_MHZ: f64 = 131.525;
const PLAN8: [f64; 8] = [131.425, 131.450, 131.475, 131.525, 131.550, 131.575, 131.725, 131.825];
const PLAN16: [f64; 16] = [
    130.425, 130.45, 130.55, 130.825, 130.85, 131.125, 131.25, 131.425, 131.45, 131.475, 131.525,
    131.55, 131.65, 131.725, 131.825, 131.85,
];

#[derive(Clone, Copy, Debug, PartialEq)]
enum Fe {
    Default,
    Critical,
    Twice,
    Shared,
}

const FES: [Fe; 4] = [Fe::Default, Fe::Critical, Fe::Twice, Fe::Shared];
const N_FE: usize = FES.len();

impl Fe {
    fn label(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::Critical => "critical",
            Self::Twice => "x2",
            Self::Shared => "shared",
        }
    }
}

fn build(fe: Fe, offs: &[f64]) -> Result<AcarsMultiChannelDecoder, String> {
    match fe {
        Fe::Default => AcarsMultiChannelDecoder::new(FS, offs),
        Fe::Critical => AcarsMultiChannelDecoder::new_channelized(FS, offs),
        Fe::Twice => {
            AcarsMultiChannelDecoder::new_channelized_with_oversample(FS, offs, Oversample::Twice)
        }
        Fe::Shared => AcarsMultiChannelDecoder::new_shared(FS, offs),
    }
}

/// The bare front end a constructor builds, for the front-end-only modes.
enum FrontOnly {
    Channelized(ChannelizedDdc),
    Shared(SharedDdc),
}

impl FrontOnly {
    fn new(fe: Fe, offs: &[f64]) -> Result<Self, String> {
        let os = match fe {
            // Whatever `new` actually picked, read back from the decoder so
            // this tracks the library default instead of restating it.
            Fe::Default => {
                let d = build(fe, offs)?.describe_front();
                if d.starts_with("shared") {
                    return Self::new(Fe::Shared, offs);
                }
                if d.contains(" Twice ") {
                    Oversample::Twice
                } else {
                    Oversample::Critical
                }
            }
            Fe::Critical => Oversample::Critical,
            Fe::Twice => Oversample::Twice,
            Fe::Shared => {
                return Ok(Self::Shared(SharedDdc::new(
                    FS,
                    CHANNEL_RATE,
                    offs,
                    CHANNEL_PASSBAND_HZ,
                )?))
            }
        };
        Ok(Self::Channelized(ChannelizedDdc::with_oversample(
            FS,
            CHANNEL_RATE,
            offs,
            CHANNEL_PASSBAND_HZ,
            os,
        )?))
    }

    fn process(&mut self, input: &[Complex<f32>], out: &mut [Vec<Complex<f32>>]) {
        match self {
            Self::Channelized(c) => c.process(input, out),
            Self::Shared(s) => s.process(input, out),
        }
    }
}

struct Plan {
    name: String,
    center_hz: f64,
    mhz: Vec<f64>,
}

impl Plan {
    fn offsets(&self) -> Vec<f64> {
        self.mhz.iter().map(|f| (f * 1e6 - self.center_hz).round()).collect()
    }

    fn index_of(&self, mhz: f64) -> usize {
        self.mhz.iter().position(|&x| (x - mhz).abs() < 1e-6).expect("channel not in plan")
    }
}

fn plans() -> Vec<Plan> {
    let mut v = Vec::new();
    for c in [131.500, 131.505, 131.5125] {
        v.push(Plan { name: format!("8ch@{c:.4}"), center_hz: c * 1e6, mhz: PLAN8.to_vec() });
    }
    for c in [131.1375, 131.1425] {
        v.push(Plan { name: format!("16ch@{c:.4}"), center_hz: c * 1e6, mhz: PLAN16.to_vec() });
    }
    v
}

/// xorshift64 + Box-Muller: deterministic, dependency-free noise.
struct Rng(u64);

impl Rng {
    fn uniform(&mut self) -> f64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        ((self.0 >> 11) as f64 + 1.0) / ((1u64 << 53) as f64 + 2.0)
    }

    fn gauss_pair(&mut self) -> (f32, f32) {
        let (u1, u2) = (self.uniform(), self.uniform());
        let r = (-2.0 * u1.ln()).sqrt();
        ((r * (TAU * u2).cos()) as f32, (r * (TAU * u2).sin()) as f32)
    }
}

/// Run `f(0..n)` across the machine's cores, results in index order.
fn par<T: Send, F: Fn(usize) -> T + Sync>(n: usize, f: F) -> Vec<T> {
    let threads = std::thread::available_parallelism().map_or(4, |x| x.get());
    let next = AtomicUsize::new(0);
    let out: Vec<Mutex<Option<T>>> = (0..n).map(|_| Mutex::new(None)).collect();
    std::thread::scope(|s| {
        for _ in 0..threads {
            s.spawn(|| loop {
                let i = next.fetch_add(1, Ordering::Relaxed);
                if i >= n {
                    break;
                }
                let v = f(i);
                *out[i].lock().expect("poisoned") = Some(v);
            });
        }
    });
    out.into_iter().map(|m| m.into_inner().expect("poisoned").expect("job not run")).collect()
}

fn fe_header() -> String {
    FES.iter().map(|fe| format!("{:>9}", fe.label())).collect()
}

fn fmt_counts(r: &[Option<usize>; N_FE]) -> String {
    r.iter()
        .map(|x| match x {
            Some(v) => format!("{v:>9}"),
            None => format!("{:>9}", "refused"),
        })
        .collect()
}

// ---------------------------------------------------------------- describe

fn describe() {
    println!("{:<13} {:<9} front end", "plan", "ctor");
    for p in plans() {
        let offs = p.offsets();
        for fe in FES {
            let d = match build(fe, &offs) {
                Ok(dec) => dec.describe_front(),
                Err(e) => format!("REFUSED: {e}"),
            };
            println!("{:<13} {:<9} {d}", p.name, fe.label());
        }
    }
}

// ---------------------------------------------------------------- leak

/// In-band (|f| < 5 kHz) output power of channel `ch` for a unit tone at
/// `freq` (Hz relative to the capture center).
fn inband_power(fe: Fe, offs: &[f64], ch: usize, freq: f64) -> f64 {
    let n = (FS * 0.12) as usize;
    let tone: Vec<Complex<f32>> = (0..n)
        .map(|k| {
            let ph = TAU * freq * k as f64 / FS;
            Complex::new(ph.cos() as f32, ph.sin() as f32)
        })
        .collect();
    let Ok(mut front) = FrontOnly::new(fe, offs) else {
        return f64::NAN;
    };
    let mut out = vec![Vec::new(); offs.len()];
    let mut acc: Vec<Complex<f32>> = Vec::new();
    for c in tone.chunks(CHUNK) {
        front.process(c, &mut out);
        acc.extend_from_slice(&out[ch]);
    }
    let mut lp = Fir::new(lowpass_taps(CHANNEL_PASSBAND_HZ / CHANNEL_RATE, 255));
    let mut y = Vec::new();
    lp.process(&acc, &mut y);
    let skip = y.len() / 3;
    let p: f64 = y[skip..].iter().map(|s| f64::from(s.norm_sqr())).sum();
    p / (y.len() - skip) as f64
}

fn leak() {
    const DELTAS: [f64; 6] = [-25_000.0, -20_000.0, -16_667.0, 16_667.0, 20_000.0, 25_000.0];
    println!(
        "Worst in-band leak of a tone at ch+delta into ch (|f|<5k), dB rel. on-channel tone (ch)"
    );
    println!(
        "{:<13} {:<9} {:>10} {:>10} {:>10} {:>10} {:>10} {:>10}",
        "plan", "front", "-25k", "-20k", "-16.7k", "+16.7k", "+20k", "+25k"
    );
    let ps = plans();
    let rows: Vec<(usize, Fe)> =
        (0..ps.len()).flat_map(|i| FES.iter().map(move |&fe| (i, fe))).collect();
    let res = par(rows.len(), |r| {
        let (i, fe) = rows[r];
        let offs = ps[i].offsets();
        if FrontOnly::new(fe, &offs).is_err() {
            return None;
        }
        let refs: Vec<f64> =
            (0..offs.len()).map(|ch| inband_power(fe, &offs, ch, offs[ch])).collect();
        let mut row = String::new();
        for d in DELTAS {
            let mut worst = (f64::NEG_INFINITY, 0usize);
            for ch in 0..offs.len() {
                let l = inband_power(fe, &offs, ch, offs[ch] + d);
                let db = 10.0 * (l / refs[ch]).log10();
                if db > worst.0 {
                    worst = (db, ch);
                }
            }
            row.push_str(&format!(" {:>6.1}({:>2})", worst.0, worst.1));
        }
        Some(row)
    });
    for ((i, fe), row) in rows.iter().zip(res) {
        let row = row.unwrap_or_else(|| " refused".to_string());
        println!("{:<13} {:<9}{row}", ps[*i].name, fe.label());
    }
}

// ---------------------------------------------------------------- decode

const VOICE_F: [f64; 8] = [310.0, 520.0, 780.0, 1130.0, 1490.0, 1870.0, 2350.0, 2950.0];
const VOICE_A: [f64; 8] = [0.2, 0.18, 0.15, 0.13, 0.11, 0.09, 0.08, 0.06];

#[derive(Clone, Copy, Debug)]
enum Interf {
    None,
    /// Back-to-back ACARS bursts at weak + delta (Hz), carrier power rel.
    /// weak carrier (dB).
    Acars(f64, f64),
    /// Band-limited multitone AM voice at weak + delta, carrier power rel.
    /// weak (dB).
    Voice(f64, f64),
    /// AM 1 kHz tone, m = 0.8, at weak + delta, carrier power rel. weak (dB).
    Tone(f64, f64),
}

impl Interf {
    fn label(self) -> String {
        match self {
            Self::None => "none".to_string(),
            Self::Acars(d, p) => format!("acars {:+.2}k {p:+.0}dB", d / 1e3),
            Self::Voice(d, p) => format!("voice {:+.2}k {p:+.0}dB", d / 1e3),
            Self::Tone(d, p) => format!("tone  {:+.2}k {p:+.0}dB", d / 1e3),
        }
    }
}

fn weak_spec(text: &str) -> FrameSpec<'_> {
    FrameSpec {
        mode: '2',
        tail: "N471XG",
        ack: None,
        label: "H1",
        block_id: '3',
        msg_num: Some("M42A"),
        flight: Some("XG0042"),
        text,
        etb: false,
    }
}

const STRONG_SPEC: FrameSpec<'static> = FrameSpec {
    mode: '2',
    tail: "N999ZZ",
    ack: None,
    label: "H1",
    block_id: '5',
    msg_num: Some("M01B"),
    flight: Some("ZZ0001"),
    text: "STRONG NEIGHBOUR CHANNEL TRAFFIC 0123456789",
    etb: false,
};

/// Decoded weak-burst counts per front end (`None` = constructor refused).
/// Every front end sees the identical capture.
fn run_decode(
    offs: &[f64],
    weak_ch: usize,
    snr_db: f64,
    interf: Interf,
    bursts: usize,
    seed: u64,
) -> [Option<usize>; N_FE] {
    let npow = 2.0 * f64::from(SIGMA).powi(2) * 10_000.0 / FS;
    let a_weak = (npow * 10f64.powf(snr_db / 10.0)).sqrt() as f32;
    let woff = offs[weak_ch];
    let texts: Vec<String> = (0..bursts).map(|k| format!("WEAK PAYLOAD {k:04}")).collect();
    let probe = burst_iq(&weak_spec(&texts[0]), FS, 0.0, 1.0).len();
    let slot = probe + (FS * 0.08) as usize;
    let lead = FS as usize;
    let total = lead + bursts * slot + (FS * 0.2) as usize;

    let (i_off, i_db) = match interf {
        Interf::None => (0.0, f64::NEG_INFINITY),
        Interf::Acars(d, p) | Interf::Voice(d, p) | Interf::Tone(d, p) => (d, p),
    };
    let i_amp = 10f64.powf(i_db / 20.0) as f32 * a_weak;
    let strong_tmpl = match interf {
        Interf::Acars(..) => burst_iq(&STRONG_SPEC, FS, 0.0, 1.0),
        _ => Vec::new(),
    };
    let i_freq = woff + i_off;

    let mut decs: Vec<Option<AcarsMultiChannelDecoder>> =
        FES.iter().map(|&fe| build(fe, offs).ok()).collect();
    let mut got = vec![vec![false; bursts]; N_FE];
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15 ^ seed.wrapping_mul(0x2545_F491_4F6C_DD1D));
    let mut cur: Option<(usize, Vec<Complex<f32>>)> = None;
    let vrot: Vec<Complex<f64>> =
        VOICE_F.iter().map(|f| Complex::from_polar(1.0, TAU * f / FS)).collect();
    let mut vph: Vec<Complex<f64>> =
        (0..VOICE_F.len()).map(|k| Complex::from_polar(1.0, k as f64 * 1.7)).collect();
    let w_rot = Complex::from_polar(1.0, TAU * woff / FS);
    let i_rot = Complex::from_polar(1.0, TAU * i_freq / FS);
    let mut wph = Complex::new(1.0f64, 0.0);
    let mut iph = Complex::new(1.0f64, 0.0);
    let strong_phase0 = 12_345usize;

    let mut buf = vec![Complex::new(0.0f32, 0.0); CHUNK];
    let mut n0 = 0usize;
    while n0 < total {
        let len = CHUNK.min(total - n0);
        for (k, s) in buf[..len].iter_mut().enumerate() {
            let n = n0 + k;
            let (a, b) = rng.gauss_pair();
            let mut x = Complex::new(a * SIGMA, b * SIGMA);
            if n >= lead {
                let idx = (n - lead) / slot;
                let pos = (n - lead) % slot;
                if idx < bursts && pos < probe {
                    if cur.as_ref().is_none_or(|(i, _)| *i != idx) {
                        cur = Some((idx, burst_iq(&weak_spec(&texts[idx]), FS, 0.0, 1.0)));
                    }
                    if let Some(&v) = cur.as_ref().and_then(|(_, b)| b.get(pos)) {
                        x += v * Complex::new(wph.re as f32, wph.im as f32) * a_weak;
                    }
                }
            }
            wph *= w_rot;
            let r = Complex::new(iph.re as f32, iph.im as f32);
            match interf {
                Interf::None => {}
                Interf::Acars(..) => {
                    x += strong_tmpl[(n + strong_phase0) % strong_tmpl.len()] * r * i_amp;
                }
                Interf::Voice(..) => {
                    let mut au = 0.0f64;
                    for (ph, (rot, amp)) in vph.iter_mut().zip(vrot.iter().zip(VOICE_A)) {
                        au += amp * ph.im;
                        *ph *= rot;
                    }
                    let syl = 0.5 * (1.0 + (TAU * 4.0 * n as f64 / FS).sin());
                    x += r * (i_amp * (1.0 + 0.8 * (au * syl) as f32));
                    if n.is_multiple_of(4096) {
                        for ph in &mut vph {
                            *ph /= ph.norm();
                        }
                    }
                }
                Interf::Tone(..) => {
                    let au = (TAU * 1000.0 * n as f64 / FS).sin() as f32;
                    x += r * (i_amp * (1.0 + 0.8 * au));
                }
            }
            iph *= i_rot;
            if n.is_multiple_of(4096) {
                wph /= wph.norm();
                iph /= iph.norm();
            }
            *s = x;
        }
        for (j, d) in decs.iter_mut().enumerate() {
            let Some(d) = d else { continue };
            for (ch, frames) in d.process(&buf[..len]) {
                if ch != weak_ch {
                    continue;
                }
                for f in frames.iter().filter(|f| f.crc_ok) {
                    let k = f.text.strip_prefix("WEAK PAYLOAD ").and_then(|k| k.parse().ok());
                    if let Some(k) = k.filter(|&k: &usize| k < bursts) {
                        got[j][k] = true;
                    }
                }
            }
        }
        n0 += len;
    }
    let mut res = [None; N_FE];
    for (j, r) in res.iter_mut().enumerate() {
        if decs[j].is_some() {
            *r = Some(got[j].iter().filter(|&&b| b).count());
        }
    }
    res
}

fn sweep(bursts: usize) {
    println!("No interferer; weak {WEAK_MHZ} decodes of {bursts} bursts");
    let snrs = [6.0, 8.0, 10.0, 12.0, 14.0];
    let ps = plans();
    let jobs: Vec<(usize, f64)> =
        (0..ps.len()).flat_map(|i| snrs.iter().map(move |&s| (i, s))).collect();
    let res = par(jobs.len(), |j| {
        let (i, s) = jobs[j];
        run_decode(&ps[i].offsets(), ps[i].index_of(WEAK_MHZ), s, Interf::None, bursts, 1)
    });
    println!("{:<13} {:>5} {}", "plan", "SNR", fe_header());
    for ((i, s), r) in jobs.iter().zip(&res) {
        println!("{:<13} {:>5.1} {}", ps[*i].name, s, fmt_counts(r));
    }
}

fn interferers() -> Vec<Interf> {
    let mut ints = vec![Interf::None];
    for d in [-25_000.0, 25_000.0] {
        for p in [20.0, 30.0, 40.0] {
            ints.push(Interf::Acars(d, p));
        }
    }
    for d in [-20_000.0, -16_667.0, 16_667.0, 20_000.0] {
        for p in [30.0, 40.0, 50.0] {
            ints.push(Interf::Voice(d, p));
        }
    }
    for d in [-20_000.0, 20_000.0] {
        ints.push(Interf::Tone(d, 40.0));
    }
    ints
}

fn interf(bursts: usize, snr: f64) {
    println!("Weak channel @ SNR {snr} dB; weak-channel decodes of {bursts} bursts");
    let ints = interferers();
    let ps = plans();
    // Weak channels per plan: 131.525 everywhere, plus the channels the leak
    // table flags as worst-placed on the off-raster centers.
    let weak: [&[f64]; 5] = [
        &[131.525],
        &[131.525],
        &[131.525, 131.450, 131.550],
        &[131.525, 130.425, 131.550],
        &[131.525, 130.450, 130.425],
    ];
    let mut jobs = Vec::new();
    for (i, ws) in weak.iter().enumerate() {
        for &w in *ws {
            for k in 0..ints.len() {
                jobs.push((i, w, k));
            }
        }
    }
    let res = par(jobs.len(), |j| {
        let (i, w, k) = jobs[j];
        run_decode(&ps[i].offsets(), ps[i].index_of(w), snr, ints[k], bursts, 7)
    });
    println!("{:<13} {:>8} {:<22} {}", "plan", "weak", "interferer", fe_header());
    for ((i, w, k), r) in jobs.iter().zip(&res) {
        println!("{:<13} {:>8.3} {:<22} {}", ps[*i].name, w, ints[*k].label(), fmt_counts(r));
    }
}

fn base(bursts: usize, snr: f64) {
    const SEEDS: u64 = 8;
    let ps = plans();
    let jobs: Vec<(usize, u64)> =
        (0..ps.len()).flat_map(|i| (1..=SEEDS).map(move |s| (i, s))).collect();
    let res = par(jobs.len(), |j| {
        let (i, sd) = jobs[j];
        run_decode(&ps[i].offsets(), ps[i].index_of(WEAK_MHZ), snr, Interf::None, bursts, 100 + sd)
    });
    println!("weak {WEAK_MHZ}, no interferer, SNR {snr} dB, {SEEDS} seeds x {bursts} bursts");
    println!("{:<13} {} {:>9}", "plan", fe_header(), "of");
    for (i, p) in ps.iter().enumerate() {
        let mut t: [Option<usize>; N_FE] = [Some(0); N_FE];
        for ((ii, _), r) in jobs.iter().zip(&res) {
            if *ii == i {
                for (acc, v) in t.iter_mut().zip(r) {
                    *acc = acc.zip(*v).map(|(a, b)| a + b);
                }
            }
        }
        println!("{:<13} {} {:>9}", p.name, fmt_counts(&t), SEEDS as usize * bursts);
    }
}

// ---------------------------------------------------------------- cpu

fn cpu(reps: usize) {
    let dur = 4.0;
    let n = (FS * dur) as usize;
    let mut r = Rng(42);
    let cap: Vec<Complex<f32>> = (0..n)
        .map(|_| {
            let (a, b) = r.gauss_pair();
            Complex::new(a * SIGMA, b * SIGMA)
        })
        .collect();
    println!(
        "CPU, idle noise, {dur} s @ 2.4 MS/s, min of {reps}, % of one core \
         (end-to-end decoder / front end only)"
    );
    for p in plans() {
        let offs = p.offsets();
        let mut line = format!("{:<13}", p.name);
        for fe in FES {
            let (Ok(mut d), Ok(mut f)) = (build(fe, &offs), FrontOnly::new(fe, &offs)) else {
                line.push_str(&format!("  {} refused", fe.label()));
                continue;
            };
            let mut best = f64::INFINITY;
            for _ in 0..reps.max(1) {
                let t = Instant::now();
                for ch in cap.chunks(CHUNK) {
                    d.process(ch);
                }
                best = best.min(t.elapsed().as_secs_f64());
            }
            let mut out = vec![Vec::new(); offs.len()];
            let mut best_front = f64::INFINITY;
            for _ in 0..reps.max(1) {
                let t = Instant::now();
                for ch in cap.chunks(CHUNK) {
                    f.process(ch, &mut out);
                }
                best_front = best_front.min(t.elapsed().as_secs_f64());
            }
            line.push_str(&format!(
                "  {} {:.2}% / {:.2}%",
                fe.label(),
                best / dur * 100.0,
                best_front / dur * 100.0
            ));
        }
        println!("{line}");
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let mode = args.get(1).map_or("describe", String::as_str);
    let trials = |default: usize| args.get(2).and_then(|s| s.parse().ok()).unwrap_or(default);
    let snr: f64 = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(9.0);
    match mode {
        "describe" => describe(),
        "leak" => leak(),
        "sweep" => sweep(trials(100)),
        "interf" => interf(trials(100), snr),
        "base" => base(trials(100), snr),
        "cpu" => cpu(trials(7)),
        other => {
            eprintln!(
                "unknown mode {other:?}; expected describe|leak|sweep|interf|base|cpu \
                 [trials] [snr_db]"
            );
            std::process::exit(2);
        }
    }
}
