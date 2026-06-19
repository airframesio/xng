//! RF loopback: packets → frame → BPSK waveform → coherent decoder.

use num_complex::Complex;
use xng_mode_stdc::frame::encode_frame;
use xng_mode_stdc::modulate::modulate;
use xng_mode_stdc::packet::build_packet;
use xng_mode_stdc::StdcChannelDecoder;

struct Noise(u64);
impl Noise {
    fn next(&mut self) -> f32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 as f32 / u64::MAX as f32) * 2.0 - 1.0
    }
}

fn egc_packet(text: &[u8]) -> Vec<u8> {
    // 0xB0 EGC, service 0x31 (NAVAREA), no continuation, safety priority.
    let mut body = vec![0xB0, 0u8, 0x31, (1 << 5) | 1];
    body.extend(881u16.to_be_bytes());
    body.push(1); // pkt seq
    body.push(0); // IA5
    body.extend([0x12, 0x34, 0x56, 0x78]); // 4-byte address for 0x31
    body.extend(text);
    body[1] = body.len() as u8; // medium: len = byte[1] + 2 = final total
    build_packet(&body)
}

fn frame_payload(text: &[u8]) -> Vec<u8> {
    let mut payload = Vec::new();
    // Bulletin board (short descriptor 0x7D: total 14).
    payload.extend(build_packet(&[0x7D, 1, 0x03, 0xE8, 0, 0, 1, 0x10, 0, 0, 0, 0]));
    payload.extend(egc_packet(text));
    payload.resize(639, 0);
    payload
}

#[test]
fn decodes_egc_from_rf_with_cfo() {
    const TEXT: &[u8] = b"SECURITE. NAVAREA XII 123/26. PACIFIC. BUOY ADRIFT NEAR 38-00N 123-30W.";
    let symbols = encode_frame(&frame_payload(TEXT));
    // Real STD-C is a continuous carrier: give the loops settling time
    // before the frames of interest, as deployment does.
    let mut all: Vec<u8> = (0..4000).map(|i| (i % 2) as u8).collect();
    all.extend(&symbols);
    all.extend(&symbols);
    all.extend(&symbols);

    // Feed through the production path: 48 kHz capture → DDC → 12 kHz.
    let mut iq = modulate(&all, 1200.0, 48_000.0, 230.0, 0.5);
    let mut noise = Noise(0x5a5a_1234_8765_dcba);
    for s in &mut iq {
        *s += Complex::new(noise.next() * 0.02, noise.next() * 0.02);
    }

    let mut dec = StdcChannelDecoder::new(48_000.0, 0.0).unwrap();
    let mut events = Vec::new();
    for chunk in iq.chunks(8192) {
        events.extend(dec.process(chunk));
    }
    let egc = events.iter().find(|e| e.name == "egc-message").expect("EGC decodes");
    assert_eq!(egc.text.as_deref(), Some(std::str::from_utf8(TEXT).unwrap()));
    assert_eq!(egc.details["priority"], "safety");
    assert_eq!(egc.details["service"], "safetynet/navarea-warning");
    let bb = events.iter().find(|e| e.name == "bulletin-board").expect("BB decodes");
    assert_eq!(bb.details["frame_number"], 1000);
}

#[test]
fn decodes_from_wideband_capture() {
    const TEXT: &[u8] = b"MET WARNING TEST";
    let symbols = encode_frame(&frame_payload(TEXT));
    let mut all = symbols.clone();
    all.extend(&symbols);
    all.extend(&symbols);

    let fs = 2_400_000.0;
    let mut iq = modulate(&all, 1200.0, fs, 60_000.0 - 110.0, 0.4);
    let mut noise = Noise(0x1111_2222_3333_4444);
    for s in &mut iq {
        *s += Complex::new(noise.next() * 0.01, noise.next() * 0.01);
    }

    let mut dec = StdcChannelDecoder::new(fs, 60_000.0).unwrap();
    let mut events = Vec::new();
    for chunk in iq.chunks(65_536) {
        events.extend(dec.process(chunk));
    }
    let egc = events.iter().find(|e| e.name == "egc-message").expect("EGC decodes");
    assert_eq!(egc.text.as_deref(), Some("MET WARNING TEST"));
}
