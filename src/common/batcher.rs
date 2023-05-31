use log::*;
use std::time::Duration;

use actix_web::web::Data;
use reqwest::Url;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use tokio::time::sleep;

use super::frame::CommonFrame;

pub fn create_es_batch_task(
    es_url: Url,
    batch: Data<Mutex<Vec<CommonFrame>>>,
    duration: Duration,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        sleep(duration).await;

        let mut batch = batch.lock().await;

        // TODO: send batch to Elasticsearch

        debug!("Sending {} items in batch to {}", batch.len(), es_url);
        batch.clear();
    })
}
