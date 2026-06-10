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
    let stats = live.stats.lock().unwrap().clone();
    for (freq, frames, ok, level) in &stats {
        let labels = format!("{{mode=\"{mode}\",freq=\"{freq}\"}}");
        out.push_str(&format!("xng_frames_total{labels} {frames}\n"));
        out.push_str(&format!("xng_frames_crc_ok_total{labels} {ok}\n"));
        out.push_str(&format!("xng_channel_level_dbfs{labels} {level:.1}\n"));
    }
    out.push_str(&format!(
        "xng_samples_total{{mode=\"{mode}\"}} {}\n",
        live.samples.load(Ordering::Relaxed)
    ));
    out
}
