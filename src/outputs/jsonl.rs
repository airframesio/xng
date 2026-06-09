//! JSONL file output: one normalized message per line, append-only.

use std::path::Path;
use std::sync::Arc;
use tokio::fs::OpenOptions;
use tokio::io::AsyncWriteExt;
use tokio::sync::broadcast;
use xng_types::Message;

/// Consume the bus until it closes, appending each message as one JSON line.
pub async fn run(mut rx: broadcast::Receiver<Arc<Message>>, path: &Path) -> std::io::Result<()> {
    let mut file = OpenOptions::new().create(true).append(true).open(path).await?;
    loop {
        match rx.recv().await {
            Ok(msg) => {
                let mut line = serde_json::to_vec(&*msg)?;
                line.push(b'\n');
                file.write_all(&line).await?;
            }
            Err(broadcast::error::RecvError::Lagged(n)) => {
                tracing::warn!("jsonl output lagged, dropped {n} messages");
            }
            Err(broadcast::error::RecvError::Closed) => break,
        }
    }
    file.flush().await
}
