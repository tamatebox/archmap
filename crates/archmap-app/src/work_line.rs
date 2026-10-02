//! The line that names a work snapshot, as `summary`'s Coverage and
//! `query '#N'` write it.

use std::fmt::Write;

use archmap_core::work::Snapshot;

/// `github acme/shop, 3 issues and 2 pull requests updated since
/// 2025-10-03 (fetched 2026-10-03), as visible to the account that fetched`.
pub(crate) fn range_line(snapshot: &Snapshot) -> String {
    let range = &snapshot.range;
    let mut line = format!(
        "{} {}, {} and {}",
        snapshot.source,
        snapshot.repository,
        plural_word(range.issues, "issue", "issues"),
        plural_word(range.pull_requests, "pull request", "pull requests"),
    );
    match &range.updated_since {
        Some(since) => {
            let _ = write!(line, " updated since {}", day(since));
        }
        None => line.push_str(", every one"),
    }
    if let Some(effective) = range
        .effective_since
        .as_deref()
        .filter(|_| range.reached_bound)
    {
        let _ = write!(
            line,
            " (bound of {} reached: updated since {})",
            range.bound,
            day(effective)
        );
    }
    let _ = write!(
        line,
        ", fetched {}, as visible to the account that fetched",
        utc_minute(&snapshot.fetched_at)
    );
    line
}

/// The date of a time as the tracker gives it (`2026-09-20T10:00:00Z`).
pub(crate) fn day(time: &str) -> &str {
    time.get(..10).unwrap_or(time)
}

pub(crate) fn plural_word(n: usize, one: &str, many: &str) -> String {
    match n {
        1 => format!("1 {one}"),
        n => format!("{n} {many}"),
    }
}

/// `2026-10-02T23:55:12Z` as `2026-10-02 23:55 UTC`, so a fetch late in
/// the day where the reader lives does not read as a day old.
fn utc_minute(time: &str) -> String {
    match (time.get(..10), time.get(11..16)) {
        (Some(date), Some(minute)) => format!("{date} {minute} UTC"),
        _ => time.to_owned(),
    }
}
