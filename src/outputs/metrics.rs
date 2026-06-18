//! Prometheus metrics endpoint: a minimal HTTP server (no framework)
//! rendering the live session state in the text exposition format.
//! Metric families follow the acarshub-style per-mode conventions.

use crate::runtime::LiveState;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

pub async fn serve(addr: String, live: Arc<LiveState>, mode: String) -> std::io::Result<()> {
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    tracing::info!("metrics endpoint on http://{addr}/metrics");
    loop {
        let (mut sock, _) = listener.accept().await?;
        let live = live.clone();
        let mode = mode.clone();
        tokio::spawn(async move {
            let mut buf = [0u8; 1024];
            let _ = sock.read(&mut buf).await; // request ignored beyond read
            let body = render(&live, &mode);
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/plain; version=0.0.4\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            let _ = sock.write_all(resp.as_bytes()).await;
        });
    }
}

fn render(live: &LiveState, mode: &str) -> String {
    let mut out = String::with_capacity(1024);
    out.push_str("# TYPE xng_frames_total counter\n");
    out.push_str("# TYPE xng_frames_crc_ok_total counter\n");
    out.push_str("# TYPE xng_channel_level_dbfs gauge\n");
    out.push_str("# TYPE xng_samples_total counter\n");
    out.push_str("# TYPE xng_acars_messages_total counter\n");
    let stats = live.stats.lock().unwrap().clone();
    for (freq, frames, ok, level) in &stats {
        let labels = format!("{{mode=\"{mode}\",freq=\"{freq}\"}}");
        out.push_str(&format!("xng_frames_total{labels} {frames}\n"));
        out.push_str(&format!("xng_frames_crc_ok_total{labels} {ok}\n"));
        out.push_str(&format!("xng_channel_level_dbfs{labels} {level:.1}\n"));
    }
    // Per-label ACARS message counts (VERIFY-9 / ACARS-5.2): the dimension
    // the flat per-channel stats can't carry. Sorted for stable output.
    let mut acars: Vec<((u64, String), u64)> =
        live.acars_labels.lock().unwrap().iter().map(|(k, v)| (k.clone(), *v)).collect();
    acars.sort();
    for ((freq, label), count) in &acars {
        let label = escape_label(label);
        out.push_str(&format!(
            "xng_acars_messages_total{{mode=\"{mode}\",freq=\"{freq}\",label=\"{label}\"}} {count}\n"
        ));
    }
    out.push_str(&format!(
        "xng_samples_total{{mode=\"{mode}\"}} {}\n",
        live.samples.load(Ordering::Relaxed)
    ));
    out
}

/// Escape a label value for the Prometheus text exposition format: backslash,
/// double-quote, and newline per the spec. ACARS labels are mostly two ASCII
/// chars but can carry control bytes on a garbled frame.
fn escape_label(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_per_channel_and_per_label_counters() {
        let live = LiveState::new();
        live.record_channel(131_550_000, 10, 7, -42.5);
        // Two messages of label 80, one of label H1 on the same channel.
        live.record_acars_label(131_550_000, "80");
        live.record_acars_label(131_550_000, "80");
        live.record_acars_label(131_550_000, "H1");
        let body = render(&live, "acars");

        assert!(body.contains("# TYPE xng_acars_messages_total counter\n"));
        assert!(
            body.contains("xng_frames_total{mode=\"acars\",freq=\"131550000\"} 10\n"),
            "{body}"
        );
        assert!(
            body.contains(
                "xng_acars_messages_total{mode=\"acars\",freq=\"131550000\",label=\"80\"} 2\n"
            ),
            "{body}"
        );
        assert!(
            body.contains(
                "xng_acars_messages_total{mode=\"acars\",freq=\"131550000\",label=\"H1\"} 1\n"
            ),
            "{body}"
        );
    }

    #[test]
    fn record_channel_keys_by_freq_not_index() {
        // Two station sessions both number their first channel 0, but distinct
        // freqs must not clobber each other in the shared state.
        let live = LiveState::new();
        live.record_channel(131_550_000, 5, 5, -40.0);
        live.record_channel(162_000_000, 3, 2, -38.0);
        live.record_channel(131_550_000, 9, 8, -41.0); // update, not duplicate
        let stats = live.stats.lock().unwrap().clone();
        assert_eq!(stats.len(), 2, "one row per freq: {stats:?}");
        let acars = stats.iter().find(|e| e.0 == 131_550_000).unwrap();
        assert_eq!((acars.1, acars.2), (9, 8));
    }

    #[test]
    fn escapes_label_special_chars() {
        assert_eq!(escape_label("H1"), "H1");
        assert_eq!(escape_label("a\"b\\c"), "a\\\"b\\\\c");
    }
}
