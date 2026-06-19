//! Beast binary output: the de facto Mode S feed format (dump1090
//! port-30005 style) — `0x1a` escape framing, type '2' (7-byte short)
//! or '3' (14-byte long) frames, 6-byte 12 MHz MLAT counter, one
//! signal byte, payload with 0x1a doubled.

use std::sync::Arc;
use tokio::io::AsyncWriteExt;
use tokio::net::TcpListener;
use tokio::sync::broadcast;
use xng_types::{Message, MessageBody, Mode};

use super::aircraft::aircraft_fix;

/// The 12 MHz MLAT counter for a message: prefer the demod's monotonic
/// sample-clock tick (so an MLAT client can fit the receiver's clock drift),
/// else the wall-clock-derived counter. Wraps naturally in 48 bits.
fn beast_ticks(msg: &Message) -> u64 {
    msg.signal
        .rx_ticks_12mhz
        .unwrap_or_else(|| msg.timestamp.timestamp_micros().max(0) as u64 * 12)
        & 0xFFFF_FFFF_FFFF
}

fn sig_byte(msg: &Message) -> u8 {
    msg.signal
        .rssi_db
        .map(|db| (((db + 50.0) / 50.0 * 255.0).clamp(0.0, 255.0)) as u8)
        .unwrap_or(0x80)
}

/// Wrap a raw 7- or 14-byte Mode S frame in Beast framing: `0x1a`, type byte,
/// 6-byte MLAT counter, signal byte, payload — every `0x1a` doubled. `None` for
/// a non-7/14-byte payload.
fn wrap_beast(raw: &[u8], ticks: u64, sig: u8) -> Option<Vec<u8>> {
    let kind = match raw.len() {
        7 => b'2',
        14 => b'3',
        _ => return None,
    };
    let mut out = Vec::with_capacity(2 + 7 + 1 + raw.len() * 2);
    out.push(0x1a);
    out.push(kind);
    let push_esc = |b: u8, out: &mut Vec<u8>| {
        out.push(b);
        if b == 0x1a {
            out.push(0x1a);
        }
    };
    for k in (0..6).rev() {
        push_esc(((ticks >> (8 * k)) & 0xFF) as u8, &mut out);
    }
    push_esc(sig, &mut out);
    for &b in raw {
        push_esc(b, &mut out);
    }
    Some(out)
}

/// The Beast frame(s) for a message. Native Mode S wraps its real 1090 frame
/// (one frame). UAT 978 / HFDL aircraft positions have no 1090 frame, so they
/// are SYNTHESIZED into DF17 extended squitters (even+odd position, +callsign)
/// — the `uat2esnt` trick (XM-2.2, Beast half) — letting raw-Beast consumers
/// (tar1090/readsb) plot them. Empty for everything else.
pub fn format_beast(msg: &Message) -> Vec<Vec<u8>> {
    let (ticks, sig) = (beast_ticks(msg), sig_byte(msg));
    // Native Mode S: wrap the real frame.
    if msg.mode == Mode::Adsb && matches!(msg.body, MessageBody::ModeS { .. }) {
        return msg.raw.as_ref().and_then(|raw| wrap_beast(raw, ticks, sig)).into_iter().collect();
    }
    // Non-Mode-S aircraft (UAT/HFDL): synthesize 1090 ES frames from the fix.
    // Native ADS-B → DF17; a UAT TIS-B/ADS-R rebroadcast → DF18 (CF preserves
    // the provenance) so a raw-Beast consumer keeps the source class.
    if let Some(fix) = aircraft_fix(msg) {
        if let Ok(icao) = u32::from_str_radix(&fix.icao, 16) {
            use crate::outputs::aircraft::AircraftSource;
            use xng_mode_adsb::synth::EsSource;
            let src = match fix.source {
                AircraftSource::Adsb => EsSource::Adsb,
                AircraftSource::TisB => EsSource::TisB,
                AircraftSource::TisBOther => EsSource::TisBOther,
                AircraftSource::AdsR => EsSource::AdsR,
            };
            return xng_mode_adsb::synth::synth_frames(
                src,
                icao,
                fix.lat,
                fix.lon,
                fix.altitude_ft,
                fix.callsign.as_deref(),
                // Only a true ground speed synthesizes a TC19 ground-velocity
                // frame; an airspeed-only fix has no ground vector to encode.
                if fix.speed_is_airspeed { None } else { fix.speed_kt },
                fix.track_deg,
                fix.vertical_rate_fpm,
            )
            .iter()
            .filter_map(|f| wrap_beast(f, ticks, sig))
            .collect();
        }
    }
    Vec::new()
}

/// Serve Beast frames on `addr` (e.g. `0.0.0.0:30005`).
pub async fn run(rx: broadcast::Receiver<Arc<Message>>, addr: String) -> std::io::Result<()> {
    let listener = TcpListener::bind(&addr).await?;
    tracing::info!("Beast output on {addr}");
    loop {
        let (mut sock, peer) = listener.accept().await?;
        tracing::info!("Beast client connected: {peer}");
        let mut rx = rx.resubscribe();
        tokio::spawn(async move {
            loop {
                match rx.recv().await {
                    Ok(msg) => {
                        let mut dead = false;
                        for frame in format_beast(&msg) {
                            if sock.write_all(&frame).await.is_err() {
                                dead = true;
                                break;
                            }
                        }
                        if dead {
                            break;
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(_) => break,
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use xng_types::{AppInfo, DecodeQuality, Provenance, SignalQuality, StationIdentity};

    #[test]
    fn long_frame_renders_with_escaping() {
        let mut raw = vec![0x8Du8; 14];
        raw[5] = 0x1a; // force one escape
        let msg = Message {
            mode: Mode::Adsb,
            timestamp: chrono::Utc::now(),
            frequency_hz: 1_090_000_000,
            signal: SignalQuality { rssi_db: Some(-20.0), ..Default::default() },
            decode: DecodeQuality { crc_ok: true, fec_corrected: None, errors: None },
            body: MessageBody::ModeS {
                df: 17,
                icao: Some("ABCDEF".into()),
                callsign: None,
                altitude_ft: None,
                squawk: None,
                lat: None,
                lon: None,
                speed_kt: None,
                speed_type: None,
                track_deg: None,
                vertical_rate_fpm: None,
                comm_b: None,
                adsb_status: None,
            },
            raw: Some(raw),
            source: Provenance {
                station: StationIdentity::new("T"),
                app: AppInfo::xng(),
                sdr: None,
                channel: None,
            },
        };
        let frames = format_beast(&msg);
        assert_eq!(frames.len(), 1, "Mode S → one wrapped frame");
        let f = &frames[0];
        assert_eq!(f[0], 0x1a);
        assert_eq!(f[1], b'3');
        // 0x1a payload byte must be doubled.
        let esc_count = f[2..].windows(2).filter(|w| w == &[0x1a, 0x1a]).count();
        assert!(esc_count >= 1);
    }

    // The Beast 6-byte 12 MHz counter is the demod's monotonic sample-clock
    // tick (MSB-first), not the wall clock.
    #[test]
    fn uses_sample_clock_mlat_tick() {
        let raw = vec![0x8Du8; 14]; // no 0x1a → timestamp bytes are not escaped
        let msg = Message {
            mode: Mode::Adsb,
            timestamp: chrono::Utc::now(),
            frequency_hz: 1_090_000_000,
            signal: SignalQuality { rx_ticks_12mhz: Some(0x0102_0304_0506), ..Default::default() },
            decode: DecodeQuality { crc_ok: true, fec_corrected: None, errors: None },
            body: MessageBody::ModeS {
                df: 17,
                icao: Some("ABCDEF".into()),
                callsign: None,
                altitude_ft: None,
                squawk: None,
                lat: None,
                lon: None,
                speed_kt: None,
                speed_type: None,
                track_deg: None,
                vertical_rate_fpm: None,
                comm_b: None,
                adsb_status: None,
            },
            raw: Some(raw),
            source: Provenance {
                station: StationIdentity::new("T"),
                app: AppInfo::xng(),
                sdr: None,
                channel: None,
            },
        };
        let f = &format_beast(&msg)[0];
        // [0]=0x1a, [1]='3', [2..8] = the 6 MLAT bytes, MSB first.
        assert_eq!(&f[2..8], &[0x01, 0x02, 0x03, 0x04, 0x05, 0x06], "{f:?}");
    }

    // XM-2.2 Beast half: a UAT 978 ADS-B position with no raw 1090 frame is
    // SYNTHESIZED into DF17 ES frames (even+odd position + callsign) so raw-Beast
    // consumers can plot it.
    #[test]
    fn uat_position_synthesizes_df17_beast_frames() {
        let msg = Message {
            mode: Mode::Uat,
            timestamp: chrono::Utc::now(),
            frequency_hz: 978_000_000,
            signal: SignalQuality::default(),
            decode: DecodeQuality { crc_ok: true, fec_corrected: None, errors: None },
            body: MessageBody::Uat {
                kind: "adsb".into(),
                details: serde_json::json!({
                    "address": "a1b2c3", "callsign": "N12345",
                    "geometric_altitude": 9500, "lat": 37.6189, "lon": -122.3750,
                    "ground_speed": 142.0, "true_track": 271.0, "vertical_rate": -640,
                }),
            },
            raw: None,
            source: Provenance {
                station: StationIdentity::new("T"),
                app: AppInfo::xng(),
                sdr: None,
                channel: None,
            },
        };
        let frames = format_beast(&msg);
        // even + odd position + ident + velocity (ground speed/track present).
        assert_eq!(frames.len(), 4, "{} frames", frames.len());
        for f in &frames {
            assert_eq!(f[0], 0x1a);
            assert_eq!(f[1], b'3', "long (DF17) frame");
        }
    }

    // Unescape a wrapped Beast frame: strip 0x1a + type byte, collapse doubled
    // 0x1a; result is 6 MLAT + 1 signal + 14 ES bytes, so [7] is the ES b[0].
    fn es_byte0(f: &[u8]) -> u8 {
        let mut out = Vec::new();
        let mut i = 2;
        while i < f.len() {
            out.push(f[i]);
            i += if f[i] == 0x1a { 2 } else { 1 };
        }
        out[7]
    }

    // NEW-P0-1.3: a UAT ADS-R rebroadcast (address_qualifier) is synthesized as
    // DF18 with CF=6 (not native DF17), so its provenance survives onto 1090.
    #[test]
    fn uat_adsr_synthesizes_df18_frames() {
        let msg = Message {
            mode: Mode::Uat,
            timestamp: chrono::Utc::now(),
            frequency_hz: 978_000_000,
            signal: SignalQuality::default(),
            decode: DecodeQuality { crc_ok: true, fec_corrected: None, errors: None },
            body: MessageBody::Uat {
                kind: "adsb".into(),
                details: serde_json::json!({
                    "address": "a1b2c3", "address_qualifier": "adsr_other",
                    "geometric_altitude": 9500, "lat": 37.6189, "lon": -122.3750,
                }),
            },
            raw: None,
            source: Provenance {
                station: StationIdentity::new("T"),
                app: AppInfo::xng(),
                sdr: None,
                channel: None,
            },
        };
        let frames = format_beast(&msg);
        assert_eq!(frames.len(), 2, "even + odd position");
        for f in &frames {
            let b0 = es_byte0(f);
            assert_eq!(b0 >> 3, 18, "DF18 for ADS-R");
            assert_eq!(b0 & 7, 6, "CF=6 ADS-R");
        }
    }
}
