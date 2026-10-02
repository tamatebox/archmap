//! A work snapshot: read as a fetch wrote it, and fetched from GitHub by
//! [`github`] only when a command asks to fetch.

use std::path::Path;

pub mod github;

use archmap_core::work::{Snapshot, WORK_SCHEMA};

/// Where a fetch writes the snapshot and readers look for it, relative to
/// the root.
pub const DEFAULT_PATH: &str = ".archmap/github.json";

/// The snapshot at `path`, normalized; `Ok(None)` when there is none.
pub fn read(path: &Path) -> Result<Option<Snapshot>, String> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("reading {}: {error}", path.display())),
    };
    let schema = serde_json::from_str::<serde_json::Value>(&text)
        .map_err(|error| format!("{} is no snapshot: {error}", path.display()))?
        .get("schema")
        .and_then(serde_json::Value::as_u64);
    if schema != Some(u64::from(WORK_SCHEMA)) {
        return Err(format!(
            "{} has snapshot schema {}, and this archmap reads {WORK_SCHEMA}: fetch it again",
            path.display(),
            schema.map_or("none".to_owned(), |s| s.to_string())
        ));
    }
    let mut snapshot: Snapshot = serde_json::from_str(&text)
        .map_err(|error| format!("{} is no snapshot: {error}", path.display()))?;
    snapshot.normalize();
    Ok(Some(snapshot))
}

/// Write `snapshot` to `path` whole: to a temporary file beside it, then
/// renamed over it, so a reader never sees half a file and a failure
/// leaves an older snapshot as it was.
pub fn write(path: &Path, snapshot: &Snapshot) -> Result<(), String> {
    let text = serde_json::to_string_pretty(snapshot).map_err(|e| e.to_string())? + "\n";
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("creating {}: {e}", dir.display()))?;
    }
    let temporary = path.with_extension(format!("json.{}.tmp", std::process::id()));
    std::fs::write(&temporary, text)
        .map_err(|e| format!("writing {}: {e}", temporary.display()))?;
    std::fs::rename(&temporary, path).map_err(|e| {
        let _ = std::fs::remove_file(&temporary);
        format!("writing {}: {e}", path.display())
    })
}

/// A time in seconds since the epoch as RFC 3339 in UTC
/// (`2026-01-14T09:30:00Z`), the form GitHub gives times in.
pub fn utc(time: i64) -> String {
    let days = time.div_euclid(86_400);
    let seconds = time.rem_euclid(86_400);
    // days to a civil date, after Howard Hinnant's `civil_from_days`
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        seconds / 3600,
        seconds % 3600 / 60,
        seconds % 60
    )
}

/// The owner and name of a github.com repository a remote URL names
/// (`https://github.com/acme/shop.git`, `git@github.com:acme/shop.git`,
/// `ssh://git@github.com/acme/shop`); `None` for any other host, which a
/// fetch takes only when named.
pub fn github_remote(url: &str) -> Option<(String, String)> {
    let path = url
        .strip_prefix("https://github.com/")
        .or_else(|| url.strip_prefix("git@github.com:"))
        .or_else(|| url.strip_prefix("ssh://git@github.com/"))?;
    let path = path.trim_end_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path);
    let (owner, name) = path.split_once('/')?;
    let part = |p: &str| {
        !p.is_empty()
            && p.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b == b'.')
    };
    (part(owner) && part(name)).then(|| (owner.to_owned(), name.to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn times_are_rfc_3339_in_utc() {
        assert_eq!(utc(0), "1970-01-01T00:00:00Z");
        assert_eq!(utc(1_768_383_000), "2026-01-14T09:30:00Z");
    }

    #[test]
    fn only_a_github_com_remote_names_a_repository() {
        for url in [
            "https://github.com/acme/shop.git",
            "https://github.com/acme/shop",
            "git@github.com:acme/shop.git",
            "ssh://git@github.com/acme/shop",
        ] {
            assert_eq!(
                github_remote(url),
                Some(("acme".into(), "shop".into())),
                "{url}"
            );
        }
        for url in [
            "https://evil.example/acme/shop",
            "git@gitlab.com:acme/shop.git",
            "https://github.com.evil.example/acme/shop",
            "https://github.com/acme",
            "/srv/git/shop.git",
        ] {
            assert_eq!(github_remote(url), None, "{url}");
        }
    }
}
