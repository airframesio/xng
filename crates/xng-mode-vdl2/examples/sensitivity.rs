//! Sensitivity A/B: shaped burst at swept noise, decode rate with and
//! without the channel-selectivity lowpass. Long trailing noise lets
//! phantom header locks fail and rewind, as in a live stream.
use num_complex::Complex;
use xng_mode_vdl2::avlc::{self, encode_address, AddressType};
use xng_mode_vdl2::modulate::burst_iq_shaped;
use xng_mode_vdl2::{demod::Vdl2Demod, interleave};

fn main() {
    let mut f = Vec::new();
    f.extend(encode_address(AddressType::GroundIcao, 0x10A234, false, false));
    f.extend(encode_address(AddressType::Aircraft, 0x800F5C, true, true));
    f.push(0x01);
    let frames = vec![f];
    let truth = avlc::build(&frames);
    let rate = 50_000.0;
    let burst = burst_iq_shaped(&frames, rate, 0.0, 0.5);
    let sig_pow: f32 =
        burst.iter().map(|c| c.norm_sqr()).sum::<f32>() / burst.len() as f32;
    let rs = interleave::vdl2_rs();
    let taps = xng_dsp::fir::lowpass_taps(10_500.0 / rate, 101);

    println!("amp    snr_dB   plain  filt   (of 40 trials)");
    for amp in [0.02f32, 0.05, 0.08, 0.11, 0.14, 0.17, 0.20, 0.25] {
        let noise_pow = 2.0 * amp * amp / 3.0;
        let snr = 10.0 * (sig_pow / noise_pow).log10();
        let mut ok = [0u32; 2];
        for trial in 0..40u64 {
            let mut iq = vec![Complex::new(0.0f32, 0.0); 2000];
            iq.extend(burst.iter().copied());
            iq.extend(vec![Complex::new(0.0f32, 0.0); 30_000]);
            let mut seed = 0x9e37_79b9_7f4a_7c15u64.wrapping_mul(trial + 1) | 1;
            let mut noise = move || {
                seed ^= seed << 13;
                seed ^= seed >> 7;
                seed ^= seed << 17;
                (seed as f32 / u64::MAX as f32) * 2.0 - 1.0
            };
            for s in &mut iq {
                *s += Complex::new(noise() * amp, noise() * amp);
            }
            let mut filt = Vec::new();
            let mut fir = xng_dsp::fir::Fir::new(taps.clone());
            fir.process(&iq, &mut filt);
            for (i, sig) in [&iq, &filt].into_iter().enumerate() {
                let mut demod = Vdl2Demod::new(rate);
                let good = demod
                    .process(sig, &rs)
                    .iter()
                    .any(|b| b.bits == truth);
                if good {
                    ok[i] += 1;
                }
            }
        }
        println!("{amp:<6} {snr:>5.1}    {:>3}    {:>3}", ok[0], ok[1]);
    }
}
