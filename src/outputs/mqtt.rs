//! MQTT output: publish each normalized message as JSON to
//! `<prefix>/<mode>` (e.g. `xng/vdl2`). URL form:
//! `mqtt://[user:pass@]host[:port]` (default port 1883).

use rumqttc::{AsyncClient, MqttOptions, QoS};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::broadcast;
use xng_types::Message;

/// Parse `mqtt://[user:pass@]host[:port]` into options.
fn parse_url(url: &str, client_id: &str) -> anyhow::Result<MqttOptions> {
    let rest = url
        .strip_prefix("mqtt://")
        .ok_or_else(|| anyhow::anyhow!("--mqtt URL must start with mqtt://"))?;
    let (auth, hostport) = match rest.rsplit_once('@') {
        Some((a, h)) => (Some(a), h),
        None => (None, rest),
    };
    let (host, port) = match hostport.rsplit_once(':') {
        Some((h, p)) => (h, p.parse::<u16>()?),
        None => (hostport, 1883),
    };
    let mut opts = MqttOptions::new(client_id, host, port);
    opts.set_keep_alive(Duration::from_secs(30));
    if let Some(auth) = auth {
        let (user, pass) = auth
            .split_once(':')
            .ok_or_else(|| anyhow::anyhow!("--mqtt credentials must be user:pass"))?;
        opts.set_credentials(user, pass);
    }
    Ok(opts)
}

/// Consume the bus until it closes, publishing each message.
pub async fn run(
    mut rx: broadcast::Receiver<Arc<Message>>,
    url: String,
    topic_prefix: String,
    station: String,
) -> anyhow::Result<()> {
    let opts = parse_url(&url, &format!("xng-{station}"))?;
    let (client, mut eventloop) = AsyncClient::new(opts, 64);
    // The event loop must be polled for the client to make progress;
    // connection errors are retried with a small backoff.
    tokio::spawn(async move {
        loop {
            if let Err(e) = eventloop.poll().await {
                tracing::warn!("mqtt: {e}; reconnecting");
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
        }
    });
    loop {
        match rx.recv().await {
            Ok(msg) => {
                let topic = format!("{topic_prefix}/{}", msg.mode.as_str());
                let payload = serde_json::to_vec(&*msg)?;
                if let Err(e) = client.publish(topic, QoS::AtMostOnce, false, payload).await {
                    tracing::warn!("mqtt publish: {e}");
                }
            }
            Err(broadcast::error::RecvError::Lagged(n)) => {
                tracing::warn!("mqtt output lagged, dropped {n} messages");
            }
            Err(broadcast::error::RecvError::Closed) => break,
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn url_forms() {
        let o = parse_url("mqtt://broker.local", "id").unwrap();
        assert_eq!(o.broker_address(), ("broker.local".to_string(), 1883));
        let o = parse_url("mqtt://broker.local:8883", "id").unwrap();
        assert_eq!(o.broker_address(), ("broker.local".to_string(), 8883));
        let o = parse_url("mqtt://u:p@broker.local:1884", "id").unwrap();
        assert_eq!(o.broker_address(), ("broker.local".to_string(), 1884));
        let login = o.credentials().expect("credentials set");
        assert_eq!(format!("{login:?}"), format!("{:?}", rumqttc::Login::new("u", "p")));
        assert!(parse_url("tcp://x", "id").is_err());
    }
}
