use serde_json::Value;

#[derive(Clone)]
pub struct GroundStationChangeEvent {
    pub ts: String,
    pub id: Value,
    pub name: Option<String>,
    pub old: String,
    pub new: String,
}

impl GroundStationChangeEvent {
    pub fn pretty_id(&self) -> String {
        match &self.id {
            Value::Number(x) => x.to_string(),
            Value::String(x) => x.to_owned(),
            _ => String::from("No ID"),
        }
    }

    pub fn pretty_name(&self) -> String {
        self.name.clone().unwrap_or(String::from("No Name"))
    }
}
