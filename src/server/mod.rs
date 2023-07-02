use actix_web::web::Data;
use clap::{arg, ArgMatches, Command};
use log::*;
use reqwest::Url;
use serde_valid::Validate;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::net::TcpListener;
use tokio::select;
use tokio::signal::unix::{signal, SignalKind};
use tokio::sync::{mpsc, Mutex, RwLock};
use tokio::task::JoinHandle;
use tokio::time::{self, Duration};
use tokio_util::sync::CancellationToken;

use crate::common;
use crate::common::arguments::{
    parse_disable_state_db, parse_elastic_url, parse_listen_host, parse_listen_port,
    parse_state_db_url,
};
use crate::common::batcher::create_es_batch_task;
use crate::common::frame::CommonFrame;
use crate::server::db::StateDB;

pub mod db;
pub mod services;

const DEFAULT_LISTEN_HOST: &'static str = "0.0.0.0";
const DEFAULT_STATE_DB_URL: &'static str = "sqlite://state.sqlite3";

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
    let listen_host = parse_listen_host(args, DEFAULT_LISTEN_HOST);
    let ingest_port: u16 = parse_listen_port(args, DEFAULT_INGEST_PORT);
    let inactive_timeout_secs: u64 = args
        .get_one::<String>("inactive-timeout")
        .unwrap_or(&String::from("default"))
        .parse::<u64>()
        .unwrap_or(DEFAULT_INACTIVE_TIMEOUT_SECS);

    let elastic_url = if let Some(raw_url) = parse_elastic_url(args) {
        match Url::parse(raw_url) {
            Ok(v) => {
                info!("Elasticsearch bulk indexing enabled: target = {}", raw_url);
                Some(v)
            }
            Err(e) => {
                error!("Elastisearch URL is invalid: {}", e.to_string());
                return;
            }
        }
    } else {
        None
    };

    let state_db_url = match Url::parse(parse_state_db_url(args, DEFAULT_STATE_DB_URL).as_str()) {
        Ok(v) => {
            info!("State DB location at {}", v.as_str());
            v
        }
        Err(e) => {
            error!("Invalid state DB URL: {}", e.to_string());
            return;
        }
    };
    let disable_state_db = parse_disable_state_db(args);
    if disable_state_db {
        debug!("State DB disabled");
    }

    let state_db = match StateDB::new(if disable_state_db {
        None
    } else {
        Some(state_db_url.to_string())
    })
    .await
    {
        Ok(v) => Data::new(RwLock::new(v)),
        Err(e) => {
            error!("Failed to create state DB: {}", e.to_string());
            return;
        }
    };

    // TODO: Init actix-web server

    let cancel_token = CancellationToken::new();
    let ingest_cancel_token = cancel_token.clone();

    let (tx, mut rx) = mpsc::channel::<CommonFrame>(DEFAULT_CHANNEL_BUFFER);

    let ingest_thread = tokio::spawn(async move {
        let listener = match TcpListener::bind(format!("{}:{}", listen_host, ingest_port)).await {
            Ok(x) => x,
            Err(e) => {
                error!(
                    "Failed to listen on {}:{} => {}",
                    listen_host,
                    ingest_port,
                    e.to_string()
                );
                return;
            }
        };

        info!(
            "Aggregator server listening on {}:{}",
            listen_host, ingest_port
        );

        loop {
            select! {
                Ok((client, client_addr)) = listener.accept() => {
                    info!("New client from {} accepted.", client_addr.ip());

                    let tx = tx.clone();

                    tokio::spawn(async move {
                        let mut reader = BufReader::new(client);

                        loop {
                            let mut msg = String::new();

                            let Ok(result) = time::timeout(
                                Duration::from_secs(inactive_timeout_secs),
                                reader.read_line(&mut msg)
                            ).await else {
                                info!("Client from {} idled for longer than {} seconds. ", client_addr.ip(), inactive_timeout_secs);
                                break;
                            };

                            let Ok(size) = result else {
                                debug!("Failed to unwrap result from read_line");
                                break;
                            };

                            if size == 0 {
                                debug!("Got EOF, shutting down client socket");
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
                _ = ingest_cancel_token.cancelled() => {
                    info!("Ingest thread got cancel request");
                    break;
                }
            }
        }

        info!("Ingest thread exited");
    });

    let frames_batch: Data<Mutex<Vec<CommonFrame>>> = Data::new(Mutex::new(Vec::new()));
    let mut batcher: Option<JoinHandle<()>> = None;

    // TODO: use elastic_url to setup elasticsearch client as an Option

    let mut interrupt_signal = match signal(SignalKind::interrupt()) {
        Ok(v) => v,
        Err(e) => {
            error!(
                "Failed to initialize signal handler to detect interrupts: {}",
                e.to_string()
            );
            return;
        }
    };

    loop {
        select! {
            Some(frame) = rx.recv() => {
                {
                    let state_db = state_db.write().await;
                    if let Err(e) = state_db.update(&frame).await {
                        warn!("Failed to updated state DB with frame: {}", e.to_string());
                    }
                }

                // TODO: use the newly created elasticsearch client option instead of elastic_url
                if let Some(es_url) = elastic_url.as_ref() {
                    let mut batch = frames_batch.lock().await;
                    let es_url = es_url.clone();

                    if batch.len() == 0 {
                        let frames_batch = frames_batch.clone();

                        batcher = Some(
                            create_es_batch_task(
                                es_url,
                                frames_batch,
                                Duration::from_millis(DEFAULT_BATCH_WAIT_MS)
                            )
                        );
                    }

                    debug!("Pushing frame to batch...");
                    batch.push(frame);
                }
            }
            _ = interrupt_signal.recv() => {
                info!("Interrupt signal detected, attempting to cleanly exit");

                break;
            }
        }
    }

    if let Some(batcher) = batcher {
        debug!("Batcher is active, waiting for completion before exiting to prevent data loss");
        if let Err(e) = batcher.await {
            warn!(
                "Error occurred while waiting for batcher to finish: {}",
                e.to_string()
            );
        }
    }

    debug!("Signaling ingest thread to cancel");
    cancel_token.cancel();

    debug!("Waiting for ingest thread to finish");
    if let Err(e) = ingest_thread.await {
        warn!(
            "Error occurred while waiting for ingest thread to exit: {}",
            e.to_string()
        );
    }

    info!("Server exited");
}
