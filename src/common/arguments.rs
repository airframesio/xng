use clap::{arg, Command};

pub fn register_common_arguments(cmd: Command) -> Command {
    cmd.args(&[
        arg!(-q --quiet "Silence all output"),
        arg!(-v --verbose ... "Verbose level"),
        arg!(--"api-token" <TOKEN> "Sets up an authentication token for API server access"),
        arg!(--"disable-cross-site" "Disable cross site requests"),
        arg!(--"listen-host" <HOST> "Host for API server to listen on"),
        arg!(--"listen-port" <PORT> "Port for API server to listen on"),
        arg!(--elastic <URL> "Export processed common JSON frames to ElasticSearch"),
        arg!(--"state-db" <URL> "SQLite3 database to store state metrics. URL should begin with sqlite://"),
    ])
}
