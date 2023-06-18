use serde_json::Value;

pub fn validate_listening_bands(_value: &Value) -> Result<(), String> {
    Err(String::from("listening_bands is read-only"))
}
