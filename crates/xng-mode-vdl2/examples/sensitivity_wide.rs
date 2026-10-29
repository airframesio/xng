//! Decode success vs noise through the LIVE wideband path: an RC-shaped VDL2
//! burst inside a 2.4 MS/s capture, decoded by `Vdl2ChannelDecoder` with its
//! full-rate DDC, at a near-center and a plan-edge channel offset. The
//! reference column is the same burst at 105 kS/s through the no-DDC path
//! (what the bench fixtures exercise), at the same in-channel SNR. Use it to
//! A/B front-end changes.
//!
//! ```bash
//! cargo run --release -p xng-mode-vdl2 --example sensitivity_wide
//! ```

use num_complex::Complex;
use xng_mode_vdl2::avlc::{encode_address, AddressType};
use xng_mode_vdl2::demod::SYMBOL_RATE;
use xng_mode_vdl2::modulate::burst_iq_shaped;
use xng_mode_vdl2::Vdl2ChannelDecoder;

const FS: f64 = 2_400_000.0;
const REF_RATE: f64 = 105_000.0;
const OFFSETS: [f64; 2] = [12_500.0, 162_500.0];
/// Target in-channel SNR per sweep point; see `main` for the definition.
const SNR_DB: [f64; 9] = [26.0, 24.0, 23.0, 22.0, 21.0, 20.0, 19.0, 18.0, 16.0];
const TRIALS: u64 = 100;
const SIG_AMP: f32 = 0.5;
const LEAD: usize = 20_000;
/// 0.5 s of noise after the burst. The decoder can currently hold a burst
/// back until more samples arrive (observed, under investigation); a short
/// tail turned that hold-back into ~35% "misses" at any SNR.
const TAIL: usize = 1_200_000;
const CHUNK: usize = 65_536;

fn aoa_frame() -> Vec<u8> {
    let mut f = Vec::new();
    f.extend(encode_address(
        AddressType::GroundIcao,
        0x10A234,
        false,
        false,
    ));
    f.extend(encode_address(AddressType::Aircraft, 0x800F5C, false, true));
    f.push(0x03); // UI
    f.push(0xFF); // AOA IPI
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

/// One trial: `lead` noise, burst, `tail` noise, uniform ±`amp` on I/Q,
/// decoded in `CHUNK`s by a fresh decoder.
struct Trial<'a> {
    burst: &'a [Complex<f32>],
    lead: usize,
    tail: usize,
    amp: f32,
    seed: u64,
    rate: f64,
    off: f64,
    truth: &'a [u8],
}

impl Trial<'_> {
    /// Success means the transmitted frame came back byte-exact:
    /// `AvlcFrame::raw` is the whole frame (addresses..FCS) and the scanner
    /// only emits FCS-valid frames, so `raw` minus its 2 FCS octets equal to
    /// the transmitted octets is the strictest equality the public API
    /// offers — stricter than comparing the ACARS text, which would ignore
    /// addresses/control and the ACARS header.
    fn run(&self) -> bool {
        let mut iq = vec![Complex::new(0.0f32, 0.0); self.lead];
        iq.extend_from_slice(self.burst);
        iq.resize(iq.len() + self.tail, Complex::new(0.0, 0.0));
        let mut s = self.seed | 1;
        let mut nx = move || {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            (s as f32 / u64::MAX as f32) * 2.0 - 1.0
        };
        for x in &mut iq {
            *x += Complex::new(nx() * self.amp, nx() * self.amp);
        }
        let mut dec =
            Vdl2ChannelDecoder::new(self.rate, self.off).expect("build VDL2 channel decoder");
        let truth = self.truth;
        iq.chunks(CHUNK).any(|c| {
            dec.process(c)
                .iter()
                .any(|f| f.avlc.raw.len() == truth.len() + 2 && f.avlc.raw[..truth.len()] == *truth)
        })
    }
}

fn main() {
    let frame = aoa_frame();
    let frames = vec![frame.clone()];
    let wide: Vec<Vec<Complex<f32>>> = OFFSETS
        .iter()
        .map(|&off| burst_iq_shaped(&frames, FS, off, SIG_AMP))
        .collect();
    let reference = burst_iq_shaped(&frames, REF_RATE, 0.0, SIG_AMP);
    let sig_pow = reference
        .iter()
        .map(|c| f64::from(c.norm_sqr()))
        .sum::<f64>()
        / reference.len() as f64;

    // Uniform ±a on I and Q is white across the whole sampled band with
    // per-sample complex power 2a²/3, i.e. PSD (2a²/3)/rate. In-channel SNR
    // is signal power over that PSD integrated across one symbol-rate
    // bandwidth: SNR = P_sig / ((2a²/3)·Rs/FS), so for a target SNR
    // a = sqrt(1.5 · P_sig · FS / (Rs · 10^(SNR/10))).
    //
    // Matching the in-channel SNR at 105 kS/s means matching that PSD, so the
    // per-sample amplitude scales by sqrt(105_000 / 2_400_000). Signal
    // amplitude is the same at both rates, and lead/tail are scaled to the
    // same durations.
    let ref_scale = (REF_RATE / FS).sqrt() as f32;
    let ref_lead = (LEAD as f64 * REF_RATE / FS) as usize;
    let ref_tail = (TAIL as f64 * REF_RATE / FS) as usize;

    println!("VDL2 wideband sensitivity — {FS:.0} S/s, burst amp {SIG_AMP}, {TRIALS} trials/point");
    println!(
        "SNR = signal power / noise power in one symbol-rate ({SYMBOL_RATE:.0} Hz) bandwidth;\n\
         amp = per-sample uniform noise ±amp on I/Q at {FS:.0} S/s\n"
    );
    println!(
        "{:>7}{:>9}{:>10}{:>10}{:>12}",
        "SNR dB", "amp", "+12.5k", "+162.5k", "ref 105k"
    );
    for (si, &snr) in SNR_DB.iter().enumerate() {
        let amp = (1.5 * sig_pow * FS / (SYMBOL_RATE * 10f64.powf(snr / 10.0))).sqrt() as f32;
        let mut ok = [0u32; 3];
        for t in 0..TRIALS {
            let seed = 0x9e37_79b9_7f4a_7c15u64.wrapping_mul(t + 1) ^ ((si as u64 + 1) << 40);
            for (i, (&off, burst)) in OFFSETS.iter().zip(&wide).enumerate() {
                let tr = Trial {
                    burst,
                    lead: LEAD,
                    tail: TAIL,
                    amp,
                    seed,
                    rate: FS,
                    off,
                    truth: &frame,
                };
                if tr.run() {
                    ok[i] += 1;
                }
            }
            let tr = Trial {
                burst: &reference,
                lead: ref_lead,
                tail: ref_tail,
                amp: amp * ref_scale,
                seed,
                rate: REF_RATE,
                off: 0.0,
                truth: &frame,
            };
            if tr.run() {
                ok[2] += 1;
            }
        }
        println!(
            "{snr:>7.1}{amp:>9.3}{:>10}{:>10}{:>12}",
            ok[0], ok[1], ok[2]
        );
    }
}
