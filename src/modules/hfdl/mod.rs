use self::systable::SystemTable;
use super::XngModule;
use actix_web::{web, Resource};
use clap::{arg, Arg, ArgAction, ArgMatches, Command};
use std::{io, path::PathBuf};

mod frame;
mod module;
mod systable;

const DEFAULT_BIN_PATH: &'static str = "/usr/bin/dumphfdl";
const DEFAULT_SYSTABLE_PATH: &'static str = "/etc/systable.conf";

const DEFAULT_STALE_TIMEOUT_SECS: u64 = 1800;
const DEFAULT_SESSION_TIMEOUT_SECS: u64 = 600;

const HFDL_COMMAND: &'static str = "hfdl";

#[derive(Default)]
pub struct HfdlModule {
    name: &'static str,

    bin: PathBuf,
    systable: SystemTable,
}

impl XngModule for HfdlModule {
    fn id(&self) -> &'static str {
        self.name
    }

    fn default_session_timeout_secs(&self) -> u64 {
        DEFAULT_SESSION_TIMEOUT_SECS
    }

    fn get_arguments(&self) -> Command {
        Command::new("hfdl")
            .args(&[
                arg!(--bin <FILE> "Path to dumphfdl binary"),
                arg!(--systable <FILE> "Path to dumphfdl system table configuration"),
                arg!(--"stale-timeout" <SECONDS> "Elapsed time since last update before an aircraft and ground station frequency data is considered stale"),
                arg!(--bandwidth <HERTZ> "Initial bandwidth to use for splitting HFDL spectrum into bands of coverage"),
                arg!(--"use-airframes-gs-map" "Use airframes.io's live HFDL ground station frequency map"),
            ])
            .arg(Arg::new("hfdl-args").action(ArgAction::Append))
    }

    fn parse_arguments(&mut self, args: &ArgMatches) -> Result<(), io::Error> {
        let bin_path = PathBuf::from(
            args.get_one::<String>("bin")
                .unwrap_or(&DEFAULT_BIN_PATH.to_string()),
        );
        if !bin_path.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!(
                    "Provided dumphfdl path is not a file: {}",
                    bin_path.to_string_lossy()
                ),
            ));
        }
        self.bin = bin_path;

        self.systable = SystemTable::load(&PathBuf::from(
            args.get_one::<String>("systable")
                .unwrap_or(&DEFAULT_SYSTABLE_PATH.to_string()),
        ))?;

        // TODO: parse for --soapy in hfdl-args so we can extract driver=

        let stale_timeout_secs = args
            .get_one("stale-timeout")
            .unwrap_or(&"default")
            .parse::<u64>()
            .unwrap_or(DEFAULT_STALE_TIMEOUT_SECS);

        // TODO: replace 1000 with a default value obtained via rust-soapy
        let bandwidth = args
            .get_one("bandwidth")
            .unwrap_or(&"default")
            .parse::<u64>()
            .unwrap_or(1000);

        let use_airframes_gs_map = args.get_flag("use-airframes-gs-map");

        Ok(())
    }
}
