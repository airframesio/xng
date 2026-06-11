//! AIS sensitivity baseline: GMSK burst at swept noise, decode rate of
//! the current discriminator demod (and any future coherent path).
use num_complex::Complex;
use xng_mode_ais::modulate::burst_iq_gmsk;
use xng_mode_ais::nmea::payload_to_bits;
use xng_mode_ais::AisChannelDecoder;

const KNOWN_PAYLOAD: &str = "177KQJ5000G?tO`K>RA1wUbN0TKH";

fn main() {
    let msg_bits = payload_to_bits(KNOWN_PAYLOAD);
    let burst = burst_iq_gmsk(&msg_bits, 48_000.0, 0.0, 0.5);
    let sig_pow: f32 =
        burst.iter().map(|c| c.norm_sqr()).sum::<f32>() / burst.len() as f32;

    println!("amp    snr_dB   ok/40");
    for amp in [0.05f32, 0.10, 0.15, 0.20, 0.25, 0.30, 0.40, 0.55] {
        let noise_pow = 2.0 * amp * amp / 3.0;
        let snr = 10.0 * (sig_pow / noise_pow).log10();
        let mut ok = 0u32;
        for trial in 0..40u64 {
            let mut iq = vec![Complex::new(0.0f32, 0.0); 400];
            iq.extend(burst.iter().copied());
            // The coherent path buffers a full max-length burst past its
            // anchor before deciding; live streams never end (the VDL2
            // stream-end lesson).
            iq.extend(vec![Complex::new(0.0f32, 0.0); 2500]);
            let mut seed = 0x2545_f491_4f6c_dd1du64.wrapping_mul(trial + 1) | 1;
            let mut noise = move || {
                seed ^= seed << 13;
                seed ^= seed >> 7;
                seed ^= seed << 17;
                (seed as f32 / u64::MAX as f32) * 2.0 - 1.0
            };
            for s in &mut iq {
                *s += Complex::new(noise() * amp, noise() * amp);
            }
            let mut dec = AisChannelDecoder::new(48_000.0, 0.0, 162_025_000).unwrap();
            let mut found = false;
            for chunk in iq.chunks(512) {
                if dec.process(chunk).iter().any(|(f, _)| f.mmsi == 477_553_000) {
                    found = true;
                }
            }
            if found {
                ok += 1;
            }
        }
        println!("{amp:<6} {snr:>5.1}    {ok:>3}");
    }
}
