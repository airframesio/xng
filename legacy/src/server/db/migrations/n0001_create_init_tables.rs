use async_trait::async_trait;
use sqlx::SqlitePool;
use tokio::io;

use super::Migration;

pub struct CreateInitTables;

#[async_trait]
impl Migration for CreateInitTables {
    async fn migrate(&self, db: &SqlitePool) -> Result<(), io::Error> {
        let queries = [
            "
                CREATE TABLE IF NOT EXISTS ground_stations (
                    id              INTEGER PRIMARY KEY,
                    name            TEXT,
                    latitude        REAL,
                    longitude       REAL,

                    msgs_heard_from INTEGER NOT NULL,
                    msgs_heard_to   INTEGER NOT NULL
                )   
            ",
            "
                CREATE TABLE IF NOT EXISTS ground_station_change_events (
                    id        INTEGER PRIMARY KEY AUTOINCREMENT,
                    gs_id     INTEGER NOT NULL,

                    ts        DATETIME NOT NULL,
                    type      TEXT NOT NULL,
                    old       TEXT NOT NULL,
                    new       TEXT NOT NULL,

                    FOREIGN KEY(gs_id) REFERENCES ground_stations(id)
                )  
            ",
            "
                CREATE TABLE IF NOT EXISTS aircrafts (
                    icao         INTEGER PRIMARY KEY,

                    addr         TEXT NOT NULL,
                    tail         TEXT,

                    msg_count    INTEGER NOT NULL
                )  
            ",
            "
                CREATE TABLE IF NOT EXISTS aircraft_events (
                    id            INTEGER PRIMARY KEY AUTOINCREMENT,
                    aircraft_icao INTEGER,
                    gs_id         INTEGER NOT NULL,

                    callsign      TEXT,
                    tail          TEXT,
            
                    ts            DATETIME NOT NULL,
                    signal        REAL NOT NULL,
                    freq_mhz      REAL NOT NULL,
                    latitude      REAL NOT NULL,
                    longitude     REAL NOT NULL,
                    altitude      INTEGER,
                
                    FOREIGN KEY(aircraft_icao) REFERENCES aircrafts(icao) ON DELETE CASCADE
                    FOREIGN KEY(gs_id) REFERENCES ground_stations(id)
                )    
            ",
            "
                CREATE TABLE IF NOT EXISTS propagation_events (
                    id                 INTEGER PRIMARY KEY AUTOINCREMENT,
                    aircraft_events_id INTEGER NOT NULL,
                    gs_id              INTEGER NOT NULL,

                    FOREIGN KEY(aircraft_events_id) REFERENCES aircraft_events(id) ON DELETE CASCADE
                    FOREIGN KEY(gs_id) REFERENCES ground_stations(id)
                    UNIQUE(aircraft_events_id, gs_id)
                )
            ",
            "
                CREATE TABLE IF NOT EXISTS frequency_stats (
                    khz        INTEGER PRIMARY KEY,
                    gs_id      INTEGER NOT NULL,
            
                    to_gs      INTEGER NOT NULL,
                    from_gs    INTEGER NOT NULL,
                    last_heard DATETIME NOT NULL,

                    UNIQUE(khz, gs_id)
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
