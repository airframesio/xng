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
