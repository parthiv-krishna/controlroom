//! Ids and timestamps in pi's formats.

use std::time::{SystemTime, UNIX_EPOCH};

/// Current unix time in milliseconds.
pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

fn random() -> u128 {
    ulid::Ulid::generate().0
}

/// A random UUID-shaped id (session ids, dialog ids).
pub fn uuid() -> String {
    let hex = format!("{:032x}", random());
    format!(
        "{}-{}-{}-{}-{}",
        &hex[..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..]
    )
}

/// An 8-hex-digit session entry id.
pub fn entry_id() -> String {
    // The low bits of a ULID are random.
    format!("{:08x}", random() as u32)
}

/// `ms` as an ISO 8601 UTC timestamp (`2024-01-02T03:04:05.006Z`).
pub fn iso(ms: u64) -> String {
    let (date, time) = parts(ms);
    format!("{date}T{}:{}:{}.{}Z", time.0, time.1, time.2, time.3)
}

/// `ms` in pi's session file name format (`2024-01-02T03-04-05-006Z`).
pub fn file_stamp(ms: u64) -> String {
    let (date, time) = parts(ms);
    format!("{date}T{}-{}-{}-{}Z", time.0, time.1, time.2, time.3)
}

type Time = (String, String, String, String);

fn parts(ms: u64) -> (String, Time) {
    let secs = ms / 1000;
    let days = i64::try_from(secs / 86_400).unwrap_or(0);
    let (y, m, d) = civil_from_days(days);
    let rem = secs % 86_400;
    (
        format!("{y:04}-{m:02}-{d:02}"),
        (
            format!("{:02}", rem / 3600),
            format!("{:02}", rem / 60 % 60),
            format!("{:02}", rem % 60),
            format!("{:03}", ms % 1000),
        ),
    )
}

/// The proleptic Gregorian date `days` after 1970-01-01.
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_match_pi() {
        assert_eq!(iso(1_700_000_000_123), "2023-11-14T22:13:20.123Z");
        assert_eq!(file_stamp(1_700_000_000_123), "2023-11-14T22-13-20-123Z");
        assert_eq!(uuid().len(), 36);
        assert_eq!(entry_id().len(), 8);
    }
}
