use async_trait::async_trait;
use sqlx::SqlitePool;
use tokio::io;

use crate::server::db::migrations::n0001_create_init_tables::CreateInitTables;

mod n0001_create_init_tables;

#[async_trait]
pub trait Migration {
    async fn migrate(&self, db: &SqlitePool) -> Result<(), io::Error>;
}

pub async fn run(db: &SqlitePool) -> Result<(), io::Error> {
    let xng_migrations: Vec<Box<dyn Migration>> = vec![Box::new(CreateInitTables)];

    for migration in xng_migrations.iter() {
        migration.migrate(db).await?;
    }

    Ok(())
}
