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

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    #[test]
    fn test_split_unix_time_to_utc_datetime() {
        // Test epoch (Jan 1, 1970 00:00:00 UTC)
        let dt = split_unix_time_to_utc_datetime(0, 0).unwrap();
        assert_eq!(dt.timestamp(), 0);
        assert_eq!(dt.timestamp_subsec_nanos(), 0);
    }

    #[test]
    fn test_split_unix_time_to_utc_datetime_with_nanos() {
        // Test with nanoseconds
        let dt = split_unix_time_to_utc_datetime(1000000, 500000000).unwrap();
        assert_eq!(dt.timestamp(), 1000000);
        assert_eq!(dt.timestamp_subsec_nanos(), 500000000);
    }

    #[test]
    fn test_unix_time_to_utc_datetime() {
        // Test epoch
        let dt = unix_time_to_utc_datetime(0.0).unwrap();
        assert_eq!(dt.timestamp(), 0);
    }

    #[test]
    fn test_unix_time_to_utc_datetime_with_fraction() {
        // Test with fractional seconds (1.5 seconds)
        let dt = unix_time_to_utc_datetime(1.5).unwrap();
        assert_eq!(dt.timestamp(), 1);
        assert_eq!(dt.timestamp_subsec_nanos(), 500000000);
    }

    #[test]
    fn test_unix_time_to_utc_datetime_typical_timestamp() {
        // Test with typical timestamp (Oct 21, 2025)
        let epoch = 1729468800.0; // Approximately Oct 21, 2025
        let dt = unix_time_to_utc_datetime(epoch).unwrap();
        assert_eq!(dt.timestamp(), 1729468800);
    }

    #[test]
    fn test_nearest_time_in_past_same_day() {
        // Create a datetime for Oct 21, 2025 15:30:00
        let naive_dt = NaiveDate::from_ymd_opt(2025, 10, 21)
            .unwrap()
            .and_hms_opt(15, 30, 0)
            .unwrap();
        let dt = UTC.from_local_datetime(&naive_dt).unwrap();

        // Find nearest 9:00:00 in the past (should be same day)
        let past = nearest_time_in_past(&dt, 9, 0, 0).unwrap();

        assert_eq!(past.day(), 21);
        assert_eq!(past.hour(), 9);
        assert_eq!(past.minute(), 0);
        assert_eq!(past.second(), 0);
        assert!(past < dt);
    }

    #[test]
    fn test_nearest_time_in_past_previous_day() {
        // Create a datetime for Oct 21, 2025 08:30:00
        let naive_dt = NaiveDate::from_ymd_opt(2025, 10, 21)
            .unwrap()
            .and_hms_opt(8, 30, 0)
            .unwrap();
        let dt = UTC.from_local_datetime(&naive_dt).unwrap();

        // Find nearest 9:00:00 in the past (should be previous day)
        let past = nearest_time_in_past(&dt, 9, 0, 0).unwrap();

        assert_eq!(past.day(), 20);
        assert_eq!(past.hour(), 9);
        assert_eq!(past.minute(), 0);
        assert_eq!(past.second(), 0);
        assert!(past < dt);
    }

    #[test]
    fn test_nearest_time_in_past_at_exact_time() {
        // Create a datetime for Oct 21, 2025 09:00:00
        let naive_dt = NaiveDate::from_ymd_opt(2025, 10, 21)
            .unwrap()
            .and_hms_opt(9, 0, 0)
            .unwrap();
        let dt = UTC.from_local_datetime(&naive_dt).unwrap();

        // Find nearest 9:00:00 in the past (should be same time, since it's not greater)
        let past = nearest_time_in_past(&dt, 9, 0, 0).unwrap();

        assert_eq!(past.day(), 21);
        assert_eq!(past.hour(), 9);
        assert!(past <= dt);
    }

    #[test]
    fn test_nearest_time_in_past_midnight() {
        // Create a datetime for Oct 21, 2025 01:00:00
        let naive_dt = NaiveDate::from_ymd_opt(2025, 10, 21)
            .unwrap()
            .and_hms_opt(1, 0, 0)
            .unwrap();
        let dt = UTC.from_local_datetime(&naive_dt).unwrap();

        // Find nearest midnight (00:00:00) in the past
        let past = nearest_time_in_past(&dt, 0, 0, 0).unwrap();

        assert_eq!(past.day(), 21);
        assert_eq!(past.hour(), 0);
        assert_eq!(past.minute(), 0);
        assert_eq!(past.second(), 0);
        assert!(past < dt);
    }
}
