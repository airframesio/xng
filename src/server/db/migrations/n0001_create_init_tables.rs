use async_trait::async_trait;
use sqlx::SqlitePool;
use tokio::io;

use crate::server::db::Migration;

pub struct CreateInitTables;

#[async_trait]
impl Migration for CreateInitTables {
    async fn migrate(&self, db: &SqlitePool) -> Result<(), io::Error> {
        let queries = vec![
            "
            CREATE TABLE IF NOT EXISTS ground_stations (
                id   INTEGER PRIMARY KEY AUTOINCREMENT,
                name TEXT
            )   
            ",
            "
            CREATE TABLE IF NOT EXISTS aircraft (
                id           INTEGER PRIMARY KEY AUTOINCREMENT,

                icao         TEXT,
                tail         TEXT,
            
                UNIQUE(icao)
                FOREIGN KEY(gs_id) REFERENCES ground_stations(id)
            )  
            ",
            "
            CREATE TABLE IF NOT EXISTS flight (
                id           INTEGER PRIMARY KEY AUTOINCREMENT,
                aircraft_id  INTEGER,
            
                last_updated DATETIME,

                FOREIGN KEY(aircraft_id) REFERENCES aircraft(id)
            )
            ",
            "
            CREATE TABLE IF NOT EXISTS aircraft_log_event (
                id          INTEGER PRIMARY KEY AUTOINCREMENT,
                aircraft_id INTEGER NOT NULL,
                gs_id       INTEGER NOT NULL,
            
                ts          DATETIME NOT NULL,
                latitude    REAL NOT NULL,
                longitude   REAL NOT NULL,
                altitude    INTEGER,
                
                FOREIGN KEY(aircraft_id) REFERENCES aircraft(id)
                FOREIGN KEY(gs_id) REFERENCES ground_stations(id)
            )    
            ",
        ];

        for query in queries.iter() {
            if let Err(e) = sqlx::query(query).execute(db).await {
                return Err(io::Error::new(
                    io::ErrorKind::Other,
                    format!("Failed to run query: {}\n\n{}", e.to_string(), query),
                ));
            }
        }

        Ok(())
    }
}
