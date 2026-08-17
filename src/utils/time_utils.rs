use chrono::Utc;

/// Get current timestamp in milliseconds
pub fn get_current_timestamp_ms() -> i64 {
    Utc::now().timestamp_millis()
}

/// Round timestamp down to the nearest interval (e.g., 5 minutes)
pub fn round_timestamp_down(ts_ms: i64, interval_ms: i64) -> i64 {
    (ts_ms / interval_ms) * interval_ms
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_round_timestamp() {
        let ts_ms = 1762050181298; // 2025-11-02 02:23:01.298
        let interval_5min = 5 * 60 * 1000; // 5 minutes in ms
        let rounded = round_timestamp_down(ts_ms, interval_5min);
        assert_eq!(rounded % interval_5min, 0);
    }
}
