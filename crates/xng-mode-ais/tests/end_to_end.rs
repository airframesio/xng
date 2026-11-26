//! RF loopback anchored to a widely published real-world AIVDM example, so
//! bit order, armoring, and checksum conventions are validated against
//! real data rather than self-consistency.

use num_complex::Complex;
use xng_mode_ais::modulate::burst_iq;
use xng_mode_ais::nmea::payload_to_bits;
use xng_mode_ais::AisChannelDecoder;

/// Canonical example sentence (gpsd AIVDM documentation): type 1 position
/// report, MMSI 477553000, channel B.
const KNOWN_PAYLOAD: &str = "177KQJ5000G?tO`K>RA1wUbN0TKH";
const KNOWN_SENTENCE: &str = "!AIVDM,1,1,,B,177KQJ5000G?tO`K>RA1wUbN0TKH,0*5C";

struct Noise(u64);
impl Noise {
    fn next(&mut self) -> f32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 as f32 / u64::MAX as f32) * 2.0 - 1.0
    }
}

#[test]
fn decodes_known_vector_at_channel_rate() {
    let msg_bits = payload_to_bits(KNOWN_PAYLOAD);
    assert_eq!(msg_bits.len(), 168);

    let mut iq = vec![Complex::new(0.0, 0.0); 300];
    iq.extend(burst_iq(&msg_bits, 48_000.0, 0.0, 0.5));
    iq.extend(vec![Complex::new(0.0, 0.0); 300]);

    let mut dec = AisChannelDecoder::new(48_000.0, 0.0, 162_025_000).unwrap();
    let mut found = Vec::new();
    for chunk in iq.chunks(512) {
        found.extend(dec.process(chunk));
    }
    assert_eq!(found.len(), 1, "expected one frame");
    let (frame, sentences) = &found[0];
    assert_eq!(frame.msg_type, 1);
    assert_eq!(frame.mmsi, 477_553_000);
    assert_eq!(sentences.len(), 1);
    assert_eq!(sentences[0], KNOWN_SENTENCE, "must reproduce the published sentence exactly");
}

#[test]
fn decodes_both_channels_from_wideband_capture_with_cfo() {
    // One 2.4 MS/s capture centered between AIS channels A and B
    // (162.000 MHz): A at −25 kHz, B at +25 kHz. Burst B carries a
    // deliberate 700 Hz carrier offset (ship + receiver ppm error) and
    // both sit in light noise.
    let fs = 2_400_000.0;
    let msg_a = payload_to_bits(KNOWN_PAYLOAD);
    let mut msg_b = msg_a.clone();
    // Different MMSI (flip bits inside the MMSI field).
    msg_b[10] ^= 1;
    msg_b[20] ^= 1;

    let burst_a = burst_iq(&msg_a, fs, -25_000.0, 0.4);
    let burst_b = burst_iq(&msg_b, fs, 25_000.0 + 700.0, 0.4);

    let b_delay = 40_000;
    let total = burst_a.len().max(burst_b.len() + b_delay) + 20_000;
    let mut iq = vec![Complex::new(0.0f32, 0.0f32); total];
    for (i, s) in burst_a.iter().enumerate() {
        iq[i + 5_000] += s;
    }
    for (i, s) in burst_b.iter().enumerate() {
        iq[i + b_delay] += s;
    }
    let mut noise = Noise(0xfeed_beef_cafe_f00d);
    for s in &mut iq {
        *s += Complex::new(noise.next() * 0.01, noise.next() * 0.01);
    }

    let mut dec_a = AisChannelDecoder::new(fs, -25_000.0, 161_975_000).unwrap();
    let mut dec_b = AisChannelDecoder::new(fs, 25_000.0, 162_025_000).unwrap();
    let mut found_a = Vec::new();
    let mut found_b = Vec::new();
    for chunk in iq.chunks(65_536) {
        found_a.extend(dec_a.process(chunk));
        found_b.extend(dec_b.process(chunk));
    }

    assert_eq!(found_a.len(), 1, "channel A should decode one frame");
    assert_eq!(found_b.len(), 1, "channel B should decode one frame (despite 700 Hz CFO)");
    assert_eq!(found_a[0].0.mmsi, 477_553_000);
    assert_ne!(found_b[0].0.mmsi, 477_553_000);
    assert!(found_a[0].1[0].contains(",A,"));
    assert!(found_b[0].1[0].contains(",B,"));
}

/// The GMSK-shaped (BT=0.4) modulator is the realistic waveform: the
/// existing discriminator demod must decode it cleanly.
#[test]
fn decodes_gmsk_shaped_burst() {
    use xng_mode_ais::modulate::burst_iq_gmsk;
    let msg_bits = payload_to_bits(KNOWN_PAYLOAD);
    let mut iq = vec![Complex::new(0.0, 0.0); 300];
    iq.extend(burst_iq_gmsk(&msg_bits, 48_000.0, 0.0, 0.5));
    iq.extend(vec![Complex::new(0.0, 0.0); 300]);
    let mut noise = Noise(0x5151_aaaa_3333_7777);
    for s in &mut iq {
        *s += Complex::new(noise.next() * 0.02, noise.next() * 0.02);
    }
    let mut dec = AisChannelDecoder::new(48_000.0, 0.0, 162_025_000).unwrap();
    let mut found = Vec::new();
    for chunk in iq.chunks(512) {
        found.extend(dec.process(chunk));
    }
    assert_eq!(found.len(), 1, "GMSK burst decodes");
    assert_eq!(found[0].0.mmsi, 477_553_000);
}

/// Long-range position report (message 27) from the pyais test vectors:
/// MMSI 206914217, 4.84° N 137.023° E, 57 kt, course 167°, not under command.
const LONG_RANGE_PAYLOAD: &str = "KC5E2b@U19PFdLbL";

/// AIS channels C (156.775 MHz) and D (156.825 MHz) carry message 27 for
/// satellite AIS. One 2.4 MS/s capture centered on 156.800 MHz holds both,
/// and the AIVDM sentences say `C` and `D`.
#[test]
fn decodes_long_range_channels_c_and_d() {
    let fs = 2_400_000.0;
    let msg_c = payload_to_bits(LONG_RANGE_PAYLOAD);
    assert_eq!(msg_c.len(), 96);
    let mut msg_d = msg_c.clone();
    msg_d[10] ^= 1; // a different MMSI
    let burst_c = burst_iq(&msg_c, fs, -25_000.0, 0.4);
    let burst_d = burst_iq(&msg_d, fs, 25_000.0 - 500.0, 0.4);
    let d_delay = 30_000;
    let mut iq = vec![Complex::new(0.0f32, 0.0f32); burst_c.len().max(burst_d.len() + d_delay) + 20_000];
    for (i, s) in burst_c.iter().enumerate() {
        iq[i + 5_000] += s;
    }
    for (i, s) in burst_d.iter().enumerate() {
        iq[i + d_delay] += s;
    }
    let mut noise = Noise(0x0c0d_0c0d_1234_5678);
    for s in &mut iq {
        *s += Complex::new(noise.next() * 0.01, noise.next() * 0.01);
    }

    let mut dec_c = AisChannelDecoder::new(fs, -25_000.0, 156_775_000).unwrap();
    let mut dec_d = AisChannelDecoder::new(fs, 25_000.0, 156_825_000).unwrap();
    let (mut found_c, mut found_d) = (Vec::new(), Vec::new());
    for chunk in iq.chunks(65_536) {
        found_c.extend(dec_c.process(chunk));
        found_d.extend(dec_d.process(chunk));
    }
    assert_eq!(found_c.len(), 1, "channel C decodes one frame");
    assert_eq!(found_d.len(), 1, "channel D decodes one frame");
    let (frame, sentences) = &found_c[0];
    assert_eq!(frame.msg_type, 27);
    assert_eq!(frame.mmsi, 206_914_217);
    assert!(sentences[0].starts_with("!AIVDM,1,1,,C,KC5E2b@U19PFdLbL,0*"), "{}", sentences[0]);
    assert_eq!(found_d[0].0.msg_type, 27);
    assert_ne!(found_d[0].0.mmsi, 206_914_217);
    assert!(found_d[0].1[0].contains(",D,"), "{}", found_d[0].1[0]);

    let details = xng_mode_ais::fields::decode(27, &frame.message_bits).unwrap();
    assert!((details["lat"].as_f64().unwrap() - 4.84).abs() < 0.01, "{details}");
    assert!((details["lon"].as_f64().unwrap() - 137.0233).abs() < 0.01, "{details}");
    assert_eq!(details["sog_kt"], 57);
    assert_eq!(details["cog_deg"], 167);
    assert_eq!(details["nav_status"], "not under command");
}

/// A 10 MS/s capture (Airspy R2) centered on 159.400 MHz spans all four AIS
/// channels at once; each decoder picks out its own burst.
#[test]
fn decodes_channels_a_to_d_from_one_wide_capture() {
    let fs = 10_000_000.0;
    let center = 159_400_000i64;
    let chans: [(u64, char); 4] =
        [(156_775_000, 'C'), (156_825_000, 'D'), (161_975_000, 'A'), (162_025_000, 'B')];
    let base = payload_to_bits(KNOWN_PAYLOAD);
    let long_range = payload_to_bits(LONG_RANGE_PAYLOAD);
    let mut bursts = Vec::new();
    for (k, (f, _)) in chans.iter().enumerate() {
        let mut bits = if k < 2 { long_range.clone() } else { base.clone() };
        bits[12] ^= (k % 2) as u8; // A≠B, C≠D
        let offset = (*f as i64 - center) as f64;
        bursts.push((bits.clone(), burst_iq(&bits, fs, offset, 0.3)));
    }
    let spacing = bursts.iter().map(|(_, b)| b.len()).max().unwrap() + 50_000;
    let mut iq = vec![Complex::new(0.0f32, 0.0f32); spacing * 4 + 50_000];
    for (k, (_, b)) in bursts.iter().enumerate() {
        for (i, s) in b.iter().enumerate() {
            iq[i + 20_000 + k * spacing] += s;
        }
    }
    let mut noise = Noise(0xabcd_0123_4567_89ef);
    for s in &mut iq {
        *s += Complex::new(noise.next() * 0.01, noise.next() * 0.01);
    }
    for (k, (f, letter)) in chans.iter().enumerate() {
        let mut dec = AisChannelDecoder::new(fs, (*f as i64 - center) as f64, *f).unwrap();
        let mut found = Vec::new();
        for chunk in iq.chunks(262_144) {
            found.extend(dec.process(chunk));
        }
        assert_eq!(found.len(), 1, "channel {letter} decodes only its own burst");
        assert_eq!(found[0].0.message_bits, bursts[k].0, "channel {letter} frame");
        assert!(found[0].1[0].contains(&format!(",{letter},")), "{}", found[0].1[0]);
    }
}
