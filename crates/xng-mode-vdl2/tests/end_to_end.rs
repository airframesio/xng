//! RF loopback: AVLC/AOA frames → D8PSK burst → decoder.

use num_complex::Complex;
use xng_dsp::Oversample;
use xng_mode_vdl2::avlc::{encode_address, AddressType};
use xng_mode_vdl2::modulate::burst_iq;
use xng_mode_vdl2::{Vdl2ChannelDecoder, Vdl2MultiChannelDecoder};

struct Noise(u64);
impl Noise {
    fn next(&mut self) -> f32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 as f32 / u64::MAX as f32) * 2.0 - 1.0
    }
}

/// Downlink AOA frame: aircraft → ground UI frame carrying an ACARS block
/// with a real ADS-C payload.
fn aoa_frame() -> Vec<u8> {
    let mut f = Vec::new();
    f.extend(encode_address(AddressType::GroundIcao, 0x10A234, false, false)); // dst
    f.extend(encode_address(AddressType::Aircraft, 0x800F5C, false, true)); // src
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

fn rr_frame() -> Vec<u8> {
    let mut f = Vec::new();
    f.extend(encode_address(AddressType::Aircraft, 0x800F5C, true, false));
    f.extend(encode_address(AddressType::GroundIcao, 0x10A234, true, true));
    f.push(0x01); // RR, NR=0
    f
}

#[test]
fn decodes_burst_at_channel_rate() {
    let iq_burst = burst_iq(&[aoa_frame(), rr_frame()], 50_000.0, 0.0, 0.5);
    let mut iq = vec![Complex::new(0.0, 0.0); 800];
    iq.extend(iq_burst);
    // Generous trailing noise: a phantom UW lock in the lead-in whose
    // garbage header passes the thin 25-bit FEC needs enough stream to
    // starve, fail RS, and rewind — live SDR streams never end.
    iq.extend(vec![Complex::new(0.0, 0.0); 30_000]);
    let mut noise = Noise(0xabcd_ef01_2345_6789);
    for s in &mut iq {
        *s += Complex::new(noise.next() * 0.01, noise.next() * 0.01);
    }

    let mut dec = Vdl2ChannelDecoder::new(50_000.0, 0.0).unwrap();
    let mut frames = Vec::new();
    for chunk in iq.chunks(1024) {
        frames.extend(dec.process(chunk));
    }
    assert_eq!(frames.len(), 2, "expected both AVLC frames");

    let acars = frames[0].acars.as_ref().expect("first frame carries ACARS");
    assert!(acars.crc_ok);
    assert_eq!(acars.core.tail.as_deref(), Some("VT-ANB"));
    assert_eq!(acars.core.label, "B6");
    assert_eq!(acars.core.flight.as_deref(), Some("AI0142"));
    let app = acars.core.app.as_ref().expect("ADS-C decodes");
    assert_eq!(app["app"], "adsc");
    assert_eq!(app["crc_ok"], true);
    assert_eq!(frames[0].avlc.src.addr, "800F5C");
    assert_eq!(frames[0].avlc.dst.addr, "10A234");

    assert!(frames[1].acars.is_none());
}

// VDL2-7: the burst's measured carrier offset tracks an injected CFO (the
// injected value is independent ground truth, not a decode loopback).
#[test]
fn freq_skew_tracks_injected_cfo() {
    for cfo in [150.0_f64, -250.0, 400.0] {
        let iq_burst = burst_iq(&[aoa_frame()], 50_000.0, cfo, 0.5);
        let mut iq = vec![Complex::new(0.0, 0.0); 800];
        iq.extend(iq_burst);
        iq.extend(vec![Complex::new(0.0, 0.0); 30_000]);
        let mut dec = Vdl2ChannelDecoder::new(50_000.0, 0.0).unwrap();
        let mut frames = Vec::new();
        for chunk in iq.chunks(1024) {
            frames.extend(dec.process(chunk));
        }
        assert!(!frames.is_empty(), "decoded at cfo={cfo}");
        let skew = frames[0].freq_skew_hz as f64;
        assert!((skew - cfo).abs() < 40.0, "freq_skew {skew} Hz vs injected {cfo} Hz");
    }
}

// VDL2-8: the EVM-derived per-burst SNR (from the symbol decision residuals)
// is finite and falls when more noise is mixed into the same burst.
#[test]
fn evm_snr_drops_with_more_noise() {
    let snr_at = |amp: f32| -> Option<f32> {
        let mut iq = vec![Complex::new(0.0, 0.0); 800];
        iq.extend(burst_iq(&[aoa_frame()], 50_000.0, 0.0, 0.5));
        iq.extend(vec![Complex::new(0.0, 0.0); 30_000]);
        let mut noise = Noise(0x51b3_2c9f_a7e1_0d44);
        for s in &mut iq {
            *s += Complex::new(noise.next() * amp, noise.next() * amp);
        }
        let mut dec = Vdl2ChannelDecoder::new(50_000.0, 0.0).unwrap();
        let mut frames = Vec::new();
        for chunk in iq.chunks(1024) {
            frames.extend(dec.process(chunk));
        }
        frames.first().map(|f| f.snr_db)
    };
    let clean = snr_at(0.01).expect("decodes at low noise");
    let noisy = snr_at(0.06).expect("decodes at higher noise");
    assert!(clean.is_finite() && noisy.is_finite(), "clean {clean} noisy {noisy}");
    assert!(noisy < clean, "more noise → lower EVM SNR: {noisy} vs {clean}");
}

// VDL2-7: an aggressive --max-ppm reject drops a far-off-frequency burst,
// while a generous limit keeps it (and zero-CFO bursts always pass).
#[test]
fn max_ppm_rejects_far_off_frequency_bursts() {
    // 400 Hz / 137 MHz ≈ 2.9 ppm. A 1 ppm gate rejects it; 10 ppm keeps it.
    let decode_with = |max_ppm: Option<f64>| -> usize {
        let mut iq = vec![Complex::new(0.0, 0.0); 800];
        iq.extend(burst_iq(&[aoa_frame()], 50_000.0, 400.0, 0.5));
        iq.extend(vec![Complex::new(0.0, 0.0); 30_000]);
        let mut dec = Vdl2ChannelDecoder::new(50_000.0, 0.0).unwrap();
        dec.set_max_ppm(max_ppm);
        let mut n = 0;
        for chunk in iq.chunks(1024) {
            n += dec.process(chunk).len();
        }
        n
    };
    assert!(decode_with(None) >= 1, "no gate → decodes");
    assert!(decode_with(Some(10.0)) >= 1, "generous gate → decodes");
    assert_eq!(decode_with(Some(1.0)), 0, "tight gate → rejected");
}

#[test]
fn decodes_from_wideband_capture_with_cfo() {
    // 2.4 MS/s capture centered at 136.900 MHz; VDL2 CSC at 136.975
    // (+75 kHz) with a 400 Hz carrier offset error.
    let fs = 2_400_000.0;
    let burst = burst_iq(&[aoa_frame()], fs, 75_000.0 + 400.0, 0.4);

    let total = burst.len() + 60_000;
    let mut iq = vec![Complex::new(0.0f32, 0.0f32); total];
    for (i, s) in burst.iter().enumerate() {
        iq[i + 30_000] += s;
    }
    let mut noise = Noise(0x1357_9bdf_0246_8ace);
    for s in &mut iq {
        *s += Complex::new(noise.next() * 0.01, noise.next() * 0.01);
    }

    let mut dec = Vdl2ChannelDecoder::new(fs, 75_000.0).unwrap();
    let mut frames = Vec::new();
    for chunk in iq.chunks(65_536) {
        frames.extend(dec.process(chunk));
    }
    assert_eq!(frames.len(), 1, "expected the AOA frame despite CFO");
    let acars = frames[0].acars.as_ref().unwrap();
    assert!(acars.crc_ok);
    assert_eq!(acars.core.text.len(), 79);
}

/// The live path (2.4 MS/s capture → DDC → channel rate) must be as
/// channel-selective as the no-DDC channel-rate path. The DDC's final stage
/// is only an anti-alias filter, so without the channel lowpass after it
/// out-of-channel noise reaches the demod and costs ~2 dB: at 20 dB
/// in-channel SNR the live path decoded ~10% of bursts before the fix and
/// ~95% after.
#[test]
fn wideband_path_matches_channel_rate_sensitivity() {
    use xng_mode_vdl2::demod::SYMBOL_RATE;
    use xng_mode_vdl2::modulate::burst_iq_shaped;
    const FS: f64 = 2_400_000.0;
    const SNR_DB: f64 = 20.0;
    const TRIALS: u64 = 8;
    const LEAD: usize = 20_000;
    // The decoder can hold a burst back until more samples arrive (a known
    // issue, tracked separately); a long tail keeps that from looking like
    // a sensitivity miss.
    const TAIL: usize = 1_200_000;

    let frame = aoa_frame();
    let frames = [frame.clone()];
    let burst = burst_iq_shaped(&frames, FS, 12_500.0, 0.5);
    // In-channel SNR as in examples/sensitivity_wide.rs: signal power over
    // the uniform ±a noise PSD integrated across one symbol-rate bandwidth.
    let reference = burst_iq_shaped(&frames, 105_000.0, 0.0, 0.5);
    let sig_pow = reference
        .iter()
        .map(|c| f64::from(c.norm_sqr()))
        .sum::<f64>()
        / reference.len() as f64;
    let amp = (1.5 * sig_pow * FS / (SYMBOL_RATE * 10f64.powf(SNR_DB / 10.0))).sqrt() as f32;

    let mut successes = 0;
    for t in 0..TRIALS {
        let mut iq = vec![Complex::new(0.0f32, 0.0); LEAD];
        iq.extend_from_slice(&burst);
        iq.resize(iq.len() + TAIL, Complex::new(0.0, 0.0));
        let mut noise = Noise(0x9e37_79b9_7f4a_7c15u64.wrapping_mul(t + 1) | 1);
        for s in &mut iq {
            *s += Complex::new(noise.next() * amp, noise.next() * amp);
        }
        let mut dec = Vdl2ChannelDecoder::new(FS, 12_500.0).unwrap();
        let ok = iq.chunks(65_536).any(|c| {
            dec.process(c).iter().any(|f| {
                f.avlc.raw.len() == frame.len() + 2 && f.avlc.raw[..frame.len()] == frame[..]
            })
        });
        if ok {
            successes += 1;
        }
    }
    assert!(
        successes >= 6,
        "live 2.4 MS/s path decoded {successes}/{TRIALS} at {SNR_DB} dB in-channel SNR; \
         it must be as selective as the channel-rate path (~95% here; ~10% without \
         the channel lowpass after the DDC)"
    );
}

/// The pulse-shaped (RC α=0.6) modulator is the realistic loopback: RC
/// is Nyquist, so the existing symbol-center demod must decode it
/// cleanly at both channel rates and survive moderate noise.
#[test]
fn decodes_pulse_shaped_burst() {
    use xng_mode_vdl2::modulate::burst_iq_shaped;
    for rate in [50_000.0, 100_000.0] {
        let iq_burst = burst_iq_shaped(&[aoa_frame(), rr_frame()], rate, 0.0, 0.5);
        let pad = (rate / 50.0) as usize;
        let mut iq = vec![Complex::new(0.0, 0.0); pad];
        iq.extend(iq_burst);
        iq.extend(vec![Complex::new(0.0, 0.0); 30_000]);
        let mut noise = Noise(0x1357_9bdf_2468_ace0);
        for s in &mut iq {
            *s += Complex::new(noise.next() * 0.02, noise.next() * 0.02);
        }

        let mut dec = Vdl2ChannelDecoder::new(rate, 0.0).unwrap();
        let mut frames = Vec::new();
        for chunk in iq.chunks(4096) {
            frames.extend(dec.process(chunk));
        }
        assert_eq!(frames.len(), 2, "rate {rate}");
        assert!(frames[0].acars.is_some(), "rate {rate}: AOA frame decodes");
    }
}

/// The live 7-channel plan inside a 2.4 MS/s capture centered at
/// 136.8125 MHz (136.650 … 136.975 MHz).
const PLAN_OFFSETS: [f64; 7] =
    [-162_500.0, -87_500.0, -37_500.0, 12_500.0, 62_500.0, 112_500.0, 162_500.0];

/// AOA frame unique per channel (distinct source address and tail).
fn chan_frame(chan: usize) -> Vec<u8> {
    let mut f = Vec::new();
    f.extend(encode_address(AddressType::GroundIcao, 0x10A234, false, false));
    f.extend(encode_address(AddressType::Aircraft, 0xA0_0000 + chan as u32, false, true));
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
        "MULTI-CHANNEL FRONT END PAYLOAD",
        false,
    ));
    f
}

/// `raw` is the whole AVLC frame (addresses..FCS); minus its 2 FCS octets it
/// must equal the transmitted octets.
fn is_frame(raw: &[u8], truth: &[u8]) -> bool {
    raw.len() == truth.len() + 2 && raw[..truth.len()] == *truth
}

/// Feed `iq` through `dec` in runtime-sized chunks; every decoded frame as
/// `(channel, raw)`.
fn run_multi(dec: &mut Vdl2MultiChannelDecoder, iq: &[Complex<f32>]) -> Vec<(usize, Vec<u8>)> {
    let mut out = Vec::new();
    for chunk in iq.chunks(65_536) {
        for (ch, frames) in dec.process(chunk) {
            out.extend(frames.into_iter().map(|f| (ch, f.avlc.raw)));
        }
    }
    out
}

/// Every multi-channel front end the capture rate supports, labelled.
fn multi_fronts(fs: f64, offsets: &[f64]) -> Vec<(&'static str, Vdl2MultiChannelDecoder)> {
    let mut v = vec![
        ("new", Vdl2MultiChannelDecoder::new(fs, offsets).unwrap()),
        ("new_shared", Vdl2MultiChannelDecoder::new_shared(fs, offsets).unwrap()),
    ];
    if let Ok(d) = Vdl2MultiChannelDecoder::new_channelized(fs, offsets) {
        v.push(("new_channelized", d));
    }
    if let Ok(d) =
        Vdl2MultiChannelDecoder::new_channelized_with_oversample(fs, offsets, Oversample::Twice)
    {
        v.push(("new_channelized_2x", d));
    }
    v
}

/// The default front end on the live 7-channel plan is the 2× oversampled
/// channelizer (PERF-20).
#[test]
fn multi_channel_default_front_is_oversampled_channelizer() {
    let dec = Vdl2MultiChannelDecoder::new(2_400_000.0, &PLAN_OFFSETS).unwrap();
    let front = dec.describe_front();
    assert!(front.starts_with("channelized ") && front.contains("Twice"), "got {front}");
}

/// A plan the 2× channelizer cannot serve falls back to the shared
/// decimation: at 130 kS/s (50 kS/s channel rate) the only oversampled bin
/// count hands the resampler 65 kS/s, past its 1.25× limit.
#[test]
fn multi_channel_default_falls_back_to_shared() {
    const FS: f64 = 130_000.0;
    const OFFSETS: [f64; 2] = [-12_500.0, 12_500.0];
    let twice =
        Vdl2MultiChannelDecoder::new_channelized_with_oversample(FS, &OFFSETS, Oversample::Twice);
    assert!(twice.is_err(), "2x channelizer unexpectedly serves {FS} S/s");
    let dec = Vdl2MultiChannelDecoder::new(FS, &OFFSETS).unwrap();
    assert_eq!(dec.describe_front(), "shared");
}

#[test]
fn multi_channel_decodes_every_channel() {
    use xng_mode_vdl2::modulate::burst_iq_shaped;
    const FS: f64 = 2_400_000.0;
    const LEAD: usize = 12_000;
    const STAGGER: usize = 30_000;
    const TAIL: usize = 1_200_000;
    let frames: Vec<Vec<u8>> = (0..PLAN_OFFSETS.len()).map(chan_frame).collect();
    let bursts: Vec<Vec<Complex<f32>>> = PLAN_OFFSETS
        .iter()
        .zip(&frames)
        .map(|(&off, f)| burst_iq_shaped(std::slice::from_ref(f), FS, off, 0.3))
        .collect();
    let longest = bursts.iter().map(Vec::len).max().unwrap();
    let mut iq = vec![Complex::new(0.0f32, 0.0); LEAD + STAGGER * bursts.len() + longest + TAIL];
    for (c, b) in bursts.iter().enumerate() {
        let at = LEAD + c * STAGGER;
        for (d, s) in iq[at..at + b.len()].iter_mut().zip(b) {
            *d += s;
        }
    }
    let mut noise = Noise(0x0bad_cafe_dead_beef);
    for s in &mut iq {
        *s += Complex::new(noise.next() * 0.01, noise.next() * 0.01);
    }

    for (name, mut dec) in multi_fronts(FS, &PLAN_OFFSETS) {
        assert_eq!(dec.num_channels(), PLAN_OFFSETS.len());
        assert!((dec.channel_rate() - 100_000.0).abs() < 1e-6, "{name}");
        let got = run_multi(&mut dec, &iq);
        for (c, truth) in frames.iter().enumerate() {
            let hits = got.iter().filter(|(_, raw)| is_frame(raw, truth)).collect::<Vec<_>>();
            assert!(
                hits.iter().any(|(ch, _)| *ch == c),
                "{name}: channel {c} frame not decoded on channel {c} (decoded on {:?})",
                hits.iter().map(|(ch, _)| ch).collect::<Vec<_>>()
            );
            assert!(
                hits.iter().all(|(ch, _)| *ch == c),
                "{name}: channel {c} frame leaked onto {:?}",
                hits.iter().map(|(ch, _)| ch).collect::<Vec<_>>()
            );
        }
    }
}

/// The shared front end must not cost sensitivity against the per-channel
/// DDC: same in-channel SNR definition and trial shape as
/// `wideband_path_matches_channel_rate_sensitivity`, at a near-center and a
/// plan-edge channel, with every other plan channel configured but quiet.
#[test]
fn multi_channel_matches_per_channel_sensitivity() {
    use xng_mode_vdl2::demod::SYMBOL_RATE;
    use xng_mode_vdl2::modulate::burst_iq_shaped;
    const FS: f64 = 2_400_000.0;
    const SNR_DB: f64 = 20.0;
    const TRIALS: u64 = 8;
    const LEAD: usize = 20_000;
    const TAIL: usize = 1_200_000;

    let frame = aoa_frame();
    let frames = [frame.clone()];
    let reference = burst_iq_shaped(&frames, 105_000.0, 0.0, 0.5);
    let sig_pow = reference
        .iter()
        .map(|c| f64::from(c.norm_sqr()))
        .sum::<f64>()
        / reference.len() as f64;
    let amp = (1.5 * sig_pow * FS / (SYMBOL_RATE * 10f64.powf(SNR_DB / 10.0))).sqrt() as f32;

    for (ch, off) in [(3usize, 12_500.0), (6, 162_500.0)] {
        assert_eq!(PLAN_OFFSETS[ch], off);
        let burst = burst_iq_shaped(&frames, FS, off, 0.5);
        let (mut single_ok, mut multi_ok) = (0, 0);
        for t in 0..TRIALS {
            let mut iq = vec![Complex::new(0.0f32, 0.0); LEAD];
            iq.extend_from_slice(&burst);
            iq.resize(iq.len() + TAIL, Complex::new(0.0, 0.0));
            let mut noise = Noise(0x9e37_79b9_7f4a_7c15u64.wrapping_mul(t + 1) | 1);
            for s in &mut iq {
                *s += Complex::new(noise.next() * amp, noise.next() * amp);
            }
            let mut single = Vdl2ChannelDecoder::new(FS, off).unwrap();
            if iq
                .chunks(65_536)
                .any(|c| single.process(c).iter().any(|f| is_frame(&f.avlc.raw, &frame)))
            {
                single_ok += 1;
            }
            let mut multi = Vdl2MultiChannelDecoder::new(FS, &PLAN_OFFSETS).unwrap();
            if run_multi(&mut multi, &iq).iter().any(|(c, raw)| *c == ch && is_frame(raw, &frame)) {
                multi_ok += 1;
            }
        }
        assert!(
            multi_ok + 1 >= single_ok,
            "offset {off:+} Hz at {SNR_DB} dB in-channel SNR: multi-channel front end decoded \
             {multi_ok}/{TRIALS}, per-channel DDC {single_ok}/{TRIALS} — the shared front end \
             must not lose more than one trial"
        );
    }
}

/// A strong burst 50 kHz away must not mask a ~20 dB weaker one on its own
/// channel, through every front end.
#[test]
fn multi_channel_rejects_adjacent_channel() {
    use xng_mode_vdl2::modulate::burst_iq_shaped;
    const FS: f64 = 2_400_000.0;
    const LEAD: usize = 24_000;
    const TAIL: usize = 1_200_000;
    // 136.775 and 136.825 MHz in the plan.
    const STRONG: usize = 2;
    const WEAK: usize = 3;
    let strong_frame = chan_frame(STRONG);
    let weak_frame = chan_frame(WEAK);
    let strong =
        burst_iq_shaped(std::slice::from_ref(&strong_frame), FS, PLAN_OFFSETS[STRONG], 0.5);
    let weak = burst_iq_shaped(std::slice::from_ref(&weak_frame), FS, PLAN_OFFSETS[WEAK], 0.05);
    // The weak burst starts a few ms into the strong one so they overlap for
    // most of their length.
    let weak_at = LEAD + 10_000;
    let mut iq = vec![Complex::new(0.0f32, 0.0); weak_at + strong.len().max(weak.len()) + TAIL];
    for (d, s) in iq[LEAD..LEAD + strong.len()].iter_mut().zip(&strong) {
        *d += s;
    }
    for (d, s) in iq[weak_at..weak_at + weak.len()].iter_mut().zip(&weak) {
        *d += s;
    }
    let mut noise = Noise(0x7f4a_7c15_9e37_79b9);
    for s in &mut iq {
        *s += Complex::new(noise.next() * 0.002, noise.next() * 0.002);
    }

    for (name, mut dec) in multi_fronts(FS, &PLAN_OFFSETS) {
        let got = run_multi(&mut dec, &iq);
        assert!(
            got.iter().any(|(c, raw)| *c == WEAK && is_frame(raw, &weak_frame)),
            "{name}: weak burst on 136.825 lost next to a 20 dB stronger one on 136.775 \
             (decoded {:?})",
            got.iter().map(|(c, raw)| (*c, raw.len())).collect::<Vec<_>>()
        );
        assert!(
            got.iter().any(|(c, raw)| *c == STRONG && is_frame(raw, &strong_frame)),
            "{name}: strong burst lost"
        );
    }
}
