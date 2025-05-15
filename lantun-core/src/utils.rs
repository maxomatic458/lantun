use std::time::{SystemTime, UNIX_EPOCH};

pub fn now_secs() -> u64 {
    let now = SystemTime::now();
    let duration = now.duration_since(UNIX_EPOCH).unwrap();
    duration.as_secs()
}
