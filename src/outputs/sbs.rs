//! SBS-1 ("BaseStation") output: the CSV line protocol dump1090/readsb
//! serve on TCP 30003, consumed by Virtual Radar Server, PlanePlotter,
//! and most ADS-B aggregator feeders. We serve it the same way: listen,
//! and stream MSG lines to every connected client.

use std::sync::Arc;
use tokio::io::AsyncWriteExt;
use tokio::net::TcpListener;
use tokio::sync::broadcast;
use xng_types::{Message, MessageBody};

/// Render a message as an SBS MSG line (only Mode S bodies map).
pub fn format_sbs(msg: &Message) -> Option<String> {
    let MessageBody::ModeS {
        icao,
        callsign,
        altitude_ft,
        squawk,
        lat,
        lon,
        speed_kt,
        speed_type,
        track_deg,
        vertical_rate_fpm,
        ..
    } = &msg.body
    else {
        return None;
    };
    let icao = icao.as_deref()?;
    // Transmission type: 1 ident, 3 airborne position, 4 velocity,
    // 5 surveillance altitude, 6 squawk.
    let tt = if lat.is_some() {
        3
    } else if speed_kt.is_some() && speed_type.as_deref() != Some("AS") {
        4
    } else if callsign.is_some() {
        1
    } else if squawk.is_some() {
        6
    } else if altitude_ft.is_some() {
        5
    } else {
        return None;
    };
    let d = msg.timestamp.format("%Y/%m/%d");
    let t = msg.timestamp.format("%H:%M:%S%.3f");
    let fmt_f = |v: &Option<f64>, p: usize| v.map(|x| format!("{x:.p$}")).unwrap_or_default();
    let fmt_i = |v: &Option<i32>| v.map(|x| x.to_string()).unwrap_or_default();
    Some(format!(
        "MSG,{tt},1,1,{icao},1,{d},{t},{d},{t},{},{},{},{},{},{},{},{},,,,",
        callsign.as_deref().unwrap_or(""),
        fmt_i(altitude_ft),
        fmt_f(speed_kt, 1),
        fmt_f(track_deg, 1),
        fmt_f(lat, 5),
        fmt_f(lon, 5),
        fmt_i(vertical_rate_fpm),
        squawk.as_deref().unwrap_or(""),
    ))
}

/// Serve SBS lines on `addr` (e.g. `0.0.0.0:30003`).
pub async fn run(rx: broadcast::Receiver<Arc<Message>>, addr: String) -> std::io::Result<()> {
    let listener = TcpListener::bind(&addr).await?;
    tracing::info!("SBS (BaseStation) output on {addr}");
    loop {
        let (mut sock, peer) = listener.accept().await?;
        tracing::info!("SBS client connected: {peer}");
        let mut rx = rx.resubscribe();
        tokio::spawn(async move {
            loop {
                match rx.recv().await {
                    Ok(msg) => {
                        if let Some(line) = format_sbs(&msg) {
                            if sock.write_all(format!("{line}\r\n").as_bytes()).await.is_err() {
                                break;
                            }
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
    use xng_types::{DecodeQuality, Mode, Provenance, SignalQuality, StationIdentity};

    #[test]
    fn position_message_renders_msg3() {
        let msg = Message {
            mode: Mode::Adsb,
            timestamp: chrono::Utc::now(),
            frequency_hz: 1_090_000_000,
            signal: SignalQuality::default(),
            decode: DecodeQuality { crc_ok: true, fec_corrected: None, errors: None },
            body: MessageBody::ModeS {
                df: 17,
                icao: Some("40621D".into()),
                callsign: None,
                altitude_ft: Some(38_000),
                squawk: None,
                lat: Some(52.2572),
                lon: Some(3.91937),
                speed_kt: None,
                speed_type: None,
                track_deg: None,
                vertical_rate_fpm: None,
                comm_b: None,
                adsb_status: None,
            },
            raw: None,
            source: Provenance {
                station: StationIdentity::new("T"),
                app: xng_types::AppInfo::xng(),
                sdr: None,
                channel: None,
            },
        };
        let line = format_sbs(&msg).unwrap();
        assert!(line.starts_with("MSG,3,1,1,40621D,1,"), "{line}");
        assert!(line.contains(",38000,"), "{line}");
        assert!(line.contains(",52.25720,3.91937,"), "{line}");
    }
}
