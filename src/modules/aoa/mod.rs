use actix_web::web::Data;
use async_trait::async_trait;
use clap::{arg, Arg, ArgAction, ArgMatches, Command};
use tokio::{io, sync::RwLock};

use super::session::EndSessionReason;
use super::settings::ModuleSettings;
use super::XngModule;
use crate::server::db::StateDB;

mod ground_station_db;
mod module;

const AOA_COMMAND: &'static str = "aoa";

const DEFAULT_BIN_PATH: &'static str = "/usr/bin/dumpvdl2";
const DEFAULT_SESSION_TIMEOUT_SECS: u64 = 900;

#[derive(Default)]
pub struct AoaModule {
    name: &'static str,
}

#[async_trait]
impl XngModule for AoaModule {
    fn id(&self) -> &'static str {
        self.name
    }

    fn default_session_timeout_secs(&self) -> u64 {
        DEFAULT_SESSION_TIMEOUT_SECS
    }

    fn get_arguments(&self) -> Command {
        Command::new(AOA_COMMAND)
            .about("Listen to ACARS-Over-AVLC messages using dumpvdl2")
            .args(&[
                arg!(--"ground-stations" <FILE> "Path to VDL2 Ground Stations text file from acars-vdl2 Groups.io mailing list")
            ])
            .arg(Arg::new("aoa-args").action(ArgAction::Append))
    }

    fn parse_arguments(&mut self, args: &ArgMatches) -> Result<(), io::Error> {
        todo!()
    }

    async fn init(
        &mut self,
        settings: Data<RwLock<ModuleSettings>>,
        state_db: Data<RwLock<StateDB>>,
    ) {
        todo!()
    }

    async fn start_session(
        &mut self,
        last_end_reason: EndSessionReason,
    ) -> Result<Box<dyn super::session::Session>, io::Error> {
        todo!()
    }

    async fn process_message(
        &mut self,
        current_band: &Vec<u16>,
        msg: &str,
    ) -> Result<crate::common::frame::CommonFrame, io::Error> {
        todo!()
    }
}
