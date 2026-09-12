//! Message ids and timestamps. One place so every module agrees on the formats.

use chrono::{DateTime, SecondsFormat, SubsecRound, Utc};

use crate::error::{Error, Result};

/// Ulid: time-ordered enough to read at a glance, but two ids minted in the same
/// millisecond can sort either way, so queues order on `created_at` + rowid instead.
pub fn new_id() -> String {
    ulid::Ulid::new().to_string()
}

/// Truncated to the precision we persist, so a value survives a store round trip
/// unchanged and timestamps compare equal across processes.
pub fn now() -> DateTime<Utc> {
    Utc::now().trunc_subsecs(3)
}

pub fn to_rfc3339(ts: &DateTime<Utc>) -> String {
    ts.to_rfc3339_opts(SecondsFormat::Millis, true)
}

pub fn parse_rfc3339(s: &str) -> Result<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s)
        .map(|t| t.with_timezone(&Utc))
        .map_err(|e| Error::Parse(format!("bad timestamp {s:?}: {e}")))
}
