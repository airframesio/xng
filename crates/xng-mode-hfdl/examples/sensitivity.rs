//! Sensitivity A/B: HFDL burst at swept noise, decode rate with and
//! without the no-DDC channel-selectivity lowpass. The modulator is
//! rectangular-phase; the LMS equalizer absorbs the filter's static
//! ISI exactly as it does off-air, so the comparison is fair.
use num_complex::Complex;
use xng_mode_hfdl::modulate::{burst_symbols, modulate};
use xng_mode_hfdl::{fec::SETTINGS, pdu, HfdlChannelDecoder, CHANNEL_RATE};

fn main() {
    let spdu = pdu::build_spdu(7, 1234, 52);
    let s = &SETTINGS[2]; // 1200 bps — the workhorse squitter rate
    let syms = burst_symbols(&spdu, s);
    let burst = modulate(&syms, CHANNEL_RATE, 1440.0, 0.5);
    let sig_pow: f32 =
        burst.iter().map(|c| c.norm_sqr()).sum::<f32>() / burst.len() as f32;

    println!("amp    snr_dB   plain  filt   (of 40 trials)");
    for amp in [0.3f32, 0.4, 0.5, 0.6, 0.7, 0.85, 1.0, 1.2] {
        let noise_pow = 2.0 * amp * amp / 3.0;
        let snr = 10.0 * (sig_pow / noise_pow).log10();
        let mut ok = [0u32; 2];
        for trial in 0..40u64 {
            let mut iq = vec![Complex::new(0.0f32, 0.0); 3000];
            iq.extend(burst.iter().copied());
            iq.extend(vec![Complex::new(0.0f32, 0.0); 3000]);
            let mut seed = 0x517c_c1b7_2722_0a95u64.wrapping_mul(trial + 1) | 1;
            let mut noise = move || {
                seed ^= seed << 13;
                seed ^= seed >> 7;
                seed ^= seed << 17;
                (seed as f32 / u64::MAX as f32) * 2.0 - 1.0
            };
            for x in &mut iq {
                *x += Complex::new(noise() * amp, noise() * amp);
            }
            // Filtered = the shipped decoder; plain = demod + parser
            // directly, skipping the selectivity filter.
            let mut dec = HfdlChannelDecoder::new(CHANNEL_RATE, 0.0).unwrap();
            let mut got = false;
            for chunk in iq.chunks(8192) {
                if dec
                    .process(chunk)
                    .iter()
                    .any(|e| e.kind == "squitter" && e.details["frame_index"] == 1234)
                {
                    got = true;
                }
            }
            if got {
                ok[1] += 1;
            }
            // Plain: demod + parser directly, no selectivity filter.
            let mut demod = xng_mode_hfdl::demod::HfdlDemod::new(CHANNEL_RATE);
            let mut parser = pdu::PduParser::new();
            let mut got = false;
            for chunk in iq.chunks(8192) {
                for b in demod.process(chunk) {
                    if parser
                        .parse(&b.payload, b.bps)
                        .iter()
                        .any(|e| e.kind == "squitter" && e.details["frame_index"] == 1234)
                    {
                        got = true;
                    }
                }
            }
            if got {
                ok[0] += 1;
            }
        }
        println!("{amp:<6} {snr:>5.1}    {:>3}    {:>3}", ok[0], ok[1]);
    }
}
