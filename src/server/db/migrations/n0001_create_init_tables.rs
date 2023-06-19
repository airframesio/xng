use async_trait::async_trait;
use sqlx::SqlitePool;
use tokio::io;

use super::Migration;

pub struct CreateInitTables;

#[async_trait]
impl Migration for CreateInitTables {
    async fn migrate(&self, db: &SqlitePool) -> Result<(), io::Error> {
        let queries = vec![
            "
                CREATE TABLE IF NOT EXISTS ground_station (
                    id              INTEGER PRIMARY KEY AUTOINCREMENT,
                    name            TEXT,
                    latitude        REAL,
                    longitude       REAL,

                    msgs_heard_from INTEGER NOT NULL,
                    msgs_heard_to   INTEGER NOT NULL
                )   
            ",
            "
                CREATE TABLE IF NOT EXISTS ground_station_change_event (
                    id        INTEGER PRIMARY KEY AUTOINCREMENT,
                    gs_id     INTEGER NOT NULL,

                    ts        DATETIME NOT NULL,
                    type      TEXT NOT NULL,
                    old       JSON NOT NULL,
                    new       JSON NOT NULL,

                    FOREIGN KEY(gs_id) REFERENCES ground_station(id)
                )  
            ",
            "
                CREATE TABLE IF NOT EXISTS aircraft (
                    icao         INTEGER PRIMARY KEY AUTOINCREMENT,

                    addr         TEXT NOT NULL,
                    tail         TEXT,
            
                    FOREIGN KEY(gs_id) REFERENCES ground_station(id)
                )  
            ",
            "
                CREATE TABLE IF NOT EXISTS flight (
                    id             INTEGER PRIMARY KEY AUTOINCREMENT,
                    aircraft_icao  INTEGER,
            
                    last_heard     DATETIME NOT NULL,

                    FOREIGN KEY(aircraft_icao) REFERENCES aircraft(icao)
                )
            ",
            "
                CREATE TABLE IF NOT EXISTS aircraft_event (
                    id            INTEGER PRIMARY KEY AUTOINCREMENT,
                    aircraft_icao INTEGER NOT NULL,
                    gs_id         INTEGER NOT NULL,
            
                    ts            DATETIME NOT NULL,
                    freq_mhz      REAL NOT NULL,
                    latitude      REAL NOT NULL,
                    longitude     REAL NOT NULL,
                    altitude      INTEGER,
                
                    FOREIGN KEY(aircraft_icao) REFERENCES aircraft(icao)
                    FOREIGN KEY(gs_id) REFERENCES ground_station(id)
                )    
            ",
            "
                CREATE TABLE IF NOT EXISTS propagation_event (
                    id                INTEGER PRIMARY KEY AUTOINCREMENT,
                    aircraft_event_id INTEGER NOT NULL,
                    gs_id             INTEGER NOT NULL,

                    FOREIGN KEY(aircraft_event_id) REFERENCES aircraft_event(id)
                    FOREIGN KEY(gs_id) REFERENCES ground_station(id)
                )
            ",
            "
                CREATE TABLE IF NOT EXISTS frequency_stat (
                    khz        INTEGER PRIMARY KEY,
                    count      INTEGER NOT NULL,
                    last_heard DATETIME NOT NULL 
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
