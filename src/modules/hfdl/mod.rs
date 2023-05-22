use clap::{arg, Arg, ArgAction, Command};

use super::XngModule;

mod frame;
mod module;

const HFDL_COMMAND: &'static str = "hfdl";

pub struct HfdlModule {
    name: &'static str,
}

impl XngModule for HfdlModule {
    fn get_arguments(&self) -> Command {
        Command::new("hfdl")
            .args(&[
                arg!(--bin <FILE> "Path to dumphfdl binary"),
                arg!(--systable <FILE> "Path to dumphfdl system table configuration"),
                arg!(--"stale-timeout" <SECONDS> "Elapsed time since last update before an aircraft and ground station frequency data is considered stale"),
                arg!(--"session-timeout" <SECONDS> "Elapsed time since last HFDL frame before a session is considered stale and requires switching"),
                arg!(--"use-airframes-gs-map" "Use airframes.io's live HFDL ground station frequency map"),
                arg!(--bandwidth <HERTZ> "Initial bandwidth to use for splitting HFDL spectrum into bands of coverage"),
            ])
            .arg(Arg::new("hfdl-args").action(ArgAction::Append))
    }

    fn id(&self) -> &'static str {
        self.name
    }
}
