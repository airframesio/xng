use clap::command;
use std::process::exit;
use tokio::runtime::Runtime;

mod common;
mod modules;
mod server;

fn main() {
    let manager = modules::ModuleManager::init();

    let cmd = manager.register_arguments(
        command!()
            .propagate_version(true)
            .subcommand_required(true)
            .arg_required_else_help(true)
            .subcommand(server::get_server_arguments()),
    );

    let args = cmd.get_matches();
    let rt = match Runtime::new() {
        Ok(v) => v,
        Err(e) => {
            eprintln!("Failed to start tokio: {}", e.to_string());
            eprintln!("Please report this bug to the developer for further analysis.");
            exit(exitcode::OSERR)
        }
    };

    rt.block_on(async {
        match args.subcommand() {
            Some((server::SERVER_COMMAND, matches)) => server::start(matches).await,
            Some((subcmd, matches)) => manager.start(subcmd, matches).await,
            _ => unreachable!("Encountered None when subcommand_required is true; see clap-rs for documentation changes or bug report link"),
        }
    })
}
