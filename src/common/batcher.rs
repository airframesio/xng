use elasticsearch::Elasticsearch;
use log::*;
use std::time::Duration;

use actix_web::web::Data;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use tokio::time::sleep;

use super::es_utils::bulk_index;
use super::frame::CommonFrame;

pub fn create_es_batch_task(
    client: &Elasticsearch,
    index: &String,
    batch: Data<Mutex<Vec<CommonFrame>>>,
    duration: Duration,
) -> JoinHandle<()> {
    let client = client.clone();
    let index = index.clone();

    tokio::spawn(async move {
        sleep(duration).await;

        let mut batch = batch.lock().await;

        if let Err(e) = bulk_index(&client, &index, batch.as_ref()).await {
            warn!("Failed to bulk index batch: {}", e.to_string());
        }

        batch.clear();
    })
}
