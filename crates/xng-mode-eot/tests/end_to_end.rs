//! End-to-end EOT/HOT decode tests.
//!
//! VERIFICATION POSTURE (the project mandate):
//!
//! 1. FRAMING is verified against the SPEC-CITED documented field map shared
//!    by the two independent public EOT decoders (ereuter/PyEOT and
//!    russinnes/EOTDecode). `build_spec_packet` hand-assembles the exact 74
//!    bits of a frame *per that documented layout* — frame sync `11100010010`,
//!    the 45-bit data block sliced exactly as the decoders slice it, and the
//!    ciphered BCH(63,45) check computed the documented way (mod-2 division of
//!    the reversed data block, XOR cipher key). We then assert the decoder
//!    recovers every documented field AND that its independent BCH verify
//!    passes. This is spec-cited ground truth, NOT a self-modulator round-trip.
//!
//! 2. DEMOD is validated by a SYNTHETIC `modulate -> complex AWGN at a
//!    controlled SNR -> demod` frame-recovery measurement (`*_synth_iq`). No
//!    off-air IQ exists, so no real-RF claim is made.
//!
//! HONESTY: this link is reverse-engineered with no public formal AAR
//! standard. The 2 "chaining" bits (packet[11:13]) are inside the BCH-
//! protected block but neither cited decoder names their meaning, so we only
//! surface them raw. See src/frame.rs notes.

use xng_mode_eot::{bch, frame, modulate, scan_bits, to_message, EotChannelDecoder, CHANNEL_RATE};

// ---------------------------------------------------------------------------
// Spec-cited framing ground truth.
// ---------------------------------------------------------------------------

/// Write `value` into `bits[start..end]` LSB-first — the on-air orientation
/// the cited decoders read each multi-bit field in (they reverse the slice,
/// then `int(..., 2)`, which equals reading the original slice LSB-first).
fn set_field(bits: &mut [u8], start: usize, end: usize, value: u32) {
    for i in 0..(end - start) {
        bits[start + i] = ((value >> i) & 1) as u8;
    }
}

/// Hand-build a complete 74-bit EOT packet from chosen field values, laid out
/// EXACTLY per the documented PyEOT/EOTDecode field map, and append the
/// ciphered BCH check computed the documented way. The decoder must recover
/// the same fields and validate the check.
#[allow(clippy::too_many_arguments)]
fn build_spec_packet(
    chaining: u8,
    battery_condition: u8,
    message_type: u8,
    unit_addr: u32,
    pressure: u8,
    battery_charge_raw: u8,
    spare: u8,
    valve: u8,
    conf: u8,
    turbine: u8,
    motion: u8,
    mkr_batt: u8,
    mkr_status: u8,
) -> Vec<u8> {
    let mut p = vec![0u8; frame::PACKET_BITS];
    // packet[0:11] = frame sync 11100010010.
    p[0..11].copy_from_slice(&frame::FRAME_SYNC);
    // packet[11:13] = chaining (stored MSB-first as the decoder parses it).
    p[11] = (chaining >> 1) & 1;
    p[12] = chaining & 1;
    // Multi-bit fields, LSB-first on the wire (= reversed slice MSB-first).
    set_field(&mut p, 13, 15, battery_condition as u32);
    set_field(&mut p, 15, 18, message_type as u32);
    set_field(&mut p, 18, 35, unit_addr);
    set_field(&mut p, 35, 42, pressure as u32);
    set_field(&mut p, 42, 49, battery_charge_raw as u32);
    // Single status bits.
    p[49] = spare;
    p[50] = valve;
    p[51] = conf;
    p[52] = turbine;
    p[53] = motion;
    p[54] = mkr_batt;
    p[55] = mkr_status;
    // packet[56:74] = ciphered BCH check over the 45-bit data block, the
    // documented way (reverse data, mod-2 divide by generator, XOR cipher).
    let check = bch::ciphered_check(&p[frame::DATA_START..frame::DATA_END]);
    p[frame::DATA_END..frame::PACKET_BITS].copy_from_slice(&check);
    p
}

#[test]
fn decodes_spec_field_map_and_bch() {
    // A plausible EOT->HOT telemetry report:
    //   unit address 0x1A2B3 (17-bit), 75 psig brake pipe, motion = moving,
    //   marker light ON with OK battery, turbine charging, message type 000
    //   (a normal status report), battery condition OK (0b11).
    let unit = 0x1A2B3 & 0x1_FFFF;
    let packet = build_spec_packet(
        0b00,  // chaining
        0b11,  // battery condition = OK
        0b000, // message type
        unit,  // unit address
        75,    // brake pipe pressure (psig)
        96,    // battery charge raw (96/127 ~ 75.6% -> 76)
        0,     // spare
        1,     // valve circuit
        0,     // conf indicator
        1,     // turbine
        1,     // motion
        1,     // marker light battery
        1,     // marker light status
    );

    let f = frame::parse_packet(&packet).expect("spec packet parses");

    // The independently implemented BCH verify must accept the documented
    // check word.
    assert!(f.bch_ok, "BCH check must verify on a spec-built packet");

    // Every documented field must come back exactly.
    assert_eq!(f.chaining, 0b00);
    assert_eq!(f.battery_condition, 0b11);
    assert_eq!(f.battery_condition_text, "OK");
    assert_eq!(f.message_type, 0b000);
    assert_eq!(f.unit_addr, unit);
    assert_eq!(f.pressure_psi, 75);
    assert_eq!(f.battery_charge_pct, 76); // round(96/127*100)
    assert_eq!(f.spare, 0);
    assert_eq!(f.valve_circuit, 1);
    assert_eq!(f.turbine, 1);
    assert_eq!(f.motion, 1);
    assert_eq!(f.marker_light_batt, 1);
    assert_eq!(f.marker_light, 1);
    // arm_status only present for message type 0b111.
    assert_eq!(f.arm_status, None);
}

#[test]
fn battery_condition_table_matches_cited_decoders() {
    // The 2-bit battery-condition map is documented identically by both
    // decoders: 11 OK, 10 Low, 01 Very Low, 00 Not Monitored.
    for (code, text) in [
        (0b11u8, "OK"),
        (0b10, "Low"),
        (0b01, "Very Low"),
        (0b00, "Not Monitored"),
    ] {
        let p = build_spec_packet(0, code, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0);
        let f = frame::parse_packet(&p).unwrap();
        assert_eq!(f.battery_condition, code);
        assert_eq!(f.battery_condition_text, text, "code {code:02b}");
    }
}

#[test]
fn arm_status_for_status_message_type() {
    // Message type 0b111 is the status/arm message; conf indicator selects
    // Arming (0) vs Armed (1), per both cited decoders.
    let arming = build_spec_packet(0, 0b11, 0b111, 5, 0, 0, 0, 0, 0, 0, 0, 0, 0);
    let f = frame::parse_packet(&arming).unwrap();
    assert_eq!(f.message_type, 0b111);
    assert_eq!(f.arm_status.as_deref(), Some("Arming"));

    let armed = build_spec_packet(0, 0b11, 0b111, 5, 0, 0, 0, 0, 1, 0, 0, 0, 0);
    let f = frame::parse_packet(&armed).unwrap();
    assert_eq!(f.arm_status.as_deref(), Some("Armed"));
}

#[test]
fn bch_detects_corrupted_data_bit() {
    // Flip one data bit after building a valid spec packet: the documented
    // ciphered BCH check must now fail (the field still parses; only bch_ok
    // drops).
    let mut p = build_spec_packet(0, 0b11, 0, 0x1234, 60, 100, 0, 0, 0, 0, 0, 0, 0);
    assert!(frame::parse_packet(&p).unwrap().bch_ok);
    p[20] ^= 1; // corrupt a unit-address data bit
    let f = frame::parse_packet(&p).unwrap();
    assert!(!f.bch_ok, "BCH must detect a single-bit data corruption");
}

#[test]
fn scan_finds_packet_after_bit_sync_preamble() {
    // Build the on-air logical bit stream: alternating bit-sync clock run
    // (ending in the 101010 the hunt keys on) + frame sync (inside the
    // packet) + data + check. scan_bits must find and decode it.
    let packet = build_spec_packet(0, 0b11, 0, 0xABCD & 0x1FFFF, 82, 110, 0, 1, 0, 1, 1, 1, 1);

    // Alternating clock run ending in the `...101010` tail the hunt keys on
    // (last bit 0, immediately before the frame sync's leading 1).
    let mut bits: Vec<u8> = (0..20).map(|i| ((i + 1) % 2) as u8).collect();
    bits.extend_from_slice(&packet);

    let frames = scan_bits(&bits);
    assert_eq!(frames.len(), 1, "exactly one packet should be found");
    let f = &frames[0];
    assert!(f.bch_ok);
    assert_eq!(f.unit_addr, 0xABCD & 0x1FFFF);
    assert_eq!(f.pressure_psi, 82);
    assert_eq!(f.motion, 1);
}

// ---------------------------------------------------------------------------
// Synthetic IQ demod validation (modulate -> AWGN -> demod).
//
// These build the on-air Manchester-FSK waveform for a KNOWN spec-built EOT
// packet and run it through the real EotChannelDecoder (DDC + FSK
// discriminator + chip timing + Manchester decode + sync hunt + the verified
// framing core), asserting the recovered fields. The modulate->demod path is
// SELF-GENERATED (see modulate.rs): the field map / BCH stay anchored to the
// cited decoders by the framing tests above. Reported as SYNTHETIC.
// ---------------------------------------------------------------------------

use num_complex::Complex;

/// Deterministic Box-Muller complex AWGN, scaled so each I/Q component has
/// standard deviation `sigma`. A small LCG keeps the test reproducible without
/// pulling in an RNG crate.
struct Lcg(u64);
impl Lcg {
    fn next_f32(&mut self) -> f32 {
        // 48-bit LCG (numerical recipes constants), mapped to (0,1].
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        let x = (self.0 >> 16) as u32;
        ((x as f64 + 1.0) / (u32::MAX as f64 + 2.0)) as f32
    }
}

fn add_awgn(iq: &mut [Complex<f32>], sigma: f32, seed: u64) {
    let mut rng = Lcg(seed);
    let mut i = 0;
    while i < iq.len() {
        let u1 = rng.next_f32().max(1e-9);
        let u2 = rng.next_f32();
        let mag = sigma * (-2.0 * u1.ln()).sqrt();
        let n_re = mag * (std::f32::consts::TAU * u2).cos();
        let n_im = mag * (std::f32::consts::TAU * u2).sin();
        iq[i] += Complex::new(n_re, n_im);
        i += 1;
    }
}

#[test]
fn channel_decoder_recovers_clean_synth_iq() {
    let unit = 0x15555 & 0x1_FFFF;
    let packet = build_spec_packet(0, 0b11, 0, unit, 90, 120, 0, 1, 0, 1, 0, 1, 1);

    let iq = modulate::burst_iq(&packet, CHANNEL_RATE, 0.0, 0.8);
    let mut dec = EotChannelDecoder::new(CHANNEL_RATE, 0.0).expect("decoder");
    let frames = dec.process(&iq);

    assert!(
        !frames.is_empty(),
        "should recover an EOT frame from clean synth IQ"
    );
    let f = &frames[0].frame;
    assert!(
        f.bch_ok,
        "BCH should verify on cleanly demodulated frame: {f:?}"
    );
    assert_eq!(f.unit_addr, unit);
    assert_eq!(f.pressure_psi, 90);
    assert_eq!(f.marker_light, 1);
    assert!(dec.level_dbfs().is_finite());
}

#[test]
fn channel_decoder_recovers_with_carrier_offset_via_ddc_synth_iq() {
    // Off-center carrier in a wider capture: the DDC mixes it down and the
    // discriminator DC tracker absorbs the residual tuning error.
    let unit = 0x0_3C3C & 0x1_FFFF;
    let packet = build_spec_packet(0, 0b10, 0, unit, 64, 80, 0, 0, 0, 1, 1, 1, 0);

    let capture_rate = 96_000.0;
    let offset_hz = 12_000.0;
    let iq = modulate::burst_iq(&packet, capture_rate, offset_hz, 0.7);

    let mut dec = EotChannelDecoder::new(capture_rate, offset_hz).expect("decoder");
    let frames = dec.process(&iq);

    assert!(
        !frames.is_empty(),
        "should recover through the DDC at an offset"
    );
    let f = &frames[0].frame;
    assert!(f.bch_ok, "{f:?}");
    assert_eq!(f.unit_addr, unit);
    assert_eq!(f.pressure_psi, 64);
    assert_eq!(f.battery_condition_text, "Low");
}

#[test]
fn synthetic_awgn_ber_frame_recovery() {
    // SYNTHETIC demod metric: modulate a known packet, add complex AWGN at a
    // controlled SNR, demod, and require correct frame recovery (BCH-verified
    // and field-exact). The BCH check makes this a strict frame-recovery test:
    // a single residual bit error fails it. Run several seeds at a moderate
    // SNR and require a healthy success fraction.
    let unit = 0x1F0F0 & 0x1_FFFF;
    let packet = build_spec_packet(0, 0b11, 0, unit, 77, 100, 0, 1, 0, 1, 1, 1, 1);

    // Per-component noise sigma. Signal amplitude 0.8 -> per-sample power ~0.64
    // (|s|^2 for unit-magnitude * amp^2). sigma=0.18 per I/Q gives noise power
    // 2*sigma^2 ~ 0.065, i.e. ~10 dB SNR — a realistic moderate-SNR capture.
    let amp = 0.8f32;
    let sigma = 0.18f32;

    let trials = 12;
    let mut recovered = 0;
    for seed in 0..trials {
        let mut iq = modulate::burst_iq(&packet, CHANNEL_RATE, 0.0, amp);
        add_awgn(&mut iq, sigma, 0x1234_5678 + seed * 0x9E37);
        let mut dec = EotChannelDecoder::new(CHANNEL_RATE, 0.0).expect("decoder");
        let frames = dec.process(&iq);
        if let Some(df) = frames.iter().find(|d| d.frame.bch_ok) {
            if df.frame.unit_addr == unit && df.frame.pressure_psi == 77 {
                recovered += 1;
            }
        }
    }

    // At ~10 dB SNR the integrate-and-dump FSK + Manchester chain should
    // recover the BCH-clean frame on the large majority of trials.
    assert!(
        recovered as f64 / trials as f64 >= 0.75,
        "synthetic AWGN frame recovery too low: {recovered}/{trials}"
    );
}

#[test]
fn to_message_emits_eot_body_from_synth_iq() {
    use xng_types::{AppInfo, MessageBody, Mode, Provenance, StationIdentity};

    let unit = 0x0_BEEF & 0x1_FFFF;
    let packet = build_spec_packet(0, 0b11, 0, unit, 85, 115, 0, 1, 0, 1, 1, 1, 1);
    let iq = modulate::burst_iq(&packet, CHANNEL_RATE, 0.0, 0.9);

    let mut dec = EotChannelDecoder::new(CHANNEL_RATE, 0.0).expect("decoder");
    let frames = dec.process(&iq);
    assert!(!frames.is_empty());

    let msg = to_message(
        &frames[0],
        457_937_500, // EOT -> HOT telemetry channel
        dec.level_dbfs(),
        false, // is_hot = false -> kind "eot"
        Provenance {
            station: StationIdentity::new("TEST-EOT"),
            app: AppInfo::xng(),
            sdr: None,
            channel: None,
        },
    );

    assert_eq!(msg.mode, Mode::Eot);
    assert_eq!(msg.frequency_hz, 457_937_500);
    assert!(msg.decode.crc_ok, "BCH-verified frame should set crc_ok");
    assert!(msg.signal.rssi_db.is_some());
    assert!(msg.raw.is_some(), "packet bits should travel as raw");
    match &msg.body {
        MessageBody::Eot { kind, details } => {
            assert_eq!(kind, "eot");
            assert_eq!(details["unit_addr"], unit);
            assert_eq!(details["pressure_psi"], 85);
            assert_eq!(details["motion"], 1);
            assert_eq!(details["marker_light"], 1);
            assert_eq!(details["bch_ok"], true);
        }
        other => panic!("expected MessageBody::Eot, got {other:?}"),
    }
}
