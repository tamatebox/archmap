//! `fetch github`: write a work snapshot from GitHub through the gh CLI.
//! The only command that reaches the network; no interface but the CLI
//! offers it.

use std::path::Path;
use std::process::Command;

use anyhow::{bail, Context, Result};
use archmap_core::history::HistoryState;
use archmap_core::work::SinceRule;
use archmap_scan::work::github::{fetch, gh, FetchOptions};
use archmap_scan::work::{github_remote, utc, write, DEFAULT_PATH};

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
}

/// Fetch, write the snapshot, and say what was written.
pub fn fetch_github(root: &Path, request: &FetchRequest) -> Result<String> {
    let (host, owner, name) = repository(root, request.repo)?;
    let now_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let (since, since_rule) = since(root, request, now_secs)?;
    let options = FetchOptions {
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
    let mut run = gh(&host);
    let snapshot = fetch(&options, &mut run).map_err(anyhow::Error::msg)?;
    let path = match request.output {
        Some(path) => path.to_path_buf(),
        None => root.join(DEFAULT_PATH),
    };
    write(&path, &snapshot).map_err(anyhow::Error::msg)?;
    let shown = path
        .strip_prefix(root)
        .unwrap_or(&path)
        .display()
        .to_string();
    Ok(format!(
        "wrote {shown}: {}\n",
        crate::work_line::range_line(&snapshot)
    ))
}

/// The host, owner and name to fetch. The root's `origin` names only a
/// github.com repository: a clone can name any host, and gh would send it
/// the token it keeps for that host. Another host must be named, and be one
/// gh is logged in to.
fn repository(root: &Path, repo: Option<&str>) -> Result<(String, String, String)> {
    let Some(repo) = repo else {
        let url = archmap_scan::history::origin_url(root)
            .context("the root has no `origin` remote: name the repository with --repo")?;
        let (owner, name) = github_remote(&url).with_context(|| {
            format!(
                "`origin` ({url}) is not a github.com repository: name it with --repo \
                 [HOST/]OWNER/NAME"
            )
        })?;
        return Ok(("github.com".to_owned(), owner, name));
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
