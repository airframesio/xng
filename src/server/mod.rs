use clap::{arg, ArgMatches, Command};

use crate::common;

pub const SERVER_COMMAND: &'static str = "server";

pub fn get_server_arguments() -> Command {
    common::arguments::register_common_arguments(
        Command::new(SERVER_COMMAND)
            .about("")
            .args(&[arg!(--elastic <URL> "Export processed common JSON frames to ElasticSearch")]),
    )
}

pub async fn start(args: &ArgMatches) {
    todo!()
}
