use async_trait::async_trait;
use sqlx::SqlitePool;
use tokio::io;

mod migrations;

#[async_trait]
pub trait Migration {
    async fn migrate(&self, db: &SqlitePool) -> Result<(), io::Error>;
}
