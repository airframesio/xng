use serde_json::Value;

pub fn validate_listening_bands(value: &Value) -> Result<(), String> {
    Err(String::from("listening_bands is read-only"))
}
