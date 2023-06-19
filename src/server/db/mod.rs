use log::*;
use sqlx::{migrate::MigrateDatabase, Sqlite, SqlitePool};
use tokio::io;

use crate::common::frame::CommonFrame;

use self::migrations as db_migrations;

mod migrations;

pub struct StateDB {
    db: SqlitePool,
}

impl StateDB {
    pub async fn new(db_url: String) -> Result<StateDB, io::Error> {
        if !Sqlite::database_exists(db_url.as_str())
            .await
            .unwrap_or(false)
        {
            match Sqlite::create_database(db_url.as_str()).await {
                Ok(_) => debug!(""),
                Err(e) => {
                    return Err(io::Error::new(
                        io::ErrorKind::Other,
                        format!("Failed to create state DB: {}", e.to_string()),
                    ))
                }
            }
        } else {
        }

        let db = match SqlitePool::connect(db_url.as_str()).await {
            Ok(x) => x,
            Err(e) => {
                return Err(io::Error::new(
                    io::ErrorKind::Other,
                    format!("Failed to connect to {}: {}", db_url, e.to_string()),
                ))
            }
        };

        if let Err(e) = db_migrations::run(&db).await {
            return Err(io::Error::new(
                io::ErrorKind::Other,
                format!("Failed to run migrations: {}", e.to_string()),
            ));
        }

        Ok(StateDB { db })
    }

    pub async fn update(&self, frame: &CommonFrame) -> Result<(), io::Error> {
        println!("{:?}", frame);
        Ok(())
    }
}
