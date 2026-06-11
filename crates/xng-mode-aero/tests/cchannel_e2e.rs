//! C-channel RF loopback: encoded frames → 8 400 bps OQPSK (RRC β=0.6)
//! → demod → deframer → AMBE voice frames + sub-band signal units.

use num_complex::Complex;
use xng_mode_aero::cchannel::{CChannelDeframer, CChannelEncoder, CChannelEvent, INFO_BITS};
use xng_mode_aero::oqpsk::{modulate_oqpsk_rate, OqpskDemod, CHANNEL_RATE_HR};

struct Noise(u64);
impl Noise {
    fn next(&mut self) -> f32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 as f32 / u64::MAX as f32) * 2.0 - 1.0
    }
}

fn test_frame_bits() -> Vec<u8> {
    let mut su10 = vec![0u8; 10];
    su10[0] = 0x30; // call-progress
    su10[1..4].copy_from_slice(&[0x12, 0x34, 0x56]); // AES
    su10[4] = 0x7E; // GES
    let su = xng_mode_aero::su::su_with_crc(su10);

    let mut bits = vec![0u8; INFO_BITS];
    // Voice slots: a recognizable byte pattern (LSB-first packing).
    for y in 0..25 {
        let offset = y * 109;
        for n in 0..96 {
            if offset + 1 + n >= INFO_BITS {
                break;
            }
            bits[offset + 1 + n] = (0xA7u8 >> (n % 8)) & 1;
        }
    }
    // SU bits into the first 8 sub-blocks' 12-bit data slots.
    let stream: Vec<u8> = su.iter().flat_map(|&b| (0..8).map(move |i| (b >> i) & 1)).collect();
    let mut it = stream.into_iter();
    for y in 0..8 {
        let offset = y * 109;
        for h in offset + 97..offset + 109 {
            bits[h] = it.next().unwrap();
        }
    }
    bits
}

#[test]
fn decodes_c_channel_voice_and_su_from_iq() {
    let info = test_frame_bits();
    let mut enc = CChannelEncoder::new();
    let mut bits = Vec::new();
    for _ in 0..4 {
        bits.extend(enc.encode(&info));
    }

    let mut iq = modulate_oqpsk_rate(&bits, 8_400.0, 0.6, CHANNEL_RATE_HR, 90.0, 0.5);
    let mut noise = Noise(0x0123_4567_89ab_cdef);
    for s in &mut iq {
        *s += Complex::new(noise.next() * 0.02, noise.next() * 0.02);
    }

    let mut demod = OqpskDemod::new_c_channel(CHANNEL_RATE_HR);
    let mut deframer = CChannelDeframer::new();
    let mut soft = Vec::new();
    let mut events = Vec::new();
    for chunk in iq.chunks(8192) {
        soft.clear();
        demod.process(chunk, &mut soft);
        for &(s, _) in &soft {
            events.extend(deframer.push(s));
        }
    }

    let voices: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            CChannelEvent::Voice(v) => Some(v),
            _ => None,
        })
        .collect();
    let sus: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            CChannelEvent::SignalUnit(s) => Some(s),
            _ => None,
        })
        .collect();

    assert!(!voices.is_empty(), "no AMBE voice frames decoded");
    assert_eq!(voices[0][0], 0xA7, "voice byte pattern");
    assert!(!sus.is_empty(), "no signal units decoded");
    assert_eq!(sus[0][0], 0x30, "call-progress type");
    assert_eq!(&sus[0][1..4], &[0x12, 0x34, 0x56], "AES id");
    assert_eq!(sus[0][4], 0x7E, "GES id");
}
