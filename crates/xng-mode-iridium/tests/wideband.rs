//! Wideband front end: multiple bursts at different offsets within a
//! 2 MHz capture must all be found, downmixed, and demodulated; and
//! gr-iridium's real test capture must decode through the wideband path
//! without being told where the burst is.

use num_complex::Complex;
use xng_mode_iridium::{decode_bits, frame, modulate, wideband::IridiumWideband};

fn push_field(bits: &mut Vec<u8>, v: u32, n: usize) {
    for k in (0..n).rev() {
        bits.push(((v >> k) & 1) as u8);
    }
}

fn ira_bits(sat: u32) -> Vec<u8> {
    let mut d = Vec::new();
    push_field(&mut d, sat, 7);
    push_field(&mut d, 5, 6);
    for v in [100i32, -200, 1500] {
        let sign = if v < 0 { 1 } else { 0 };
        let mag = if v < 0 { v + (1 << 11) } else { v } as u32;
        push_field(&mut d, sign, 1);
        push_field(&mut d, mag, 11);
    }
    push_field(&mut d, 9, 7);
    push_field(&mut d, 0, 1);
    push_field(&mut d, 0, 1);
    push_field(&mut d, 3, 5);
    d.extend(std::iter::repeat(1).take(42));
    let mut padded = d;
    while padded.len() % 21 != 0 {
        padded.push(0);
    }
    let mut nblk = padded.len() / 21;
    while (nblk - 3) % 2 != 0 {
        padded.extend(std::iter::repeat(0).take(21));
        nblk += 1;
    }
    let blocks: Vec<Vec<u8>> = padded
        .chunks_exact(21)
        .map(|x| frame::bch_encode(frame::RINGALERT_BCH_POLY, x))
        .collect();
    let mut bits: Vec<u8> = frame::ACCESS_DL.to_vec();
    bits.extend(frame::interleave3(&blocks[0], &blocks[1], &blocks[2]));
    for pair in blocks[3..].chunks_exact(2) {
        bits.extend(frame::interleave2(&pair[0], &pair[1]));
    }
    bits
}

#[test]
fn finds_bursts_across_the_band() {
    let fs = 2_000_000.0;
    let offsets = [-700_000.0f64, 123_000.0, 651_000.0];
    let sats = [11u32, 22, 33];
    let mut sig = vec![Complex::new(0.0f32, 0.0); (fs * 1.0) as usize];
    for (k, (&off, &sat)) in offsets.iter().zip(&sats).enumerate() {
        let bits = ira_bits(sat);
        // Modulate at channel rate then upsample by zero-stuffing is
        // wrong; modulate directly at fs with the offset.
        let burst = modulate::modulate(&bits, 64, fs, off, 0.4);
        let at = ((0.2 + 0.25 * k as f64) * fs) as usize;
        for (i, s) in burst.iter().enumerate() {
            sig[at + i] += s;
        }
    }
    let mut noise = 0xfeed_beef_cafe_1234u64;
    for s in &mut sig {
        noise ^= noise << 13;
        noise ^= noise >> 7;
        noise ^= noise << 17;
        let n1 = (noise as f32 / u64::MAX as f32) - 0.5;
        noise ^= noise << 13;
        noise ^= noise >> 7;
        noise ^= noise << 17;
        let n2 = (noise as f32 / u64::MAX as f32) - 0.5;
        *s += Complex::new(n1 * 0.01, n2 * 0.01);
    }

    let mut wb = IridiumWideband::new(fs).unwrap();
    let mut found = Vec::new();
    for chunk in sig.chunks(65_536) {
        for b in wb.process(chunk) {
            if let Some(f) = decode_bits(&b.bits) {
                found.push((b.offset_hz, f));
            }
        }
    }
    assert_eq!(found.len(), 3, "all three bursts decode (got {})", found.len());
    for (&off, &sat) in offsets.iter().zip(&sats) {
        let hit = found
            .iter()
            .find(|(o, _)| (o - off).abs() < 5_000.0)
            .unwrap_or_else(|| panic!("burst near {off} Hz"));
        assert_eq!(hit.1.details["sat"], sat);
    }
}

#[test]
fn wideband_decoder_emits_frames_with_offsets() {
    use xng_mode_iridium::IridiumWidebandDecoder;
    let fs = 2_000_000.0;
    let off = -450_000.0f64;
    let bits = ira_bits(77);
    let burst = modulate::modulate(&bits, 64, fs, off, 0.4);
    let mut sig = vec![Complex::new(0.0f32, 0.0); (0.2 * fs) as usize];
    sig.extend(burst);
    sig.extend(std::iter::repeat(Complex::new(0.0f32, 0.0)).take((0.2 * fs) as usize));
    let mut noise = 0x4242_4242_4242_4242u64;
    for s in &mut sig {
        noise ^= noise << 13;
        noise ^= noise >> 7;
        noise ^= noise << 17;
        let n1 = (noise as f32 / u64::MAX as f32) - 0.5;
        noise ^= noise << 13;
        noise ^= noise >> 7;
        noise ^= noise << 17;
        let n2 = (noise as f32 / u64::MAX as f32) - 0.5;
        *s += Complex::new(n1 * 0.01, n2 * 0.01);
    }
    let mut dec = IridiumWidebandDecoder::new(fs).unwrap();
    let mut frames = Vec::new();
    for chunk in sig.chunks(65_536) {
        frames.extend(dec.process(chunk));
    }
    let (o, f) = frames.first().expect("frame decoded");
    assert!((o - off).abs() < 5_000.0, "offset {o}");
    assert_eq!(f.kind, "ring-alert");
    assert_eq!(f.details["sat"], 77);
}

#[test]
fn decodes_gr_iridium_capture_via_wideband() {
    // The vendored channel-rate fixture proves the demod; this test
    // reuses the same burst re-upconverted into a 2 MHz band at an
    // arbitrary offset to prove the wideband hunt end-to-end with real
    // reference-modulator samples.
    let raw = std::fs::read(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/data/grtest_prbs15_250k.i16"
    ))
    .expect("fixture present");
    let chan: Vec<Complex<f32>> = raw
        .chunks_exact(4)
        .map(|b| {
            Complex::new(
                i16::from_le_bytes([b[0], b[1]]) as f32 / 32768.0,
                i16::from_le_bytes([b[2], b[3]]) as f32 / 32768.0,
            )
        })
        .collect();
    // Upsample ×8 (zero-order hold) and shift to +400 kHz.
    let fs = 2_000_000.0;
    let off = 400_000.0f64;
    let mut sig = vec![Complex::new(0.0f32, 0.0); (0.05 * fs) as usize];
    sig.extend(chan.iter().flat_map(|&s| std::iter::repeat(s).take(8)));
    sig.extend(std::iter::repeat(Complex::new(0.0f32, 0.0)).take((0.2 * fs) as usize));
    for (i, s) in sig.iter_mut().enumerate() {
        let ph = std::f64::consts::TAU * off * i as f64 / fs;
        *s *= Complex::from_polar(1.0, ph as f32);
    }

    let mut wb = IridiumWideband::new(fs).unwrap();
    let mut bursts = Vec::new();
    for chunk in sig.chunks(65_536) {
        bursts.extend(wb.process(chunk));
    }
    assert!(!bursts.is_empty(), "burst found");
    // The zero-order-hold upsampling leaves images at ±250 kHz
    // multiples; pick the burst nearest the fundamental (any image
    // carries the same bits anyway).
    let b = bursts
        .iter()
        .min_by(|a, b| {
            (a.offset_hz - off)
                .abs()
                .partial_cmp(&(b.offset_hz - off).abs())
                .unwrap()
        })
        .unwrap();
    assert!((b.offset_hz - off).abs() < 40_000.0, "offset {}", b.offset_hz);
    assert_eq!(&b.bits[..24], &frame::ACCESS_DL[..]);
    let payload = &b.bits[24..];
    let violations = (15..payload.len())
        .filter(|&i| payload[i] != (payload[i - 15] ^ payload[i - 14]))
        .count();
    assert_eq!(violations, 0, "PRBS15 must hold");
}
