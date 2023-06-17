use serde_json::Value;

pub fn validate_listening_bands(value: &Value) -> Result<(), String> {
    Err(String::from("listening_bands is read-only"))
}

pub fn validate_session_method(value: &Value) -> Result<(), String> {
    let Some(method) = value.as_str() else {
        return Err(format!("Expected string"));  
    };

    match method.to_lowercase().as_str() {
        "random" | "static" | "inc" | "dec" => Ok(()),
        _ => Err(format!("Unknown method type")),
    }
}

pub fn validate_next_session_band(value: &Value) -> Result<(), String> {
    let Some(band) = value.as_u64() else {
        return Err(format!("Expected positive kHz frequency"));
    };

    match band {
        0 | 2000..=21997 => Ok(()),
        _ => Err(format!("Invalid kHz range: {}", band)),
    }
}
