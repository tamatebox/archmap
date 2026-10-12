//! A work snapshot: read as a fetch wrote it, and fetched from GitHub by
//! [`github`] only when a command asks to fetch.

use std::fs::{File, OpenOptions, TryLockError};
use std::path::{Path, PathBuf};

pub mod github;

use archmap_core::work::{RefreshStatus, Snapshot, STATUS_SCHEMA, WORK_SCHEMA};
use serde::Serialize;

/// Where a fetch writes the snapshot and readers look for it, relative to
/// the root.
pub const DEFAULT_PATH: &str = ".archmap/github.json";

/// Why a snapshot that exists could not be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReadError {
    /// The file system refused it (permissions, a directory, ...).
    Unreadable(String),
    /// Not JSON, or not a snapshot's shape.
    Corrupt(String),
    /// Another format version: `found` is the file's, `None` when it names
    /// none.
    Schema { found: Option<u64> },
}

impl ReadError {
    /// Written by a newer archmap, which this one must not overwrite
    /// unasked.
    pub fn is_newer(&self) -> bool {
        matches!(self, ReadError::Schema { found: Some(n) } if *n > u64::from(WORK_SCHEMA))
    }
}

impl std::fmt::Display for ReadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ReadError::Unreadable(error) => write!(f, "not read: {error}"),
            ReadError::Corrupt(error) => write!(f, "no snapshot: {error}"),
            ReadError::Schema { found } => write!(
                f,
                "snapshot schema {}, and this archmap reads {WORK_SCHEMA}: fetch it again",
                found.map_or("none".to_owned(), |s| s.to_string())
            ),
        }
    }
}

/// The snapshot at `path`, normalized; `Ok(None)` when there is none. An
/// error says what is wrong with the file, not where it is: the caller names
/// it as its answers do, relative to the root.
pub fn read(path: &Path) -> Result<Option<Snapshot>, ReadError> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) if error.kind() == std::io::ErrorKind::InvalidData => {
            return Err(ReadError::Corrupt(error.to_string()))
        }
        Err(error) => return Err(ReadError::Unreadable(error.to_string())),
    };
    let schema = serde_json::from_str::<serde_json::Value>(&text)
        .map_err(|error| ReadError::Corrupt(error.to_string()))?
        .get("schema")
        .and_then(serde_json::Value::as_u64);
    if schema != Some(u64::from(WORK_SCHEMA)) {
        return Err(ReadError::Schema { found: schema });
    }
    let mut snapshot: Snapshot =
        serde_json::from_str(&text).map_err(|error| ReadError::Corrupt(error.to_string()))?;
    snapshot.normalize();
    Ok(Some(snapshot))
}

/// Write `snapshot` to `path` whole: to a temporary file beside it, then
/// renamed over it, so a reader never sees half a file and a failure
/// leaves an older snapshot as it was.
pub fn write(path: &Path, snapshot: &Snapshot) -> Result<(), String> {
    write_whole(path, snapshot)
}

/// Write `value` as JSON to `path` whole, through a temporary file beside
/// it renamed over it. Errors name no path: the caller names the file.
fn write_whole<T: Serialize>(path: &Path, value: &T) -> Result<(), String> {
    let text = serde_json::to_string_pretty(value).map_err(|e| e.to_string())? + "\n";
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir).map_err(|e| format!("creating its directory: {e}"))?;
    }
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let temporary = path.with_file_name(format!("{name}.{}.tmp", std::process::id()));
    std::fs::write(&temporary, text).map_err(|e| format!("writing it: {e}"))?;
    std::fs::rename(&temporary, path).map_err(|e| {
        let _ = std::fs::remove_file(&temporary);
        format!("writing it: {e}")
    })
}

/// The status file of the snapshot at `snapshot`: `github.status.json`
/// beside `github.json`.
pub fn status_path(snapshot: &Path) -> PathBuf {
    let stem = snapshot
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    snapshot.with_file_name(format!("{stem}.status.json"))
}

/// The refresh status at `path`; `None` when there is none, or none this
/// archmap reads: the status only advises, and the next attempt writes it
/// again.
pub fn read_status(path: &Path) -> Option<RefreshStatus> {
    let text = std::fs::read_to_string(path).ok()?;
    let status: RefreshStatus = serde_json::from_str(&text).ok()?;
    (status.schema == STATUS_SCHEMA).then_some(status)
}

/// Write the refresh status whole, as a snapshot is written.
pub fn write_status(path: &Path, status: &RefreshStatus) -> Result<(), String> {
    write_whole(path, status)
}

/// What taking a snapshot's lock found.
#[derive(Debug)]
pub enum Locked {
    /// This process holds it until the file is dropped.
    Held(File),
    /// Another process holds it: a fetch of this snapshot is running.
    Busy,
}

/// Take the lock every fetch of the snapshot at `snapshot` shares: an OS
/// advisory lock on `<snapshot>.lock` beside it, which ends with the
/// process however it ends. The file is created when missing and never
/// removed, since removing it could let two processes lock two files.
/// Another holder is `Busy`; an error of the file system is an error.
pub fn lock(snapshot: &Path) -> Result<Locked, String> {
    if let Some(dir) = snapshot.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir).map_err(|e| format!("creating its directory: {e}"))?;
    }
    let name = snapshot
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(snapshot.with_file_name(format!("{name}.lock")))
        .map_err(|e| format!("opening its lock: {e}"))?;
    match file.try_lock() {
        Ok(()) => Ok(Locked::Held(file)),
        Err(TryLockError::WouldBlock) => Ok(Locked::Busy),
        Err(TryLockError::Error(e)) => Err(format!("locking it: {e}")),
    }
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

/// Seconds since the epoch of a time `utc` writes (`2026-01-14T09:30:00Z`);
/// `None` for any other form.
pub fn seconds(time: &str) -> Option<i64> {
    let b = time.as_bytes();
    let shape = b.len() == 20
        && b[4] == b'-'
        && b[7] == b'-'
        && b[10] == b'T'
        && b[13] == b':'
        && b[16] == b':'
        && b[19] == b'Z';
    if !shape {
        return None;
    }
    let number = |range: std::ops::Range<usize>| -> Option<i64> { time.get(range)?.parse().ok() };
    let (year, month, day) = (number(0..4)?, number(5..7)?, number(8..10)?);
    let (hour, minute, second) = (number(11..13)?, number(14..16)?, number(17..19)?);
    if !(1..=12).contains(&month)
        || !(1..=31).contains(&day)
        || hour > 23
        || minute > 59
        || second > 60
    {
        return None;
    }
    // a civil date to days, after Howard Hinnant's `days_from_civil`
    let y = year - i64::from(month <= 2);
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some(days * 86_400 + hour * 3_600 + minute * 60 + second)
}

/// A remote URL without the credentials it may carry before its host
/// (`https://user:token@host/..`, `token@host:..`), as an answer or an error
/// may show it.
pub fn without_credentials(url: &str) -> String {
    let (scheme, rest) = match url.split_once("://") {
        Some((scheme, rest)) => (Some(scheme), rest),
        None => (None, url),
    };
    // the authority ends at the first `/`, or for an scp-like address at
    // the first `:`
    let end = match scheme {
        Some(_) => rest.find('/').unwrap_or(rest.len()),
        None => rest.find(':').unwrap_or(rest.len()),
    };
    let rest = match rest[..end].rfind('@') {
        Some(at) => &rest[at + 1..],
        None => rest,
    };
    match scheme {
        Some(scheme) => format!("{scheme}://{rest}"),
        None => rest.to_owned(),
    }
}

/// Whether a remote URL reaches its host over SSH (`git@host:owner/name`,
/// `ssh://host/owner/name`), where the host may be an alias of SSH's
/// configuration.
pub fn over_ssh(url: &str) -> bool {
    url.starts_with("ssh://") || (!url.contains("://") && remote_repository(url).is_some())
}

/// The host SSH's configuration names for `alias` (`ssh -G`, which reads
/// the user's configuration and connects nowhere), as GitHub's SSH host
/// over port 443 (`ssh.github.com`) is github.com; `None` when ssh cannot
/// say. An alias the configuration does not name is its own host.
pub fn ssh_host(alias: &str) -> Option<String> {
    if alias.starts_with('-') {
        return None;
    }
    let out = std::process::Command::new("ssh")
        .args(["-G", alias])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    host_named(&String::from_utf8_lossy(&out.stdout))
}

/// The host a configuration as `ssh -G` prints it names.
fn host_named(config: &str) -> Option<String> {
    config
        .lines()
        .find_map(|line| line.strip_prefix("hostname "))
        .map(|host| match host.trim().to_ascii_lowercase().as_str() {
            "ssh.github.com" => "github.com".to_owned(),
            host => host.to_owned(),
        })
}

/// The host, owner and name of the repository a remote URL names
/// (`https://github.com/acme/shop.git`, `git@github.com:acme/shop.git`,
/// `ssh://git@github.com/acme/shop`), the host as written: an SSH alias
/// stays the alias, which a fetch takes only for the host it names.
pub fn remote_repository(url: &str) -> Option<(String, String, String)> {
    let (authority, path) = match url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("ssh://"))
    {
        Some(rest) => rest.split_once('/')?,
        None => url.split_once(':').filter(|(a, _)| !a.contains('/'))?,
    };
    let host = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
    let host = host.split_once(':').map_or(host, |(h, _)| h);
    let path = path.trim_end_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path);
    let (owner, name) = path.split_once('/')?;
    let part = |p: &str| {
        !p.is_empty()
            && p.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b == b'.')
    };
    (part(host) && !host.starts_with('-') && part(owner) && part(name))
        .then(|| (host.to_owned(), owner.to_owned(), name.to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_ssh_remote_and_the_host_its_alias_names() {
        assert!(over_ssh("git@github-work:acme/shop.git"));
        assert!(over_ssh("ssh://git@github.com/acme/shop"));
        assert!(!over_ssh("https://github.com/acme/shop.git"));
        assert!(!over_ssh("/srv/git/shop"));
        // GitHub's SSH host over port 443 is github.com
        assert_eq!(
            host_named("user git\nhostname ssh.github.com\nport 443\n"),
            Some("github.com".into())
        );
        assert_eq!(
            host_named("hostname GHE.Example.com\n"),
            Some("ghe.example.com".into())
        );
        assert_eq!(host_named("user git\n"), None);
        assert_eq!(ssh_host("-oProxyCommand=x"), None);
    }

    #[test]
    fn times_are_rfc_3339_in_utc() {
        assert_eq!(utc(0), "1970-01-01T00:00:00Z");
        assert_eq!(utc(1_768_383_000), "2026-01-14T09:30:00Z");
        for time in [0, 1_768_383_000, 951_782_400, 4_107_542_399] {
            assert_eq!(seconds(&utc(time)), Some(time), "{time}");
        }
        assert_eq!(seconds("2026-01-14T09:30:00+09:00"), None);
        assert_eq!(seconds("2026-13-14T09:30:00Z"), None);
    }

    #[test]
    fn a_remote_shown_keeps_no_credentials() {
        for (url, shown) in [
            (
                "https://ci-token:ghs_secret@git.example.com/acme/shop.git",
                "https://git.example.com/acme/shop.git",
            ),
            (
                "ssh://git@github.com:22/acme/shop",
                "ssh://github.com:22/acme/shop",
            ),
            (
                "ghs_secret@git.example.com:acme/shop.git",
                "git.example.com:acme/shop.git",
            ),
            // an `@` past the host is no credential
            (
                "https://git.example.com/acme/sh@p.git",
                "https://git.example.com/acme/sh@p.git",
            ),
            ("/srv/git/shop.git", "/srv/git/shop.git"),
        ] {
            assert_eq!(without_credentials(url), shown, "{url}");
        }
    }

    #[test]
    fn a_remote_names_its_host_as_written() {
        let named = |host: &str| Some((host.into(), "acme".into(), "shop".into()));
        for url in [
            "https://github.com/acme/shop.git",
            "https://github.com/acme/shop",
            "git@github.com:acme/shop.git",
            "ssh://git@github.com/acme/shop",
            "ssh://git@github.com:22/acme/shop/",
        ] {
            assert_eq!(remote_repository(url), named("github.com"), "{url}");
        }
        for (url, host) in [
            ("https://evil.example/acme/shop", "evil.example"),
            ("git@gitlab.com:acme/shop.git", "gitlab.com"),
            (
                "https://github.com.evil.example/acme/shop",
                "github.com.evil.example",
            ),
            // an SSH alias, which only ssh's config resolves
            ("git@github-work:acme/shop.git", "github-work"),
        ] {
            assert_eq!(remote_repository(url), named(host), "{url}");
        }
        for url in [
            "https://github.com/acme",
            "/srv/git/shop.git",
            "../shop",
            "file:///srv/git/acme/shop.git",
            "git@-oProxyCommand=x:acme/shop.git",
        ] {
            assert_eq!(remote_repository(url), None, "{url}");
        }
    }
}
