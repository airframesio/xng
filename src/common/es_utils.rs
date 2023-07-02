use elasticsearch::auth::Credentials;
use elasticsearch::cert::CertificateValidation;
use elasticsearch::http::transport::{SingleNodeConnectionPool, TransportBuilder};
use elasticsearch::Elasticsearch;

use reqwest::Url;
use serde_json::{json, Value};

pub fn create_es_client(
    es_url: &mut Url,
    validate: bool,
) -> Result<Elasticsearch, elasticsearch::Error> {
    let credentials = match (es_url.username(), es_url.password()) {
        ("", _) | (_, None) => None,
        (user, Some(passwd)) => {
            es_url.set_password(None);
            es_url.set_username("");

            Some(Credentials::Basic(user.to_string(), passwd.to_string()))
        }
    };

    let conn_pool = SingleNodeConnectionPool::new(es_url.clone());
    let mut builder = TransportBuilder::new(conn_pool);

    builder = match credentials {
        Some(c) => builder.auth(c).cert_validation(if validate {
            CertificateValidation::Default
        } else {
            CertificateValidation::None
        }),
        None => builder,
    };

    let transport = builder.build()?;
    Ok(Elasticsearch::new(transport))
}

pub fn get_xng_index_mapping() -> Value {
    json!({})
}
