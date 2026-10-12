//! `fetch github`: write a work snapshot from GitHub through the gh CLI.
//! The only command that reaches the network; no interface but the CLI
//! offers it.

use std::path::Path;
use std::process::Command;

use anyhow::{bail, Context, Result};
use archmap_core::history::HistoryState;
use archmap_core::work::SinceRule;
use archmap_scan::work::github::{fetch, gh, update, FetchOptions, Update};
use archmap_scan::work::{read, remote_repository, utc, without_credentials, write, DEFAULT_PATH};

/// Items read per kind unless told otherwise.
pub const DEFAULT_MAX_ITEMS: usize = 5_000;

/// The history read that sets the default date when it holds at least this
/// many commits and is no shallow clone.
const MIN_COMMITS_FOR_DATE: usize = 100;

/// What `fetch github` takes.
#[derive(Debug, Clone, Copy)]
pub struct FetchRequest<'a> {
    /// `owner/name` on github.com, or `host/owner/name`; else the root's
    /// `origin` when it is on github.com.
    pub repo: Option<&'a str>,
    /// Items updated since this date (`2026-01-31` or RFC 3339).
    pub since: Option<&'a str>,
    /// Every item, whatever its date.
    pub all: bool,
    pub max_items: usize,
    pub titles: bool,
    /// Where to write; `.archmap/github.json` under the root otherwise.
    pub output: Option<&'a Path>,
    /// Update the snapshot at `output` with the items changed since its
    /// fetch rather than fetch it whole, when it can.
    pub update: bool,
    /// For an update: fetch whole when the last whole fetch is older than
    /// this (`30m`, `24h`, `7d`).
    pub full_if_older_than: &'a str,
}

/// A duration as `fetch github` takes it: a whole number of seconds,
/// minutes, hours or days (`90s`, `30m`, `24h`, `7d`).
pub fn duration(text: &str) -> Result<i64> {
    let wrong =
        || anyhow::anyhow!("a duration is a number and s, m, h or d (30m, 24h), not `{text}`");
    let unit = text.chars().last().ok_or_else(wrong)?;
    let number = &text[..text.len() - unit.len_utf8()];
    let per = match unit {
        's' => 1,
        'm' => 60,
        'h' => 3_600,
        'd' => 86_400,
        _ => return Err(wrong()),
    };
    match number.parse::<i64>() {
        Ok(n) if n >= 0 && !number.starts_with('+') => {
            n.checked_mul(per).context("that duration is too long")
        }
        _ => Err(wrong()),
    }
}

/// Fetch, write the snapshot, and say what was written.
pub fn fetch_github(root: &Path, request: &FetchRequest) -> Result<String> {
    let (host, owner, name) = repository(root, request.repo)?;
    let now_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    // a wrong limit fails before anything runs, whether it is used or not
    let full_limit = duration(request.full_if_older_than)?;
    let (since, since_rule) = since(root, request, now_secs)?;
    let mut options = FetchOptions {
        host: host.clone(),
        owner,
        name,
        since,
        since_rule,
        max_items: request.max_items,
        titles: request.titles,
        now: utc(now_secs),
        // a margin for a local clock ahead of GitHub's
        reread_since: utc(now_secs - 300),
    };
    let path = match request.output {
        Some(path) => path.to_path_buf(),
        None => root.join(DEFAULT_PATH),
    };
    let shown = path
        .strip_prefix(root)
        .unwrap_or(&path)
        .display()
        .to_string();
    let mut run = gh(&host);
    // an update when the snapshot allows one, else why it is fetched whole
    let whole = match request.update {
        false => None,
        true => Some(match read(&path) {
            Ok(None) => "there is no snapshot".to_owned(),
            Err(_) => "the snapshot could not be read".to_owned(),
            Ok(Some(previous)) => {
                let whole_after = utc(now_secs - full_limit);
                match update(&options, &mut run, &previous, &whole_after)
                    .map_err(anyhow::Error::msg)?
                {
                    Update::Merged { snapshot, read } => {
                        write(&path, &snapshot).map_err(anyhow::Error::msg)?;
                        return Ok(format!(
                            "updated {shown} with {} changed since {}: {}\n",
                            crate::work_line::plural_word(read, "item", "items"),
                            crate::work_line::utc_minute(&previous.fetched_at),
                            crate::work_line::range_line(&snapshot)
                        ));
                    }
                    Update::Whole { reason } => {
                        over_range(&mut options, &previous.range);
                        reason
                    }
                }
            }
        }),
    };
    let snapshot = fetch(&options, &mut run).map_err(anyhow::Error::msg)?;
    write(&path, &snapshot).map_err(anyhow::Error::msg)?;
    let why = whole
        .map(|reason| format!(" (fetched whole: {reason})"))
        .unwrap_or_default();
    Ok(format!(
        "wrote {shown}{why}: {}\n",
        crate::work_line::range_line(&snapshot)
    ))
}

/// A whole fetch in place of a refused update reads the snapshot's own
/// range, which `--update` takes no flag to change.
fn over_range(options: &mut FetchOptions, range: &archmap_core::work::Range) {
    options.since = range.updated_since.clone();
    options.since_rule = range.since_rule;
}

/// The host, owner and name to fetch. The root's `origin` names only a
/// github.com repository: a clone can name any host, and gh would send it
/// the token it keeps for that host. Another host, an SSH alias of
/// github.com included, must be named, and be one gh is logged in to; the
/// error says what `origin` names.
fn repository(root: &Path, repo: Option<&str>) -> Result<(String, String, String)> {
    let Some(repo) = repo else {
        let url = archmap_scan::history::origin_url(root)
            .context("the root has no `origin` remote: name the repository with --repo")?;
        let shown = without_credentials(&url);
        return match remote_repository(&url) {
            Some((host, owner, name)) if host == "github.com" => Ok((host, owner, name)),
            Some((host, owner, name)) => bail!(
                "`origin` ({shown}) is not a github.com repository: name it with --repo \
                 {owner}/{name} if `{host}` is an SSH alias of github.com, else with \
                 --repo HOST/{owner}/{name}"
            ),
            None => bail!(
                "`origin` ({shown}) is not a github.com repository: name it with --repo \
                 [HOST/]OWNER/NAME"
            ),
        };
    };
    let parts: Vec<&str> = repo.split('/').collect();
    let valid = |p: &str| {
        !p.is_empty()
            && p.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b == b'.')
    };
    match parts.as_slice() {
        [owner, name] if valid(owner) && valid(name) => Ok((
            "github.com".to_owned(),
            (*owner).to_owned(),
            (*name).to_owned(),
        )),
        [host, owner, name] if valid(host) && valid(owner) && valid(name) => {
            if *host != "github.com" && !logged_in(host) {
                bail!("gh is not logged in to {host}: `gh auth login --hostname {host}` first");
            }
            Ok(((*host).to_owned(), (*owner).to_owned(), (*name).to_owned()))
        }
        _ => bail!("--repo takes OWNER/NAME or HOST/OWNER/NAME, not `{repo}`"),
    }
}

/// Whether gh reports a login for `host`.
fn logged_in(host: &str) -> bool {
    Command::new("gh")
        .args(["auth", "status", "--hostname", host])
        .env("GH_PROMPT_DISABLED", "1")
        .output()
        .is_ok_and(|out| out.status.success())
}

/// The date items must be updated since, and what set it: the one given,
/// else the oldest commit the history read holds, else 365 days ago; none
/// for every item.
fn since(root: &Path, request: &FetchRequest, now: i64) -> Result<(Option<String>, SinceRule)> {
    if request.all {
        return Ok((None, SinceRule::All));
    }
    if let Some(date) = request.since {
        let given = match date.len() {
            10 => format!("{date}T00:00:00Z"),
            _ => date.to_owned(),
        };
        let shape = given.len() == 20
            && given.as_bytes()[4] == b'-'
            && given.as_bytes()[10] == b'T'
            && given.ends_with('Z');
        if !shape {
            bail!("--since takes a date (2026-01-31) or a UTC time (2026-01-31T09:00:00Z)");
        }
        return Ok((Some(given), SinceRule::Given));
    }
    let history = archmap_scan::history::read(root, archmap_scan::history::DEFAULT_BOUND);
    let deep = matches!(history.state, HistoryState::Read { shallow: false, .. })
        && history.commits.len() >= MIN_COMMITS_FOR_DATE;
    match history
        .commits
        .iter()
        .map(|c| c.time)
        .min()
        .filter(|_| deep)
    {
        Some(oldest) => Ok((Some(utc(oldest)), SinceRule::OldestCommitRead)),
        None => Ok((Some(utc(now - 365 * 86_400)), SinceRule::Days365)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_whole_fetch_in_place_of_an_update_keeps_the_snapshots_range() {
        let mut options = FetchOptions {
            host: "github.com".into(),
            owner: "acme".into(),
            name: "shop".into(),
            since: Some("2026-01-01T00:00:00Z".into()),
            since_rule: SinceRule::OldestCommitRead,
            max_items: 10,
            titles: true,
            now: "2026-10-04T00:00:00Z".into(),
            reread_since: "2026-10-03T23:55:00Z".into(),
        };
        // a snapshot of every item stays one of every item
        let range = archmap_core::work::Range {
            updated_since: None,
            since_rule: SinceRule::All,
            bound: 10,
            reached_bound: false,
            effective_since: None,
            issues: 0,
            pull_requests: 0,
        };
        over_range(&mut options, &range);
        assert_eq!((options.since, options.since_rule), (None, SinceRule::All));
    }

    #[test]
    fn a_duration_is_a_number_and_a_unit() {
        assert_eq!(duration("90s").unwrap(), 90);
        assert_eq!(duration("30m").unwrap(), 1_800);
        assert_eq!(duration("24h").unwrap(), 86_400);
        assert_eq!(duration("7d").unwrap(), 604_800);
        for wrong in [
            "",
            "h",
            "1.5h",
            "-1h",
            "+1h",
            "24",
            "1w",
            "99999999999999999d",
            "24時",
        ] {
            assert!(duration(wrong).is_err(), "{wrong}");
        }
    }
}
