//! End-to-end GFSK demod validation on SELF-GENERATED (synthetic) IQ.
//!
//! There is no captured RS41 IQ vendored in this crate — only the published
//! byte-level oracle frames from rs1729/RS (`rs41.txt`), exercised by
//! `tests/frame_decode.rs`. To validate the IQ → bits → bytes demodulator
//! front-end ([`xng_mode_sonde::SondeChannelDecoder`]) end to end, this test
//! takes a *known oracle frame*, reconstructs its on-air whitened byte
//! stream, GFSK-modulates it (4800 bd NRZ, BT ≈ 0.5) into IQ with the crate's
//! own modulator, runs that IQ through the channel decoder, and asserts the
//! recovered frame's decoded fields equal the published oracle values.
//!
//! The modulate → demod path is therefore self-generated; the DECODE core
//! (whitening / RS / sub-block parse) remains oracle-anchored by
//! `frame_decode.rs`. Tests here are named `*_synth_iq`. See PROVENANCE.md.

use xng_mode_sonde::{
    modulate, to_message, whitening, SondeChannelDecoder, CHANNEL_RATE,
};

/// rs41.txt example (1): clean 320-byte standard frame, sonde K1930293
/// (de-whitened form, header `86 35 F4 40 …`). Same vector as
/// `frame_decode.rs`.
const FRAME1_DEWHITENED_HEX: &str = "8635f44093df1a602c87e0fa0521e8943d9cef4c7a67393f6d39fb546461f2111b6447ab79a746c80350cda5344157f8c0c12234f46902220f792816174b313933303239331a00000300000a00002f0007322ce53e31991abf12dada3eb68468c16755d51c7a2a15310216060245f302000d08a31607821e08bb210219060243f302000000000000000000000000000000220d7c1e0807d03cdc071fd81ddb19d70a8d0eb602b60cb518d40692ff00ff00ff001c277d59b8d83301ff0f881f0f38f4fe18b283038735ff000000003eb8ff4947201e6e3aff55415f13fc6e005440440cf100009e9f7406f85800832b631719d70010bebc172a8b00000000000000000000000000000000000000000000a48b7b15366181193ef05d07e1245b1be0f721f801f60804107b0b76110000000000000000000000000000000000ecc7";

fn hex(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
        .collect()
}

/// Reconstruct the on-air *whitened* frame from the de-whitened oracle frame:
/// XOR the whole frame with the whitening mask at phase 0. (The header is
/// whitened at phase 0 and the body at its natural phase, which is exactly
/// the mask-phase-0 application across the contiguous frame.)
fn on_air_whitened(dewhitened: &[u8]) -> Vec<u8> {
    let mut buf = dewhitened.to_vec();
    whitening::xor_mask(&mut buf, 0);
    buf
}

#[test]
fn channel_decoder_recovers_oracle_frame_synth_iq() {
    let dewhitened = hex(FRAME1_DEWHITENED_HEX);
    let on_air = on_air_whitened(&dewhitened);
    // Sanity: the reconstructed on-air header is the published whitened sync.
    assert_eq!(
        &on_air[..8],
        &[0x10, 0xB6, 0xCA, 0x11, 0x22, 0x96, 0x12, 0xF8]
    );

    // Modulate the on-air frame to GFSK IQ at the channel rate (offset 0 so
    // the decoder runs at channel rate directly, no DDC).
    let iq = modulate::burst_iq(&on_air, CHANNEL_RATE, 0.0, 1.0);

    let mut dec = SondeChannelDecoder::new(CHANNEL_RATE, 0.0).expect("decoder");
    let frames = dec.process(&iq);

    assert_eq!(frames.len(), 1, "expected exactly one decoded frame");
    let d = &frames[0];

    // RS reports a clean decode (the synthetic channel is noiseless).
    assert!(d.rs.ok(), "RS must report a successful decode");
    assert_eq!(d.rs.total_corrected(), 0);

    // STATUS sub-block fields match the published oracle values.
    let f = &d.frame;
    assert_eq!(f.serial, "K1930293");
    assert_eq!(f.frame_num, 5910);
    assert!((f.battery_v - 2.6).abs() < 1e-6);
    assert!(f.crc.status);

    // GPS-INFO / GPS-POS recovered through the demod path.
    let t = f.gps_time.expect("gps time");
    assert_eq!(t.week, 1800);
    assert_eq!(t.tow_ms, 131_874_000);

    let p = f.gps_pos.as_ref().expect("gps pos");
    assert!((p.lat - 46.050_263).abs() < 1e-5);
    assert!((p.lon - 16.110_771).abs() < 1e-5);
    assert!((p.alt_m - 28_410.02).abs() < 0.1);
    assert_eq!(p.num_sv, 8);

    // The recovered de-whitened wire frame equals the oracle bytes.
    assert_eq!(d.wire_bytes, dewhitened);

    // level_dbfs is a finite power estimate (unit-amplitude carrier ≈ 0 dBFS).
    assert!(dec.level_dbfs().is_finite());
}

#[test]
fn channel_decoder_through_ddc_offset_synth_iq() {
    // Same frame, but transmit it offset from the capture center and run a
    // higher capture rate so the internal Ddc must mix + decimate.
    let dewhitened = hex(FRAME1_DEWHITENED_HEX);
    let on_air = on_air_whitened(&dewhitened);

    let capture_rate = 240_000.0; // 5x the channel rate (integer decimation)
    let offset = 30_000.0; // sonde 30 kHz above capture center
    let iq = modulate::burst_iq(&on_air, capture_rate, offset, 1.0);

    let mut dec = SondeChannelDecoder::new(capture_rate, offset).expect("decoder");
    let frames = dec.process(&iq);

    assert_eq!(frames.len(), 1, "expected one frame after DDC mix/decimate");
    assert_eq!(frames[0].frame.serial, "K1930293");
    assert_eq!(frames[0].frame.frame_num, 5910);
}

#[test]
fn to_message_emits_sonde_rs41_synth_iq() {
    use xng_types::{AppInfo, MessageBody, Mode, Provenance, StationIdentity};

    let dewhitened = hex(FRAME1_DEWHITENED_HEX);
    let on_air = on_air_whitened(&dewhitened);
    let iq = modulate::burst_iq(&on_air, CHANNEL_RATE, 0.0, 1.0);

    let mut dec = SondeChannelDecoder::new(CHANNEL_RATE, 0.0).expect("decoder");
    let frames = dec.process(&iq);
    assert_eq!(frames.len(), 1);

    let source = Provenance {
        station: StationIdentity::new("TEST-STATION"),
        app: AppInfo::xng(),
        sdr: None,
        channel: None,
    };
    let msg = to_message(&frames[0], 404_000_000, dec.level_dbfs(), source);

    assert_eq!(msg.mode, Mode::Sonde);
    assert_eq!(msg.frequency_hz, 404_000_000);
    assert!(msg.decode.crc_ok);
    assert_eq!(msg.decode.fec_corrected, Some(0));
    assert_eq!(msg.signal.rssi_db, Some(dec.level_dbfs()));
    assert_eq!(msg.raw.as_deref(), Some(&dewhitened[..]));

    match &msg.body {
        MessageBody::Sonde { kind, details } => {
            assert_eq!(kind, "rs41");
            assert_eq!(details["serial"], "K1930293");
            assert_eq!(details["frame_num"], 5910);
        }
        other => panic!("expected MessageBody::Sonde, got {other:?}"),
    }
}
