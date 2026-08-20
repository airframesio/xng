//! RF loopback: modulator → (noise, offsets, multiple channels) → decoder.

use num_complex::Complex;
use xng_mode_acars::modulate::{burst_iq, FrameSpec};
use xng_mode_acars::{AcarsChannelDecoder, AcarsMultiChannelDecoder};
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
    let msg = xng_mode_acars::to_message(&frames[0], 131_550_000, -20.0, -55.0, source);
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
    let msg = xng_mode_acars::to_message(&frames[0], 131_550_000, -20.0, -55.0, source);
    let MessageBody::Acars(core) = &msg.body else { panic!("not acars") };
    let app = core.app.as_ref().expect("position should populate app JSON");
    let lat = app["position"]["latitude"].as_f64().unwrap();
    let lon = app["position"]["longitude"].as_f64().unwrap();
    assert!((lat - 38.160).abs() < 1e-3, "lat {lat}");
    assert!((lon + 77.075).abs() < 1e-3, "lon {lon}");
}

#[test]
fn h2_sublabel_and_mfi_surface_in_message_body() {
    // ACARS-3.2: a non-H1 sublabel-bearing label (H2) carries the same
    // libacars `#xxB/yy ` downlink grammar. The decoded sublabel/MFI must
    // surface on AcarsCore the way H1 already does. Grammar oracle: libacars
    // 2.2.1 `la_acars_extract_sublabel_and_mfi`.
    let spec = FrameSpec {
        mode: '2',
        tail: "N471XG",
        ack: None,
        label: "H2",
        block_id: '3',
        msg_num: Some("M01A"),
        flight: Some("XG0042"),
        text: "#DFB/M1 ENGINE DATA",
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
    let msg = xng_mode_acars::to_message(&frames[0], 131_550_000, -20.0, -55.0, source);
    let MessageBody::Acars(core) = &msg.body else { panic!("not acars") };
    assert_eq!(core.label, "H2");
    assert_eq!(core.sublabel.as_deref(), Some("DF"), "H2 sublabel must surface");
    assert_eq!(core.mfi.as_deref(), Some("M1"), "H2 MFI must surface");
}

#[test]
fn single_bit_error_recovered_through_rf_path() {
    // ACARS-4.2: a single bit error injected at RF in a real-shape block is
    // recovered by the O(1) syndrome lookup (acarsdec syndrom.h scheme) over
    // the full demod→deframe→FEC path, restoring the exact original text and
    // reporting exactly one corrected bit.
    use xng_mode_acars::modulate::{burst_bits, modulate_audio, modulate_iq};

    let spec = downlink("ENROUTE WX REQUEST", "XG0042");

    // Build the on-air bit stream and flip ONE bit inside the frame body.
    // 128 pre-key bits + 5 sync octets (40 bits) precede the frame; pick a bit
    // well inside the payload so it lands on a text character.
    let mut bits = burst_bits(&spec);
    let flip_idx = 128 + 40 + 8 * 18; // ~18 octets into the frame (text region)
    bits[flip_idx] ^= 1;

    let audio = modulate_audio(&bits, 24_000.0);
    let burst = modulate_iq(&audio, 24_000.0, 0.0, 0.5, 0.85);

    let mut iq = vec![Complex::new(0.0, 0.0); 500];
    iq.extend(burst);
    iq.extend(vec![Complex::new(0.0, 0.0); 500]);

    let mut dec = AcarsChannelDecoder::new(24_000.0, 0.0).unwrap();
    let mut frames = Vec::new();
    for chunk in iq.chunks(1024) {
        frames.extend(dec.process(chunk));
    }
    assert_eq!(frames.len(), 1, "expected one frame");
    let f = &frames[0];
    assert!(f.crc_ok, "single-bit RF error should be repaired: {f:?}");
    assert_eq!(f.fixed_bits, 1, "exactly one bit corrected by syndrome FEC");
    assert_eq!(f.parity_errors, 0);
    assert_eq!(f.text, "ENROUTE WX REQUEST", "original text must be recovered");
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

#[test]
fn shared_front_end_decodes_many_channels() {
    // The multi-channel decoder (one shared SharedDdc front end) must decode
    // the same bursts the per-channel decoders do — the CPU optimization is
    // output-equivalent. Three simultaneous channels at different offsets,
    // one 2.4 MS/s capture.
    let fs = 2_400_000.0;
    let offsets = [50_000.0, -75_000.0, 150_000.0];
    let specs = [
        downlink("CHANNEL ONE PAYLOAD", "XG0001"),
        downlink("CHANNEL TWO PAYLOAD", "XG0002"),
        downlink("CHANNEL TRE PAYLOAD", "XG0003"),
    ];
    let bursts: Vec<Vec<Complex<f32>>> = specs
        .iter()
        .zip(offsets.iter())
        .map(|(s, &off)| burst_iq(s, fs, off, 0.4))
        .collect();

    let delays = [0usize, 30_000, 60_000];
    let total = bursts
        .iter()
        .zip(delays.iter())
        .map(|(b, &d)| b.len() + d)
        .max()
        .unwrap()
        + 10_000;
    let mut iq = vec![Complex::new(0.0f32, 0.0f32); total];
    for (b, &d) in bursts.iter().zip(delays.iter()) {
        for (i, s) in b.iter().enumerate() {
            iq[i + d] += s;
        }
    }
    let mut noise = Noise(0x0bad_f00d_dead_beef);
    for s in &mut iq {
        *s += Complex::new(noise.next() * 0.01, noise.next() * 0.01);
    }

    let mut dec = AcarsMultiChannelDecoder::new(fs, &offsets).unwrap();
    assert_eq!(dec.num_channels(), 3);
    // Collect frames per channel index across streamed blocks.
    let mut got: Vec<Vec<String>> = vec![Vec::new(); offsets.len()];
    for chunk in iq.chunks(65_536) {
        for (i, frames) in dec.process(chunk) {
            for f in frames {
                assert!(f.crc_ok, "channel {i} frame CRC failed: {f:?}");
                got[i].push(f.text.clone());
            }
        }
    }

    assert_eq!(got[0], ["CHANNEL ONE PAYLOAD"], "channel 0");
    assert_eq!(got[1], ["CHANNEL TWO PAYLOAD"], "channel 1");
    assert_eq!(got[2], ["CHANNEL TRE PAYLOAD"], "channel 2");
}

#[test]
fn both_front_ends_decode_equivalently() {
    // The channelizer (default) and the shared-decimation fallback must both
    // decode the same raster channels — the front end is a CPU choice, not a
    // correctness one. Offsets are on the 25 kHz airband raster.
    let fs = 2_400_000.0;
    let offsets = [50_000.0, -75_000.0, 150_000.0];
    let specs = [
        downlink("FRONT END ONE", "XG0001"),
        downlink("FRONT END TWO", "XG0002"),
        downlink("FRONT END TRE", "XG0003"),
    ];
    let bursts: Vec<Vec<Complex<f32>>> = specs
        .iter()
        .zip(offsets.iter())
        .map(|(s, &off)| burst_iq(s, fs, off, 0.4))
        .collect();
    let delays = [0usize, 30_000, 60_000];
    let total =
        bursts.iter().zip(delays.iter()).map(|(b, &d)| b.len() + d).max().unwrap() + 10_000;
    let mut iq = vec![Complex::new(0.0f32, 0.0f32); total];
    for (b, &d) in bursts.iter().zip(delays.iter()) {
        for (i, s) in b.iter().enumerate() {
            iq[i + d] += s;
        }
    }
    let mut noise = Noise(0xfeed_face_cafe_d00d);
    for s in &mut iq {
        *s += Complex::new(noise.next() * 0.01, noise.next() * 0.01);
    }

    let decode = |mut dec: AcarsMultiChannelDecoder| -> Vec<Vec<String>> {
        let mut got: Vec<Vec<String>> = vec![Vec::new(); offsets.len()];
        for chunk in iq.chunks(65_536) {
            for (i, frames) in dec.process(chunk) {
                for f in frames {
                    assert!(f.crc_ok);
                    got[i].push(f.text.clone());
                }
            }
        }
        got
    };

    let channelized = decode(AcarsMultiChannelDecoder::new(fs, &offsets).unwrap());
    let shared = decode(AcarsMultiChannelDecoder::new_shared(fs, &offsets).unwrap());
    assert_eq!(channelized, shared, "front ends must decode identically");
    assert_eq!(channelized[0], ["FRONT END ONE"]);
    assert_eq!(channelized[1], ["FRONT END TWO"]);
    assert_eq!(channelized[2], ["FRONT END TRE"]);
}

/// A burst arriving after a long idle period must decode identically to one
/// arriving immediately — the squelch's whole job is to be invisible.
///
/// This is the case the gate is most likely to break: the channel has been
/// shut for seconds, the noise floor has fully settled, and the gate has to
/// open on the pre-key and hand the demod enough lead-in that the timing loop
/// locks before the frame content arrives.
#[test]
fn squelch_decodes_a_burst_after_long_silence() {
    let spec = downlink("BURST AFTER LONG SILENCE", "XG0042");
    // 5 seconds of silence at the channel rate, then the burst.
    let mut iq = vec![Complex::new(0.0, 0.0); 24_000 * 5];
    iq.extend(burst_iq(&spec, 24_000.0, 0.0, 0.5));
    iq.extend(vec![Complex::new(0.0, 0.0); 24_000]);
    let mut noise = Noise(0x5115_ce00_1234_5678);
    for s in &mut iq {
        *s += Complex::new(noise.next() * 0.01, noise.next() * 0.01);
    }

    let mut dec = AcarsChannelDecoder::new(24_000.0, 0.0).unwrap();
    let mut frames = Vec::new();
    for chunk in iq.chunks(1024) {
        frames.extend(dec.process(chunk));
    }
    assert_eq!(frames.len(), 1, "expected exactly one frame after long silence");
    assert!(frames[0].crc_ok, "CRC failed: {:?}", frames[0]);
    assert_eq!(frames[0].text, "BURST AFTER LONG SILENCE");
}

/// Bursts arriving back to back, separated by less than the squelch's
/// hangover. The gate stays open across the gap, so the demod sees a
/// continuous stream — every burst must still decode, and none may be
/// duplicated by a pre-roll replaying samples the demod already consumed.
#[test]
fn squelch_decodes_back_to_back_bursts() {
    let texts = ["BACK TO BACK ONE", "BACK TO BACK TWO", "BACK TO BACK TRE"];
    let mut iq = vec![Complex::new(0.0, 0.0); 24_000];
    for t in &texts {
        iq.extend(burst_iq(&downlink(t, "XG0042"), 24_000.0, 0.0, 0.5));
        // 40 ms gap — shorter than the 100 ms hangover, so the gate never
        // closes between them.
        iq.extend(vec![Complex::new(0.0, 0.0); 960]);
    }
    iq.extend(vec![Complex::new(0.0, 0.0); 24_000]);
    let mut noise = Noise(0xb2b0_0000_dead_beef);
    for s in &mut iq {
        *s += Complex::new(noise.next() * 0.01, noise.next() * 0.01);
    }

    let mut dec = AcarsChannelDecoder::new(24_000.0, 0.0).unwrap();
    let mut got = Vec::new();
    for chunk in iq.chunks(1024) {
        for f in dec.process(chunk) {
            assert!(f.crc_ok, "CRC failed: {f:?}");
            got.push(f.text.clone());
        }
    }
    assert_eq!(got, texts, "every back-to-back burst must decode exactly once");
}

/// Widely-spaced bursts: the gate closes fully between them and must re-open
/// for each. Catches a gate that latches shut, or a noise floor that creeps up
/// after the first burst and swallows later ones.
#[test]
fn squelch_reopens_for_bursts_separated_by_long_gaps() {
    let texts = ["SPACED BURST ONE", "SPACED BURST TWO", "SPACED BURST TRE"];
    let mut iq = vec![Complex::new(0.0, 0.0); 24_000];
    for t in &texts {
        iq.extend(burst_iq(&downlink(t, "XG0042"), 24_000.0, 0.0, 0.5));
        iq.extend(vec![Complex::new(0.0, 0.0); 24_000 * 2]); // 2 s idle
    }
    let mut noise = Noise(0x5eed_1234_abcd_0001);
    for s in &mut iq {
        *s += Complex::new(noise.next() * 0.01, noise.next() * 0.01);
    }

    let mut dec = AcarsChannelDecoder::new(24_000.0, 0.0).unwrap();
    let mut got = Vec::new();
    for chunk in iq.chunks(1024) {
        for f in dec.process(chunk) {
            assert!(f.crc_ok, "CRC failed: {f:?}");
            got.push(f.text.clone());
        }
    }
    assert_eq!(got, texts, "gate must re-open for every burst");
}

/// The squelch is a CPU optimisation, not a decode decision: gating on and
/// gating off must produce the same frames from the same capture.
///
/// Deliberately at a low amplitude (0.12 against 0.01 noise) so the bursts sit
/// near the gate's threshold rather than slamming it open — a gate that only
/// works on loud signals would pass a test run at full scale.
#[test]
fn squelch_does_not_change_what_decodes() {
    let fs = 2_400_000.0;
    let offsets = [50_000.0, -75_000.0, 150_000.0];
    let specs = [
        downlink("EQUIVALENCE ONE", "XG0001"),
        downlink("EQUIVALENCE TWO", "XG0002"),
        downlink("EQUIVALENCE TRE", "XG0003"),
    ];
    let bursts: Vec<Vec<Complex<f32>>> =
        specs.iter().zip(offsets.iter()).map(|(s, &off)| burst_iq(s, fs, off, 0.12)).collect();
    let delays = [240_000usize, 900_000, 1_500_000];
    let total = bursts.iter().zip(delays.iter()).map(|(b, &d)| b.len() + d).max().unwrap()
        + 240_000;
    let mut iq = vec![Complex::new(0.0f32, 0.0f32); total];
    for (b, &d) in bursts.iter().zip(delays.iter()) {
        for (i, s) in b.iter().enumerate() {
            iq[i + d] += s;
        }
    }
    let mut noise = Noise(0xe0e0_5555_aaaa_1111);
    for s in &mut iq {
        *s += Complex::new(noise.next() * 0.01, noise.next() * 0.01);
    }

    let decode = |hold: bool| -> Vec<Vec<String>> {
        let mut dec = AcarsMultiChannelDecoder::new(fs, &offsets).unwrap();
        dec.hold_squelch_open(hold);
        let mut got: Vec<Vec<String>> = vec![Vec::new(); offsets.len()];
        for chunk in iq.chunks(65_536) {
            for (i, frames) in dec.process(chunk) {
                for f in frames {
                    assert!(f.crc_ok, "channel {i} CRC failed: {f:?}");
                    got[i].push(f.text.clone());
                }
            }
        }
        got
    };

    let gated = decode(false);
    let ungated = decode(true);
    assert_eq!(gated, ungated, "squelch changed the decoded frame set");
    assert_eq!(gated[0], ["EQUIVALENCE ONE"]);
    assert_eq!(gated[1], ["EQUIVALENCE TWO"]);
    assert_eq!(gated[2], ["EQUIVALENCE TRE"]);
}
