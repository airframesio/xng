//! Synthetic PHY loopback: modulate → AWGN → demod → HDLC deframe → ASM
//! decode. There is NO published off-air VDES ASM IQ, so the demod is
//! validated ONLY by this genuine modulate→AWGN→demod chain (reported as
//! synthetic). The framing/payload layer is independently verified against
//! spec-cited bit vectors in `tests/asm_decode.rs`.

use num_complex::Complex;
use xng_mode_vdes::modulate::{burst_iq, burst_iq_gmsk, hdlc_bits, wire_bytes_from_message_bits};
use xng_mode_vdes::{asm, frame, VdesChannelDecoder, CHANNEL_RATE};

/// Deterministic xorshift noise generator (no external RNG dependency).
struct Noise(u64);
impl Noise {
    fn next(&mut self) -> f32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 as f32 / u64::MAX as f32) * 2.0 - 1.0
    }
}

/// Build a broadcast (Message 8) ASM bit string for a given DAC/FID with a
/// 13-bit persons-on-board count (DAC=1 FID=16, IMO SN.1/Circ.289).
fn pob_message(source_mmsi: u32, count: u64) -> Vec<u8> {
    let mut bits = Vec::new();
    let mut pack = |v: u64, w: usize| {
        for k in (0..w).rev() {
            bits.push(((v >> k) & 1) as u8);
        }
    };
    pack(8, 6); // message ID 8
    pack(0, 2); // repeat
    pack(source_mmsi as u64, 30); // source MMSI
    pack(0, 2); // spare
    pack(1, 10); // DAC 1
    pack(16, 6); // FID 16
    pack(count, 13); // persons on board
    while bits.len() % 8 != 0 {
        bits.push(0);
    }
    bits
}

#[test]
fn modulate_msk_awgn_demod_decodes_asm() {
    let msg = pob_message(211_000_001, 167);

    let mut iq = vec![Complex::new(0.0, 0.0); 300];
    iq.extend(burst_iq(&msg, CHANNEL_RATE, 0.0, 0.5));
    iq.extend(vec![Complex::new(0.0, 0.0); 300]);
    let mut noise = Noise(0x1234_5678_9abc_def0);
    for s in &mut iq {
        *s += Complex::new(noise.next() * 0.02, noise.next() * 0.02);
    }

    let mut dec = VdesChannelDecoder::new(CHANNEL_RATE, 0.0).unwrap();
    let mut frames = Vec::new();
    for chunk in iq.chunks(512) {
        frames.extend(dec.process(chunk));
    }
    assert_eq!(frames.len(), 1, "one ASM frame recovered");
    assert_eq!(frames[0].message_bits, msg);
    let a = asm::decode(&frames[0].message_bits).unwrap();
    assert_eq!(a.msg_id, 8);
    assert_eq!(a.source_mmsi, 211_000_001);
    assert_eq!(a.dac, 1);
    assert_eq!(a.fid, 16);
    assert_eq!(a.app["persons_on_board"], 167);
}

#[test]
fn modulate_gmsk_awgn_demod_decodes_asm() {
    // The realistic ITU-R M.2092-1 ASM waveform (Gaussian BT=0.5) must
    // decode through the discriminator demod in light AWGN.
    let msg = pob_message(244_000_000, 42);
    let mut iq = vec![Complex::new(0.0, 0.0); 300];
    iq.extend(burst_iq_gmsk(&msg, CHANNEL_RATE, 0.0, 0.5));
    iq.extend(vec![Complex::new(0.0, 0.0); 300]);
    let mut noise = Noise(0xfeed_face_dead_beef);
    for s in &mut iq {
        *s += Complex::new(noise.next() * 0.02, noise.next() * 0.02);
    }
    let mut dec = VdesChannelDecoder::new(CHANNEL_RATE, 0.0).unwrap();
    let mut frames = Vec::new();
    for chunk in iq.chunks(512) {
        frames.extend(dec.process(chunk));
    }
    assert_eq!(frames.len(), 1, "GMSK ASM burst decodes");
    let a = asm::decode(&frames[0].message_bits).unwrap();
    assert_eq!(a.source_mmsi, 244_000_000);
    assert_eq!(a.app["persons_on_board"], 42);
}

#[test]
fn wideband_capture_with_carrier_offset() {
    // One 2.4 MS/s capture; the ASM channel sits at +50 kHz with a
    // deliberate 600 Hz carrier offset (ship + receiver ppm error).
    let fs = 2_400_000.0;
    let msg = pob_message(366_000_005, 99);
    let burst = burst_iq_gmsk(&msg, fs, 50_000.0 + 600.0, 0.4);
    let mut iq = vec![Complex::new(0.0f32, 0.0f32); burst.len() + 20_000];
    for (i, s) in burst.iter().enumerate() {
        iq[i + 5_000] += s;
    }
    let mut noise = Noise(0x0bad_c0de_1337_d00d);
    for s in &mut iq {
        *s += Complex::new(noise.next() * 0.01, noise.next() * 0.01);
    }

    let mut dec = VdesChannelDecoder::new(fs, 50_000.0).unwrap();
    let mut frames = Vec::new();
    for chunk in iq.chunks(65_536) {
        frames.extend(dec.process(chunk));
    }
    assert_eq!(frames.len(), 1, "ASM decodes despite 600 Hz CFO");
    let a = asm::decode(&frames[0].message_bits).unwrap();
    assert_eq!(a.source_mmsi, 366_000_005);
    assert_eq!(a.app["persons_on_board"], 99);
}

/// Genuine modulate→AWGN→demod BER measurement (synthetic; reported, not
/// asserted to a fixed floor beyond "decodes cleanly at moderate SNR").
#[test]
fn synthetic_ber_at_moderate_snr() {
    // Run many independent bursts at a fixed noise level; require the
    // overwhelming majority to deframe AND decode correctly. This exercises
    // the timing/offset loops across different bit patterns, not one vector.
    let trials = 40usize;
    let mut decoded = 0usize;
    for t in 0..trials {
        let mmsi = 200_000_000 + (t as u32) * 137;
        let count = 1 + (t as u64 * 53) % 8000;
        let msg = pob_message(mmsi, count);
        let mut iq = vec![Complex::new(0.0, 0.0); 200];
        iq.extend(burst_iq_gmsk(&msg, CHANNEL_RATE, 0.0, 0.5));
        iq.extend(vec![Complex::new(0.0, 0.0); 200]);
        let mut noise = Noise(0xa5a5_0000_0000_0001u64.wrapping_add(t as u64 * 0x9E37_79B9));
        for s in &mut iq {
            *s += Complex::new(noise.next() * 0.03, noise.next() * 0.03);
        }
        let mut dec = VdesChannelDecoder::new(CHANNEL_RATE, 0.0).unwrap();
        let mut frames: Vec<frame::VdesFrame> = Vec::new();
        for chunk in iq.chunks(512) {
            frames.extend(dec.process(chunk));
        }
        if let Some(f) = frames.first() {
            if let Some(a) = asm::decode(&f.message_bits) {
                if a.source_mmsi == mmsi && a.app["persons_on_board"] == count {
                    decoded += 1;
                }
            }
        }
    }
    // Discriminator GMSK demod at this SNR is essentially error-free; allow
    // a small margin for timing edge cases.
    assert!(
        decoded >= trials - 2,
        "synthetic BER: {decoded}/{trials} bursts decoded correctly"
    );
}

/// Sanity: the FCS guards against corruption — a flipped wire bit drops the
/// frame rather than emitting a bogus ASM.
#[test]
fn corrupted_frame_is_rejected() {
    let msg = pob_message(211_000_001, 167);
    let mut wire = wire_bytes_from_message_bits(&msg);
    wire[3] ^= 0x10; // flip a payload bit
    let stream = hdlc_bits(&wire);
    let mut d = frame::HdlcDeframer::new();
    let frames: Vec<_> = stream.iter().filter_map(|&b| d.push_bit(b)).collect();
    assert!(frames.is_empty(), "corrupted frame fails FCS");
}
