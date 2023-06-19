use log::*;
use serde_json::Value;
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
    pub async fn new(db_url: String) -> Result<StateDB, io::Error> {
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
                INSERT INTO ground_station (id, name, latitude, longitude, msgs_heard_from, msgs_heard_to)
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
                INSERT INTO ground_station_change_event (gs_id, ts, type, old, new) VALUES (?, ?, \"freq_change\", ?, ?)        
                "
            )
            .bind(gs_id as u32)
            .bind(event.ts)
            .bind(&event.old)
            .bind(&event.new)
            .execute(db)
            .await?;
        }

        Ok(())
    }

    pub async fn update(&self, frame: &CommonFrame) -> Result<(), sqlx::Error> {
        if let Some(ref db) = self.db {
            sqlx::query(
                "
            INSERT INTO frequency_stat (khz, count, last_heard) VALUES (?, 1, ?) 
            ON CONFLICT (khz) DO UPDATE SET count=count+1
            ",
            )
            .bind((frame.freq * 1000.0) as u32)
            .bind(&frame.timestamp)
            .execute(db)
            .await?;
        }
        Ok(())
    }
}
