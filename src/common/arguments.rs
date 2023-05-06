use clap::{arg, Command};

pub fn register_common_arguments(cmd: Command) -> Command {
    cmd.args(&[
        arg!(-q "Silence all output"),
        arg!(-v "Verbose mode"),
        arg!(-vv "Extra verbose mode for debugging"),
        arg!(--feed-airframes "Feed JSON frames to airframes.io"),
        arg!(--station-name <NAME> "Sets up a station name for feeding to airframes.io"),
        arg!(--api-token <TOKEN> "Sets up an authentication token for API server access"),
        arg!(--disable-cross-site "Disable cross site requests"),
        arg!(--disable-api-control "Disable controlling of session from API server"),
        arg!(--disable-print-frame "Disable printing JSON frames to STDOUT"),
        arg!(--session-intermission <SECONDS> "Time to wait between sessions"),
        arg!(--listen-host <HOST> "Host for API server to listen on"),
        arg!(--listen-port <PORT> "Port for API server to listen on"),
        arg!(--swarm <URL> "xng server instance to connect to (local API server will be disabled)"),
    ])
}
