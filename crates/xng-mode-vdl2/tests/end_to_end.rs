//! RF loopback: AVLC/AOA frames → D8PSK burst → decoder.

use num_complex::Complex;
use xng_mode_vdl2::avlc::{encode_address, AddressType};
use xng_mode_vdl2::modulate::burst_iq;
use xng_mode_vdl2::Vdl2ChannelDecoder;

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
