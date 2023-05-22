use self::systable::SystemTable;
use super::XngModule;
use clap::{arg, Arg, ArgAction, ArgMatches, Command};
use std::{io, path::PathBuf};

mod frame;
mod module;
mod systable;

const DEFAULT_BIN_PATH: &'static str = "/usr/bin/dumphfdl";
const DEFAULT_SYSTABLE_PATH: &'static str = "/etc/systable.conf";

const DEFAULT_STALE_TIMEOUT_SECS: u64 = 600;

const HFDL_COMMAND: &'static str = "hfdl";

#[derive(Default)]
pub struct HfdlModule {
    name: &'static str,

    bin: PathBuf,
    systable: SystemTable,
}

impl XngModule for HfdlModule {
    fn get_arguments(&self) -> Command {
        Command::new("hfdl")
            .args(&[
                arg!(--bin <FILE> "Path to dumphfdl binary"),
                arg!(--systable <FILE> "Path to dumphfdl system table configuration"),
                arg!(--"stale-timeout" <SECONDS> "Elapsed time since last update before an aircraft and ground station frequency data is considered stale"),
                arg!(--"session-timeout" <SECONDS> "Elapsed time since last HFDL frame before a session is considered stale and requires switching"),
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

        let stale_timeout_secs = args
            .get_one("stale-timeout")
            .unwrap_or(&"default")
            .parse::<u64>()
            .unwrap_or(DEFAULT_STALE_TIMEOUT_SECS);

        let use_airframes_gs_map = args.get_flag("use-airframes-gs-map");

        Ok(())
    }

    fn id(&self) -> &'static str {
        self.name
    }
}
