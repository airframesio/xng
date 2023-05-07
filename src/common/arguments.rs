use clap::{arg, Command};

pub fn register_common_arguments(cmd: Command) -> Command {
    cmd.args(&[
        arg!(-q --quiet "Silence all output"),
        arg!(-v --verbose "Verbose mode"),
        arg!(-d --debug "Extra verbose mode for debugging"),
        arg!(--"api-token" <TOKEN> "Sets up an authentication token for API server access"),
        arg!(--"disable-cross-site" "Disable cross site requests"),
        arg!(--"disable-api-control" "Disable controlling of session from API server"),
        arg!(--"listen-host" <HOST> "Host for API server to listen on"),
        arg!(--"listen-port" <PORT> "Port for API server to listen on"),
        arg!(--swarm <URL> "xng server instance to connect to (local API server will be disabled)"),
    ])
}
