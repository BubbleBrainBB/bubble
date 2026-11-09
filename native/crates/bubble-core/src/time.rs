//! Wall-clock helpers without a date library.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

pub struct Now {
    /// `2026-11-08T13:45:01.123Z`
    pub iso: String,
    /// `2026-11-08`
    pub date: String,
    pub millis: u128,
}

/// Days since 1970-01-01 to a civil date (Howard Hinnant's algorithm).
fn civil(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (yoe + era * 400 + i64::from(month <= 2), month, day)
}

pub fn from_millis(millis: u128) -> Now {
    let secs = (millis / 1000) as i64;
    let (year, month, day) = civil(secs.div_euclid(86_400));
    let rem = secs.rem_euclid(86_400);
    let date = format!("{year:04}-{month:02}-{day:02}");
    let iso = format!("{date}T{:02}:{:02}:{:02}.{:03}Z", rem / 3600, rem % 3600 / 60, rem % 60, millis % 1000);
    Now { iso, date, millis }
}

pub fn now_utc() -> Now {
    from_millis(SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or_default())
}

/// A process-unique id: time plus a counter.
pub fn unique_id() -> String {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    format!("{:x}-{:x}-{:x}", now_utc().millis, std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed))
}

#[cfg(test)]
mod tests {
    #[test]
    fn dates_are_civil() {
        assert_eq!(super::from_millis(0).iso, "1970-01-01T00:00:00.000Z");
        assert_eq!(super::from_millis(1_794_163_233_079).iso, "2026-11-08T18:40:33.079Z");
        assert_eq!(super::from_millis(951_782_400_000).date, "2000-02-29");
    }
}
