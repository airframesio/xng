use clap::{arg, ArgMatches, Command};
use log::*;
use reqwest::Url;
use serde_json::Value;
use serde_valid::Validate;
use tokio::net::TcpListener;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::select;
use tokio::signal::unix::{SignalKind, signal};
use tokio::sync::mpsc;
use tokio::time::{self, Duration, Instant};

use crate::common;
use crate::common::frame::CommonFrame;

pub const SERVER_COMMAND: &'static str = "server";
pub const DEFAULT_INGEST_PORT: u16 = 5552;
pub const DEFAULT_INACTIVE_TIMEOUT_SECS: u64 = 3600;

pub const DEFAULT_CHANNEL_BUFFER: usize = 4096;
pub const DEFAULT_BATCH_WAIT_MS: u64 = 200;

pub fn get_server_arguments() -> Command {
    common::arguments::register_common_arguments(
        Command::new(SERVER_COMMAND)
            .about("Aggregator server mode")
            .args(&[
                arg!(--tcp <PORT> "TCP port to listen for frames on (default: 5552)"),
                arg!(--"inactive-timeout" <SECONDS> "Disconnect client if inactive for specified seconds (default: 60)")
            ]),
    )
}

pub async fn start(args: &ArgMatches) {
    let listen_host = args
        .get_one::<String>("listen-host")
        .unwrap_or(&"0.0.0.0".to_string())
        .to_owned();
    let ingest_port: u16 = *args.get_one("tcp").unwrap_or(&DEFAULT_INGEST_PORT);
    let inactive_timeout_secs: u64 = *args
        .get_one("inactive-timeout")
        .unwrap_or(&DEFAULT_INACTIVE_TIMEOUT_SECS);
    let elastic_url = args
        .get_one::<Url>("elastic");

    // TODO: Init actix-web server

    let (tx, mut rx) = mpsc::channel::<CommonFrame>(DEFAULT_CHANNEL_BUFFER);
    
    let ingest_thread = tokio::spawn(async move {
        let listener = match TcpListener::bind(format!("{}:{}", listen_host, ingest_port)).await {
            Ok(x) => x,
            Err(e) => {
                error!("Failed to listen on {}:{} => {}", listen_host, ingest_port, e);
                return; 
            }
        };

        loop {
            let (client, client_addr) = match listener.accept().await {
                Ok(x) => x,
                Err(e) => {
                    error!("Failed to accept client: {}", e);
                    break;
                }
            };

            info!("New client from {} accepted.", client_addr.ip());

            let tx = tx.clone();
            
            tokio::spawn(async move {
                let mut reader = BufReader::new(client);

                loop {
                    let mut msg = String::new();

                    let Ok(result) = time::timeout(Duration::from_secs(inactive_timeout_secs), reader.read_line(&mut msg)).await else {
                        info!("Client from {} idled for longer than {} seconds. ", client_addr.ip(), inactive_timeout_secs);
                        break;
                    };
                    
                    let Ok(size) = result else {
                        debug!("Failed to unwrap result from read_line");
                        break;  
                    };

                    if size == 0 {
                        debug!("Received 0 bytes, client socket is probably dead.");
                        break;
                    }

                    let frame = match serde_json::from_str::<CommonFrame>(&msg) {
                        Ok(frame) => frame,
                        Err(e) => {
                            error!("Malformed common frame: {}", e.to_string());
                            continue
                        }
                    };

                    if let Err(e) = frame.validate() {
                        error!("Common Frame failed validation: {}", e.to_string());
                        continue;    
                    }
                    
                    if let Err(e) = tx.send(frame).await {
                        error!("Failed to send common frame to parse thread: {}", e.to_string());
                    }
                }
            });
        }

        info!("Ingest thread exiting...");
    });

    let mut since_batch_start: Option<Instant> = None;
    let mut batch: Vec<Value> = Vec::new();
    let Ok(mut interrupt_signal) = signal(SignalKind::interrupt()) else {
        error!("Failed to register interrupt signal");
        return;    
    };
    
    loop {
        select! {
            Ok(frame) = time::timeout(
                Duration::from_millis(
                    if since_batch_start.is_none() { inactive_timeout_secs * 1000 } else { DEFAULT_BATCH_WAIT_MS }
                ), rx.recv()
            ) => {
                // TODO: parse frame
            }
            _ = interrupt_signal.recv() => {
                // TODO
                break;
            }
        }

        // TODO: only if we are sending to ES
        
        if let Some(batch_start) = since_batch_start {
            if batch_start.elapsed() >= Duration::from_millis(DEFAULT_BATCH_WAIT_MS) && !batch.is_empty() {
                // TODO: send batch to ElasticSearch
                
                batch.clear();
                since_batch_start = None;
            }
        }
    }

    info!("Got interrupt signal, exiting...");
    ingest_thread.abort();
}
