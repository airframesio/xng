use clap::command;
use modules::elasticsearch;
use std::process::exit;
use tokio::runtime::Runtime;

mod common;
mod modules;
mod server;
mod utils;

fn main() {
    let mut manager = modules::ModuleManager::init();

    let cmd = manager.register_arguments(
        command!()
            .propagate_version(true)
            .subcommand_required(true)
            .arg_required_else_help(true)
            .subcommand(server::get_server_arguments()),
    )
        .subcommands([
            elasticsearch::get_arguments(elasticsearch::INIT_ES_COMMAND, "Initialize ElasticSearch indices"),
            elasticsearch::get_arguments(elasticsearch::DELETE_ES_COMMAND, "Delete ElasticSearch indices"),
        ]);

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
            Some((subcmd, matches)) => {
                let verbose_level = *matches.get_one::<u8>("verbose").unwrap_or(&0);
                
                stderrlog::new()
                    .module(module_path!())
                    .quiet(matches.get_flag("quiet"))
                    .verbosity((verbose_level as usize) + 1)
                    .timestamp(
                        if verbose_level > 1 { 
                            stderrlog::Timestamp::Second 
                        } else { 
                            stderrlog::Timestamp::Off 
                        }
                    )
                    .init()
                    .unwrap();

                match subcmd {
                    server::SERVER_COMMAND => server::start(matches).await,
                    elasticsearch::INIT_ES_COMMAND => elasticsearch::init_es(matches).await,
                    elasticsearch::DELETE_ES_COMMAND => elasticsearch::delete_es(matches).await,
                    _ => manager.start(subcmd, matches).await,
                }  
            },
            _ => unreachable!("Encountered None when subcommand_required is true; see clap-rs for documentation changes or bug report link"),
        }
    })
}
