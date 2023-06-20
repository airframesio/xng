use clap::{arg, ArgMatches, Command};

pub fn register_common_arguments(cmd: Command) -> Command {
    cmd.args(&[
        arg!(-q --quiet "Silence all output"),
        arg!(-v --verbose ... "Verbose level"),
        arg!(--"api-token" <TOKEN> "Sets up an authentication token for API server access"),
        arg!(--"disable-cross-site" "Disable cross site requests"),
        arg!(--"listen-host" <HOST> "Host for API server to listen on"),
        arg!(--"listen-port" <PORT> "Port for API server to listen on"),
        arg!(--elastic <URL> "[TODO] Export processed common JSON frames to ElasticSearch"),
        arg!(--"state-db" <URL> "SQLite3 database to store state metrics. URL should begin with sqlite://"),
        arg!(--"disable-state-db" "Disables SQLite3 database to store state metrics."),
    ])
}

pub fn parse_api_token(args: &ArgMatches) -> Option<&String> {
    args.get_one::<String>("api-token")
}

pub fn parse_disable_cross_site(args: &ArgMatches) -> bool {
    args.get_flag("disable-cross-site")
}

pub fn parse_listen_host(args: &ArgMatches, default_host: &str) -> String {
    args.get_one::<String>("listen-host")
        .unwrap_or(&default_host.to_string())
        .to_owned()
}

pub fn parse_listen_port(args: &ArgMatches, default_port: u16) -> u16 {
    args.get_one::<String>("listen-port")
        .unwrap_or(&String::from("default"))
        .parse::<u16>()
        .unwrap_or(default_port)
}

pub fn parse_elastic_url(args: &ArgMatches) -> Option<&String> {
    args.get_one::<String>("elastic")
}

pub fn parse_state_db_url(args: &ArgMatches, default_url: &str) -> String {
    args.get_one::<String>("state-db")
        .unwrap_or(&String::from(default_url))
        .to_owned()
}

pub fn parse_disable_state_db(args: &ArgMatches) -> bool {
    args.get_flag("disable-state-db")
}
