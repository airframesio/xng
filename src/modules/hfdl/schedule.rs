use chrono::offset::{Local, TimeZone};
use chrono::{DateTime, Days};

use lazy_static::lazy_static;
use regex::Regex;
use serde_json::Value;

pub fn parse_session_schedule(value: &str) -> Result<Vec<(DateTime<Local>, u32)>, String> {
    lazy_static! {
        static ref SCHEDULE_ENTRY_FMT: Regex =
            Regex::new(r"time=([0-9]|[01][1-9]|2[0-3]):([0-5][0-9]),band_contains=([0-9]{4,5})")
                .unwrap();
    }

    let mut schedule: Vec<(DateTime<Local>, u32)> = Vec::new();
    let now = Local::now();

    for token in value.split(";") {
        let Some(m) = SCHEDULE_ENTRY_FMT.captures(token) else {
            return Err(format!("Bad schedule entry format: {}", value));
        };

        let Ok(hour) = m.get(1).map_or("", |x| x.as_str()).parse::<u8>() else {
            return Err(format!("Bad hour format: {}", token));   
        };
        let Ok(min) = m.get(2).map_or("", |x| x.as_str()).parse::<u8>() else {
            return Err(format!("Bad minute format: {}", token));  
        };
        let Ok(freq) = m.get(3).map_or("", |x| x.as_str()).parse::<u16>() else {
            return Err(format!("Bad target frequency: {}", token));
        };

        let Some(naive_dt) = now.date_naive().and_hms_opt(hour as u32, min as u32, 0) else {
            return Err(format!("Bad naive datetime: {}", token));
        };
        let Some(mut dt) = Local.from_local_datetime(&naive_dt).latest() else {
            return Err(format!("Bad naive datetime to TZ-aware datetime converstion: {}", token));  
        };

        if dt < now {
            dt = match dt.checked_add_days(Days::new(1)) {
                Some(t) => t,
                None => return Err(format!("Failed to find next session switch for: {}", token)),
            }
        }

        schedule.push((dt, freq as u32));
    }

    schedule.sort_by(|a, b| a.cmp(b));
    schedule.dedup_by(|a, b| a.0 == b.0);

    Ok(schedule)
}

pub fn valid_session_schedule(value: &Value) -> Result<(), String> {
    let Some(value) = value.as_str() else {
        return Err(format!("Schedule is not a string: {:?}", value));
    };

    parse_session_schedule(value).map(|_| ())
}
