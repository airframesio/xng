use serde_json::Value;

pub fn valid_session_method(value: &Value) -> Result<(), String> {
    let Some(method) = value.as_str() else {
        return Err(format!("Expected string"));  
    };

    match method.to_lowercase().as_str() {
        "random" | "static" | "inc" | "dec" => Ok(()),
        _ => Err(format!("Unknown method type")),
    }
}
