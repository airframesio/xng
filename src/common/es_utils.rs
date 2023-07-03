use elasticsearch::auth::Credentials;
use elasticsearch::cert::CertificateValidation;
use elasticsearch::http::transport::{SingleNodeConnectionPool, TransportBuilder};
use elasticsearch::{BulkOperation, Elasticsearch};

use log::{debug, warn};
use reqwest::Url;
use serde_json::{json, Value};

use super::frame::CommonFrame;

pub fn create_es_client(
    es_url: &mut Url,
    validate: bool,
) -> Result<Elasticsearch, elasticsearch::Error> {
    let credentials = match (es_url.username(), es_url.password()) {
        ("", _) | (_, None) => None,
        (user, Some(passwd)) => Some(Credentials::Basic(user.to_string(), passwd.to_string())),
    };

    let conn_pool = SingleNodeConnectionPool::new(es_url.clone());
    let mut builder = TransportBuilder::new(conn_pool);

    builder = match credentials {
        Some(c) => {
            #[allow(unused_must_use)]
            {
                es_url.set_username("");
                es_url.set_password(None);
            }

            builder.auth(c).cert_validation(if validate {
                CertificateValidation::Default
            } else {
                CertificateValidation::None
            })
        }
        None => builder,
    };

    let transport = builder.build()?;
    Ok(Elasticsearch::new(transport))
}

pub async fn bulk_index(
    client: &Elasticsearch,
    index: &String,
    frames: &Vec<CommonFrame>,
) -> Result<(), elasticsearch::Error> {
    let body: Vec<BulkOperation<_>> = frames
        .iter()
        .map(|p| BulkOperation::index(p).into())
        .collect();
    debug!("{:?}", frames);

    let response = client
        .bulk(elasticsearch::BulkParts::Index(index.as_str()))
        .body(body)
        .send()
        .await?;

    let json: Value = response.json().await?;

    if json["errors"].as_bool().unwrap() {
        let failed: Vec<&Value> = json["items"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|v| !v["error"].is_null())
            .collect();
        // TODO: what to do here?
        warn!("Failed to index {} docs!", failed.len());
    }

    Ok(())
}

pub fn get_xng_index_mapping() -> Value {
    json!({
        "mappings": {
            "dynamic": "true",
            "date_detection": false,
            "numeric_detection": false,
            "dynamic_templates": [
                {
                    "frequency": {
                        "match_mapping_type": "double",
                        "match_pattern": "regex",
                        "match": "^freq[s]{0,1}$",
                        "mapping": {
                            "type": "double"
                        }
                    }
                },
                {
                    "coords": {
                        "match_mapping_type": "string",
                        "match": "coords",
                        "mapping": {
                            "type": "geo_point"
                        }
                    },
                },
                {
                    "polylines": {
                        "match_mapping_type": "string",
                        "match": "path",
                        "mapping": {
                            "type": "geo_shape"
                        }
                    }
                },
                {
                    "timestamps": {
                        "match_mapping_type": "string",
                        "match": "*timestamp",
                        "mapping": {
                            "type": "date",
                            "format": "strict_date_optional_time_nanos"
                        }
                    }
                }
            ]
        }
    })
}
