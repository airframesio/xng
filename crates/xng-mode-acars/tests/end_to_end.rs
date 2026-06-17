//! RF loopback: modulator → (noise, offsets, multiple channels) → decoder.

use num_complex::Complex;
use xng_mode_acars::modulate::{burst_iq, FrameSpec};
use xng_mode_acars::AcarsChannelDecoder;
use xng_types::{MessageBody, Provenance};

fn downlink<'a>(text: &'a str, flight: &'a str) -> FrameSpec<'a> {
    FrameSpec {
        mode: '2',
        tail: "N471XG",
        ack: None,
        label: "H1",
        block_id: '3',
        msg_num: Some("M42A"),
        flight: Some(flight),
        text,
        etb: false,
    }
}

/// Tiny xorshift noise source (no rand dependency; deterministic tests).
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
fn decodes_at_channel_rate() {
    let spec = downlink("POSN 4737.2N 12218.1W", "XG0042");
    let mut iq = vec![Complex::new(0.0, 0.0); 500];
    iq.extend(burst_iq(&spec, 24_000.0, 0.0, 0.5));
    iq.extend(vec![Complex::new(0.0, 0.0); 500]);

    let mut dec = AcarsChannelDecoder::new(24_000.0, 0.0).unwrap();
    let mut frames = Vec::new();
    for chunk in iq.chunks(1024) {
        frames.extend(dec.process(chunk));
    }
    assert_eq!(frames.len(), 1, "expected one frame");
    let f = &frames[0];
    assert!(f.crc_ok, "CRC failed: {f:?}");
    assert_eq!(f.parity_errors, 0);
    assert_eq!(f.tail.as_deref(), Some("N471XG"));
    assert_eq!(f.label, "H1");
    assert_eq!(f.flight.as_deref(), Some("XG0042"));
    assert_eq!(f.msg_num.as_deref(), Some("M42A"));
    assert_eq!(f.text, "POSN 4737.2N 12218.1W");
}

#[test]
fn oooi_fields_surface_in_message_body() {
    // ACARS-2.1: a real documented QQ "OFF Report" (research/QQ.md:
    // origin KEWR, dest KSWF) flows through the full RF path and the OOOI
    // fields appear in the message body's `app` JSON (acarsdec field names).
    let spec = FrameSpec {
        mode: '2',
        tail: "N471XG",
        ack: None,
        label: "QQ",
        block_id: '4',
        msg_num: Some("M01A"),
        flight: Some("XG0042"),
        text: "KEWRKSWF20041942",
        etb: false,
    };
    let mut iq = vec![Complex::new(0.0, 0.0); 500];
    iq.extend(burst_iq(&spec, 24_000.0, 0.0, 0.5));
    iq.extend(vec![Complex::new(0.0, 0.0); 500]);

    let mut dec = AcarsChannelDecoder::new(24_000.0, 0.0).unwrap();
    let mut frames = Vec::new();
    for chunk in iq.chunks(1024) {
        frames.extend(dec.process(chunk));
    }
    assert_eq!(frames.len(), 1, "expected one frame");
    assert!(frames[0].crc_ok);

    let source = Provenance {
        station: xng_types::StationIdentity::new("XX-TEST-ACARS"),
        app: xng_types::AppInfo::xng(),
        sdr: None,
        channel: None,
    };
    let msg = xng_mode_acars::to_message(&frames[0], 131_550_000, -20.0, source);
    let MessageBody::Acars(core) = &msg.body else { panic!("not acars") };
    let app = core.app.as_ref().expect("OOOI should populate app JSON");
    assert_eq!(app["depa"], "KEWR");
    assert_eq!(app["dsta"], "KSWF");
    assert_eq!(app["wloff"], "2004");
}

#[test]
fn free_text_position_surfaces_in_message_body() {
    // ACARS-2.2: a real documented label-20 POS report (Label_20_POS
    // test data: 38.160 / -77.075) flows through the RF path and the
    // lat/lon appear in the message body's `app` JSON.
    let spec = FrameSpec {
        mode: '2',
        tail: "N471XG",
        ack: None,
        label: "20",
        block_id: '3',
        msg_num: Some("M01A"),
        flight: Some("XG0042"),
        text: "POSN38160W077075,,211733,360,OTT,212041,,N42,19689,40,544",
        etb: false,
    };
    let mut iq = vec![Complex::new(0.0, 0.0); 500];
    iq.extend(burst_iq(&spec, 24_000.0, 0.0, 0.5));
    iq.extend(vec![Complex::new(0.0, 0.0); 500]);

    let mut dec = AcarsChannelDecoder::new(24_000.0, 0.0).unwrap();
    let mut frames = Vec::new();
    for chunk in iq.chunks(1024) {
        frames.extend(dec.process(chunk));
    }
    assert_eq!(frames.len(), 1);
    assert!(frames[0].crc_ok);

    let source = Provenance {
        station: xng_types::StationIdentity::new("XX-TEST-ACARS"),
        app: xng_types::AppInfo::xng(),
        sdr: None,
        channel: None,
    };
    let msg = xng_mode_acars::to_message(&frames[0], 131_550_000, -20.0, source);
    let MessageBody::Acars(core) = &msg.body else { panic!("not acars") };
    let app = core.app.as_ref().expect("position should populate app JSON");
    let lat = app["position"]["latitude"].as_f64().unwrap();
    let lon = app["position"]["longitude"].as_f64().unwrap();
    assert!((lat - 38.160).abs() < 1e-3, "lat {lat}");
    assert!((lon + 77.075).abs() < 1e-3, "lon {lon}");
}

#[test]
fn decodes_two_channels_from_wideband_capture() {
    // Two simultaneous ACARS bursts on different channels of one 2.4 MS/s
    // capture (the acarsdec-replacement scenario).
    let fs = 2_400_000.0;
    let spec_a = downlink("CHANNEL A PAYLOAD", "XG0001");
    let spec_b = downlink("CHANNEL B PAYLOAD", "XG0002");
    let burst_a = burst_iq(&spec_a, fs, 50_000.0, 0.4);
    let burst_b = burst_iq(&spec_b, fs, -75_000.0, 0.4);

    // Offset burst B by 12.5 ms; add light noise.
    let b_delay = 30_000;
    let total = (burst_a.len()).max(burst_b.len() + b_delay) + 10_000;
    let mut iq = vec![Complex::new(0.0f32, 0.0f32); total];
    for (i, s) in burst_a.iter().enumerate() {
        iq[i] += s;
    }
    for (i, s) in burst_b.iter().enumerate() {
        iq[i + b_delay] += s;
    }
    let mut noise = Noise(0x1234_5678_9abc_def0);
    for s in &mut iq {
        *s += Complex::new(noise.next() * 0.01, noise.next() * 0.01);
    }

    let mut dec_a = AcarsChannelDecoder::new(fs, 50_000.0).unwrap();
    let mut dec_b = AcarsChannelDecoder::new(fs, -75_000.0).unwrap();
    let mut frames_a = Vec::new();
    let mut frames_b = Vec::new();
    for chunk in iq.chunks(65_536) {
        frames_a.extend(dec_a.process(chunk));
        frames_b.extend(dec_b.process(chunk));
    }

    assert_eq!(frames_a.len(), 1, "channel A should decode exactly one frame");
    assert_eq!(frames_b.len(), 1, "channel B should decode exactly one frame");
    assert!(frames_a[0].crc_ok && frames_b[0].crc_ok);
    assert_eq!(frames_a[0].text, "CHANNEL A PAYLOAD");
    assert_eq!(frames_b[0].text, "CHANNEL B PAYLOAD");
    assert_eq!(frames_a[0].flight.as_deref(), Some("XG0001"));
    assert_eq!(frames_b[0].flight.as_deref(), Some("XG0002"));
}
