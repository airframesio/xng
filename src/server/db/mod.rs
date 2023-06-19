use log::*;
use sqlx::{migrate::MigrateDatabase, Sqlite, SqlitePool};
use tokio::io;

use crate::common::events::GroundStationChangeEvent;
use crate::common::frame::CommonFrame;

use self::migrations as db_migrations;

mod migrations;

pub struct StateDB {
    db: Option<SqlitePool>,
}

impl StateDB {
    pub async fn new(db_url: Option<String>) -> Result<StateDB, io::Error> {
        let Some(db_url) = db_url else {
            return Ok(StateDB { db: None });  
        };
        
        if !Sqlite::database_exists(db_url.as_str())
            .await
            .unwrap_or(false)
        {
            match Sqlite::create_database(db_url.as_str()).await {
                Ok(_) => debug!("State DB created at {}", db_url),
                Err(e) => {
                    return Err(io::Error::new(
                        io::ErrorKind::Other,
                        format!("Failed to create state DB: {}", e.to_string()),
                    ))
                }
            }
        } else {
            debug!("State DB already exists at {}", db_url);
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

        Ok(StateDB { db: Some(db) })
    }

    pub async fn create_ground_station(
        &self,
        id: u32,
        name: &String,
        latitude: f64,
        longitude: f64,
    ) -> Result<(), sqlx::Error> {
        if let Some(ref db) = self.db {
            sqlx::query(
                "
                INSERT INTO ground_stations (id, name, latitude, longitude, msgs_heard_from, msgs_heard_to)
                VALUES (?, ?, ?, ?, 0, 0)
                ON CONFLICT DO NOTHING
                "
            )
            .bind(id)
            .bind(name)
            .bind(latitude)
            .bind(longitude)
            .execute(db)
            .await?;
        }

        Ok(())
    }

    pub fn db_pool(&self) -> Option<&SqlitePool> {
        self.db.as_ref()
    }

    pub async fn handle_gs_change_event(
        &self,
        event: &GroundStationChangeEvent,
    ) -> Result<(), sqlx::Error> {
        if let Some(ref db) = self.db {
            let gs_id = if let Some(num) = event.id.as_u64() {
                num
            } else if let Some(val) = event.id.as_str() {
                match u64::from_str_radix(val, 16) {
                    Ok(x) => x,
                    Err(e) => {
                        return Err(sqlx::Error::TypeNotFound {
                            type_name: format!("String is not hexidecimal: {}", e.to_string()),
                        });
                    }
                }
            } else {
                return Err(sqlx::Error::TypeNotFound {
                    type_name: String::from("Expecting string or number, got something else."),
                });
            };

            sqlx::query(
                "
                INSERT INTO ground_station_change_events (gs_id, ts, type, old, new) VALUES (?, ?, \"freq_change\", ?, ?)        
                "
            )
            .bind(gs_id as u32)
            .bind(&event.ts)
            .bind(&event.old)
            .bind(&event.new)
            .execute(db)
            .await?;
        }

        Ok(())
    }

    pub async fn update(&self, frame: &CommonFrame) -> Result<(), sqlx::Error> {
        if let Some(ref db) = self.db {
            let (aircraft, ground_station, from_ground_station) = if frame.src.is_ground_station() {
                (frame.dst.as_ref(), Some(&frame.src), true)
            } else {
                (Some(&frame.src), frame.dst.as_ref(), false)
            };
            let ground_station = ground_station.unwrap();
            let Some(gs_id) = ground_station.id else {
                return Err(sqlx::Error::TypeNotFound { type_name: String::from("Unexpected ground station with no ID") })    
            };

            sqlx::query(
                "
                INSERT INTO frequency_stats (khz, gs_id, count, last_heard) VALUES (?, ?, 1, ?) 
                ON CONFLICT (khz) DO UPDATE SET count = count + 1
                ",
            )
            .bind((frame.freq * 1000.0) as u32)
            .bind(gs_id)
            .bind(&frame.timestamp)
            .execute(db)
            .await?;

            if from_ground_station {
                sqlx::query(
                    "
                    UPDATE ground_stations SET msgs_heard_from = msgs_heard_from + 1 WHERE id = ?  
                    ",
                )
                .bind(gs_id)
                .execute(db)
                .await?;
            } else {
                sqlx::query(
                    "
                    UPDATE ground_stations SET msgs_heard_to = msgs_heard_to + 1 WHERE id = ?  
                    ",
                )
                .bind(gs_id)
                .execute(db)
                .await?;
            }

            if let Some(aircraft) = aircraft {
                let icao_addr = aircraft.icao.clone();
                let icao_id = if let Some(ref addr) = icao_addr {
                    match u32::from_str_radix(addr.as_str(), 16) {
                        Ok(v) => Some(v),
                        Err(e) => {
                            debug!(
                                "Failed to convert ICAO hex to number for {}: {}",
                                addr,
                                e.to_string()
                            );
                            None
                        }
                    }
                } else {
                    None
                };

                if icao_addr.is_some() && icao_id.is_some() {
                    sqlx::query(
                        "
                        INSERT INTO aircrafts (icao, addr, tail, msg_count) VALUES (?, ?, ?, 1)
                        ON CONFLICT (icao) DO UPDATE SET tail = ?, msg_count = msg_count + 1
                        ",
                    )
                    .bind(icao_id.unwrap())
                    .bind(icao_addr.unwrap())
                    .bind(&aircraft.tail)
                    .bind(&aircraft.tail)
                    .execute(db)
                    .await?;
                }

                if let Some(ref coords) = aircraft.coords {
                    let result = sqlx::query(
                        "
                        INSERT INTO aircraft_events (aircraft_icao, gs_id, callsign, tail, ts, signal, freq_mhz, latitude, longitude, altitude)
                        VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
                        "
                    )
                    .bind(icao_id)
                    .bind(gs_id)
                    .bind(&aircraft.callsign)
                    .bind(&aircraft.tail)
                    .bind(&frame.indexed.timestamp)
                    .bind(frame.signal)
                    .bind(frame.freq)
                    .bind(coords.y)
                    .bind(coords.x)
                    .bind(coords.z)
                    .execute(db)
                    .await?;

                    let aircraft_event_id = result.last_insert_rowid();

                    for path in frame.paths.iter() {
                        if let Some(gs_id) = path.party.id {
                            sqlx::query(
                                "
                                INSERT INTO propagation_events (aircraft_events_id, gs_id) VALUES (?, ?)
                                ON CONFLICT DO NOTHING
                                "
                            )
                            .bind(aircraft_event_id)
                            .bind(gs_id)
                            .execute(db)
                            .await?;
                        }
                    }
                }
            }
        }
        Ok(())
    }
}
