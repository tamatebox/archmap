//! The words that name work and commits, as `summary`'s Coverage, `query
//! '#N'`, `Work` and `Changed in the same commits` write them: a snapshot's
//! line, an item's kind and state, a commit's short SHA and date.

use std::fmt::Write;

use archmap_core::work::{Item, ItemKind, ItemState, Snapshot};

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

pub(crate) fn kind_word(kind: ItemKind) -> &'static str {
    match kind {
        ItemKind::Issue => "issue",
        ItemKind::PullRequest => "pull request",
    }
}

pub(crate) fn state_word(state: ItemState) -> &'static str {
    match state {
        ItemState::Open => "open",
        ItemState::Closed => "closed",
        ItemState::Merged => "merged",
    }
}

/// `merged 2026-09-20`, `closed as completed 2026-09-20`, `open`; the
/// date left out when the snapshot has none.
pub(crate) fn state_line(item: &Item) -> String {
    let on = |t: &Option<String>| {
        t.as_deref()
            .map(|t| format!(" {}", day(t)))
            .unwrap_or_default()
    };
    match item.state {
        ItemState::Open => "open".to_owned(),
        ItemState::Merged => format!("merged{}", on(&item.merged_at)),
        ItemState::Closed => match item.state_reason.as_deref() {
            Some(reason) => format!(
                "closed as {}{}",
                reason.replace('_', " "),
                on(&item.closed_at)
            ),
            None => format!("closed{}", on(&item.closed_at)),
        },
    }
}

/// A commit's SHA as the text shows it: its first 7 characters.
pub(crate) fn short(id: &str) -> &str {
    id.get(..7).unwrap_or(id)
}

/// The UTC date of a time in seconds since the epoch, as `2026-01-14`.
pub(crate) fn date(time: i64) -> String {
    // days to a civil date, after Howard Hinnant's `civil_from_days`
    let z = time.div_euclid(86_400) + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates_are_utc_calendar_days() {
        assert_eq!(date(0), "1970-01-01");
        assert_eq!(date(951_782_400), "2000-02-29");
        assert_eq!(date(1_768_348_800), "2026-01-14");
        assert_eq!(date(1_768_435_199), "2026-01-14");
        assert_eq!(date(-86_400), "1969-12-31");
    }
}
