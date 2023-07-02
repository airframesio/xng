use crate::common::{
    arguments::{parse_elastic_index, parse_elastic_url},
    es_utils::{create_es_client, get_xng_index_mapping},
};
use clap::{arg, ArgMatches, Command};
use elasticsearch::indices::{IndicesCreateParts, IndicesDeleteParts, IndicesExistsParts};
use log::*;
use reqwest::{StatusCode, Url};

pub const INIT_ES_COMMAND: &'static str = "init_es";
pub const DELETE_ES_COMMAND: &'static str = "delete_es";

pub fn get_arguments(cmd: &'static str, desc: &'static str) -> Command {
    Command::new(cmd).about(desc).args(&[
        arg!(--elastic <URL> "Export processed common JSON frames to ElasticSearch"),
        arg!(--"elastic-index" <INDEXNAME> "ElasticSearch Index name to use for storing common JSON frames"),
        arg!(--apply "Apply changes to specified ElasticSearch server"),
        arg!(--validate "Validate SSL certificates"),
        arg!(-q --quiet "Silence all output"),
        arg!(-v --verbose ... "Verbose level"),
    ])
}

async fn perform_es_index_action(args: &ArgMatches, delete: bool) {
    let mut elastic_url = if let Some(raw_url) = parse_elastic_url(args) {
        match Url::parse(raw_url) {
            Ok(v) => {
                info!("Target Elasticsearch URL: {}", raw_url);
                v
            }
            Err(e) => {
                error!("Provided Elasticsearch URL is invalid: {}", e.to_string());
                return;
            }
        }
    } else {
        error!("Required Elasticsearch URL argument not found");
        return;
    };
    let elastic_index = parse_elastic_index(args);
    let validate = args.get_flag("validate");
    let apply = args.get_flag("apply");

    let client = match create_es_client(&mut elastic_url, validate) {
        Ok(x) => x,
        Err(e) => {
            error!(
                "Failed to create Elasticsearch client for given URL: {}",
                e.to_string()
            );
            return;
        }
    };

    let exists = match client
        .indices()
        .exists(IndicesExistsParts::Index(&[elastic_index.as_str()]))
        .send()
        .await
    {
        Ok(x) => x,
        Err(e) => {
            error!(
                "Failed to determine existence of index {} on {}: {}",
                elastic_index,
                elastic_url,
                e.to_string()
            );
            return;
        }
    };

    if exists.status_code().is_success() {
        if !delete {
            error!("Index {} already exists, use the delete_es subcommand to delete the index first before rerunning.", elastic_index);
            return;
        }

        if !apply {
            println!(
                "Actions to be performed: delete index {} on Elasticsearch server at {}; rerun with --apply to apply operations",
                elastic_index, elastic_url
            );
            return;
        }

        let delete = match client
            .indices()
            .delete(IndicesDeleteParts::Index(&[elastic_index.as_str()]))
            .send()
            .await
        {
            Ok(x) => x,
            Err(e) => {
                error!(
                    "Failed to send index {} deletion request to {}: {}",
                    elastic_index,
                    elastic_url,
                    e.to_string()
                );
                return;
            }
        };

        if delete.status_code().is_success() {
            println!(
                "Deleted index {} on Elasticsearch server at {}",
                elastic_index, elastic_url
            );
        } else {
            error!("Deletion failed: error code = {:?}", delete.status_code());
        }
    } else if exists.status_code() == StatusCode::NOT_FOUND {
        if delete {
            error!(
                "Index {} does not exist. We cannot delete an index that doesn't exist!",
                elastic_index
            );
            return;
        }

        if !apply {
            println!(
                "Actions to be performed: create and define index {} on Elasticsearch server at {}; rerun with --apply to apply operations", 
                elastic_index, elastic_url
            );
            return;
        }

        let response = match client
            .indices()
            .create(IndicesCreateParts::Index(elastic_index.as_str()))
            .body(get_xng_index_mapping())
            .send()
            .await
        {
            Ok(x) => x,
            Err(e) => {
                error!("{}", e.to_string());
                return;
            }
        };
        if response.status_code().is_success() {
            println!(
                "Created index {} on Elasticsearch server at {}",
                elastic_index, elastic_url
            );
        } else {
            error!("Creation failed: error code = {:?}", response.status_code());
        }
    }
}

pub async fn init_es(args: &ArgMatches) {
    perform_es_index_action(args, false).await
}

pub async fn delete_es(args: &ArgMatches) {
    perform_es_index_action(args, true).await
}
