use chrono::{DateTime, Days, NaiveDateTime, TimeZone};
use chrono_tz::{Tz, UTC};

pub fn split_unix_time_to_utc_datetime(secs: i64, nsecs: u32) -> Option<DateTime<Tz>> {
    let local_dt = NaiveDateTime::from_timestamp_opt(secs, nsecs)?;

    UTC.from_local_datetime(&local_dt).latest()
}

pub fn unix_time_to_utc_datetime(epoch: f64) -> Option<DateTime<Tz>> {
    let secs = f64::trunc(epoch) as i64;
    let nsecs = ((epoch - f64::trunc(epoch)) * 1_000_000_000_f64) as u32;

    split_unix_time_to_utc_datetime(secs, nsecs)
}

pub fn nearest_time_in_past(dt: &DateTime<Tz>, hour: u8, min: u8, sec: u8) -> Option<DateTime<Tz>> {
    let past_naive_date = dt
        .date_naive()
        .and_hms_opt(hour as u32, min as u32, sec as u32)?;
    let mut past_utc_date = UTC.from_local_datetime(&past_naive_date).latest()?;

    if past_utc_date > *dt {
        past_utc_date = past_utc_date.checked_sub_days(Days::new(1))?;
    }

    Some(past_utc_date)
}
