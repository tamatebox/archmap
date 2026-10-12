//! `fetch github`: write a work snapshot from GitHub through the gh CLI.
//! The only command that reaches the network; no interface but the CLI
//! offers it.

use std::cell::RefCell;
use std::path::Path;
use std::process::Command;

use anyhow::{anyhow, bail, Context, Result};
use archmap_core::history::HistoryState;
use archmap_core::work::{
    Account, FailureKind, RefreshError, RefreshStatus, SinceRule, Snapshot, STATUS_SCHEMA,
};
use archmap_scan::work::github::{
    classify, fetch, fetched_otherwise, rate_limit_reset, redact, update, FetchOptions, Gh,
    RateHints, Response, Run, Update,
};
use archmap_scan::work::{
    lock, over_ssh, read, read_status, remote_repository, seconds, ssh_host, status_path, utc,
    without_credentials, write, write_status, Locked, ReadError, DEFAULT_PATH,
};
use serde_json::Value;

use crate::work_line::{plural_word, range_line, utc_minute};

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
    /// Items read per kind at most; when not given, the snapshot's own
    /// bound for an automatic refresh, else [`DEFAULT_MAX_ITEMS`].
    pub max_items: Option<usize>,
    /// `false` when titles are left out (`--no-titles`); an automatic
    /// refresh also leaves them out of a snapshot fetched without them.
    pub titles: bool,
    /// Where to write; `.archmap/github.json` under the root otherwise.
    pub output: Option<&'a Path>,
    /// Update the snapshot at `output` with the items changed since its
    /// fetch rather than fetch it whole, when it can.
    pub update: bool,
    /// For an update: fetch whole when the last whole fetch is older than
    /// this (`30m`, `24h`, `7d`).
    pub full_if_older_than: &'a str,
    /// An automatic refresh: update only when the snapshot's last fetch is
    /// older than this, and no failure's backoff is running.
    pub if_older_than: Option<&'a str>,
}

/// What `fetch github` did, and what to print.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fetched {
    pub message: String,
    pub outcome: Outcome,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// The snapshot was written.
    Written,
    /// An automatic refresh found nothing to do: the snapshot is fresh, a
    /// failure's backoff runs, or another fetch holds the snapshot.
    Skipped,
    /// Nothing was written, which the caller asked for: the fetch failed
    /// (the status says so), another fetch holds the snapshot, or an
    /// automatic refresh met a snapshot it does not overwrite.
    NotWritten,
}

/// The first wait after a failure, doubled with each failure in a row.
const BACKOFF: i64 = 300;
/// The longest wait after failures.
const MAX_BACKOFF: i64 = 6 * 3_600;

/// Asks gh whether it holds a login for a host: `Err` with what it printed.
type LoggedIn<'a> = dyn FnMut(&str) -> Result<(), String> + 'a;

/// What a fetch reaches outside the repository: GitHub through a
/// transport and the rate-limit facts it gives, gh's logins, and the clock.
/// `fetch github` gives the real ones; tests give their own.
pub(crate) struct Env<'a> {
    pub(crate) run: Box<Run<'a>>,
    /// What the last response was; `None` when gh could not be run.
    pub(crate) response: Box<dyn Fn() -> Option<Response> + 'a>,
    /// When GitHub's primary limit resets, from its `rate_limit` endpoint.
    pub(crate) primary_reset: Box<dyn FnMut() -> Option<i64> + 'a>,
    /// Whether gh holds a login for a host other than github.com: `Err`
    /// with what `gh auth status` printed when it does not say so.
    pub(crate) logged_in: Box<LoggedIn<'a>>,
    pub(crate) now: i64,
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

/// Fetch, write the snapshot, and say what was done.
pub fn fetch_github(root: &Path, request: &FetchRequest) -> Result<Fetched> {
    limits(request)?;
    let automatic = request.if_older_than.is_some();
    let (repository, from_snapshot) = match repository(root, request.repo) {
        Ok(repository) => (repository, false),
        Err(refused) => {
            // an automatic refresh updates the snapshot a fetch with --repo
            // wrote for an origin that names no github.com repository
            let url = (automatic && request.repo.is_none())
                .then(|| archmap_scan::history::origin_url(root))
                .flatten();
            let Some((url, origin)) = url.and_then(|u| Some((u.clone(), remote_repository(&u)?)))
            else {
                return Err(refused);
            };
            let path = snapshot_file(root, request);
            let shown = relative(root, &path);
            let ssh = over_ssh(&url).then_some(ssh_host as fn(&str) -> Option<String>);
            let repository =
                snapshot_repository(&origin, ssh, &read(&path), &shown).map_err(|why| {
                    anyhow!(
                        "{refused}; an automatic refresh would update the repository of a \
                         snapshot fetched so, but {why}"
                    )
                })?;
            (repository, true)
        }
    };
    let host = repository.0.clone();
    // an automatic refresh with no login for the host writes nothing
    if automatic {
        match holds_token(&host) {
            Ok(true) => {}
            Ok(false) => {
                return Ok(not_written(format!(
                    "gh holds no login for {host}: nothing fetched (`gh auth login --hostname \
                     {host}`)"
                )))
            }
            Err(error) => return Ok(not_written(format!("{error}: nothing fetched"))),
        }
    }
    let gh = RefCell::new(Gh::new(&host));
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let mut env = Env {
        run: Box::new(|request| gh.borrow_mut().call(request)),
        response: Box::new(|| gh.borrow().last.clone()),
        primary_reset: Box::new(|| rate_limit_reset(&host)),
        logged_in: Box::new(logged_in),
        now,
    };
    fetch_with(root, request, (repository, from_snapshot), &mut env)
}

/// The whole limit and the automatic refresh's limit of a request, its
/// date checked: wrong durations and dates fail before anything runs,
/// whether used or not.
fn limits(request: &FetchRequest) -> Result<(i64, Option<i64>)> {
    let full_limit = duration(request.full_if_older_than)?;
    let ttl = request.if_older_than.map(duration).transpose()?;
    request.since.map(given_since).transpose()?;
    Ok((full_limit, ttl))
}

/// `fetch_github` with what it reaches outside the repository given, and
/// the repository with whether it was taken from the snapshot. Everything
/// is decided under the snapshot's lock, from the snapshot and the status
/// as they are then.
pub(crate) fn fetch_with(
    root: &Path,
    request: &FetchRequest,
    ((host, owner, name), from_snapshot): ((String, String, String), bool),
    env: &mut Env,
) -> Result<Fetched> {
    let (full_limit, ttl) = limits(request)?;
    let automatic = ttl.is_some();
    let repository = format!("{owner}/{name}");
    let path = snapshot_file(root, request);
    let shown = relative(root, &path);
    let status_file = status_path(&path);
    let status_shown = relative(root, &status_file);
    let _lock = match lock(&path).map_err(|e| anyhow!("{shown}: {e}"))? {
        Locked::Held(file) => file,
        Locked::Busy => {
            let busy = format!("another archmap fetch is updating {shown}: nothing fetched");
            return Ok(match automatic {
                true => skipped(format!("busy: {busy}")),
                false => not_written(busy),
            });
        }
    };
    let previous = read(&path);
    // a repository taken from the snapshot is that snapshot's still
    let still = |s: &Snapshot| {
        s.source == "github" && s.host == host && s.repository.eq_ignore_ascii_case(&repository)
    };
    if from_snapshot && !matches!(&previous, Ok(Some(s)) if still(s)) {
        return Ok(not_written(format!(
            "{shown} is no longer of {host}/{repository}, which `origin` named through it: \
             nothing fetched"
        )));
    }
    // what the last attempts at this repository left since the snapshot: a
    // failure no later than its fetch is past, since a fetch wrote the
    // snapshot after it, and may have stopped before writing its status
    let status = read_status(&status_file).filter(|s| {
        s.source == "github"
            && s.host == host
            && s.repository.eq_ignore_ascii_case(&repository)
            && match &previous {
                Ok(Some(snapshot)) => after_fetch(s, snapshot),
                _ => true,
            }
    });
    // an automatic refresh keeps the snapshot's title setting and bound
    // where the command gives none, so titles left out stay out
    let kept = match &previous {
        Ok(Some(snapshot)) if automatic => Some(snapshot),
        _ => None,
    };
    let titles = request.titles && !kept.is_some_and(|s| s.no_titles);
    let max_items = request
        .max_items
        .or(kept.map(|s| s.range.bound))
        .unwrap_or(DEFAULT_MAX_ITEMS);
    if let (Some(ttl), Some(limit)) = (ttl, request.if_older_than) {
        match &previous {
            Ok(Some(snapshot)) => {
                // fresh only as what this fetch asks for: another's is
                // updated, which fetches it whole and says why
                let asked = (titles, max_items);
                let same = fetched_otherwise(snapshot, &host, &repository, asked).is_none();
                if same && seconds(&snapshot.fetched_at).is_some_and(|at| at > env.now - ttl) {
                    return Ok(skipped(format!(
                        "fresh: {shown} was fetched {}, within {limit}: nothing fetched",
                        utc_minute(&snapshot.fetched_at)
                    )));
                }
            }
            Ok(None) => {}
            Err(error) => {
                return Ok(not_written(format!(
                    "{shown}: {error}: an automatic refresh never overwrites a snapshot it \
                     cannot read, so nothing was fetched; `archmap fetch github` fetches it again"
                )))
            }
        }
        if let Some((status, next)) = status
            .as_ref()
            .and_then(|s| Some((s, waiting(s, env.now)?)))
        {
            let kind = status
                .last_error
                .as_ref()
                .map_or("error", |e| e.kind.as_str());
            return Ok(skipped(format!(
                "waiting: the last attempt, {}, failed ({kind}); no automatic attempt before \
                 {}, and a change of account is not seen until then: nothing fetched \
                 (`archmap fetch github` tries now)",
                utc_minute(&status.last_attempt_at),
                utc_minute(next)
            )));
        }
    }
    let (since, since_rule) = since(root, request, env.now)?;
    let mut options = FetchOptions {
        host: host.clone(),
        owner,
        name,
        since,
        since_rule,
        max_items,
        titles,
        now: utc(env.now),
        // a margin for a local clock ahead of GitHub's
        reread_since: utc(env.now - 300),
    };
    // the account this attempt runs as, as the host answers; none when the
    // attempt stops before it learns it
    let mut account: Option<Account> = None;
    // a host gh confirms no login for fails as an attempt does, and waits
    let login = match host.as_str() {
        "github.com" => Ok(()),
        _ => (env.logged_in)(&host).map_err(|output| login_failure(&host, &output)),
    };
    let outcome = if let Err(failure) = login {
        Err(failure)
    } else {
        let run = &mut env.run;
        let mut learn = |request: &Value| -> Result<Value, String> {
            let answer = run(request)?;
            let schema = request["query"]
                .as_str()
                .is_some_and(|q| q.starts_with("query Schema"));
            if schema {
                account = answer["data"]["viewer"]["login"]
                    .as_str()
                    .map(|login| Account {
                        login: login.to_owned(),
                        id: answer["data"]["viewer"]["databaseId"].as_u64(),
                    });
            }
            Ok(answer)
        };
        let update_wanted = request.update || automatic;
        let whole_after = utc(env.now - full_limit);
        attempt(
            &mut options,
            &mut learn,
            update_wanted,
            &previous,
            &whole_after,
            &path,
            &shown,
        )
        .map_err(|failed| failure(failed, &shown, (env.response)()))
    };
    let started = utc(env.now);
    match outcome {
        Ok(message) => {
            // after the snapshot: a status older than it never shows
            let recorded = write_status(
                &status_file,
                &RefreshStatus {
                    schema: STATUS_SCHEMA,
                    source: "github".to_owned(),
                    host,
                    repository,
                    account,
                    last_attempt_at: started,
                    last_error: None,
                    next_retry_at: None,
                    consecutive_failures: 0,
                },
            );
            let message = match recorded {
                Ok(()) => message,
                Err(e) => format!("{message}\n  the attempt is not recorded: {status_shown}: {e}"),
            };
            Ok(Fetched {
                message,
                outcome: Outcome::Written,
            })
        }
        Err(Failure {
            printed,
            recorded,
            kind,
        }) => {
            // the same account failing again waits longer; an account the
            // attempt did not learn is no other account's failure
            let failures = match &status {
                Some(s) if s.last_error.is_some() && !other_account(&s.account, &account) => {
                    s.consecutive_failures + 1
                }
                _ => 1,
            };
            let backoff = (BACKOFF << (failures - 1).min(16)).min(MAX_BACKOFF);
            let hints = (env.response)().map(|r| r.hints);
            let next = retry_at(kind, hints, &mut env.primary_reset, env.now, backoff);
            let recorded = write_status(
                &status_file,
                &RefreshStatus {
                    schema: STATUS_SCHEMA,
                    source: "github".to_owned(),
                    host,
                    repository,
                    account,
                    last_attempt_at: started,
                    last_error: Some(RefreshError {
                        kind,
                        message: recorded,
                    }),
                    next_retry_at: Some(utc(next)),
                    consecutive_failures: failures,
                },
            );
            let note = match recorded {
                Ok(()) => format!(
                    "recorded in {status_shown}: no automatic attempt before {}",
                    utc_minute(&utc(next))
                ),
                Err(e) => format!("not recorded: {status_shown}: {e}"),
            };
            Ok(not_written(format!(
                "{printed}\n  {shown} is as it was; the failure ({}) is {note}",
                kind.as_str()
            )))
        }
    }
}

/// Where an attempt failed.
enum Failed {
    /// Asking GitHub through gh: the message gh or GitHub gave.
    Asking(String),
    /// Writing the snapshot: the file system's message, which names no path.
    Writing(String),
}

/// A failed attempt: what the fetch prints, what the status keeps and
/// answers show, and its kind.
#[derive(Debug, PartialEq, Eq)]
struct Failure {
    printed: String,
    recorded: String,
    kind: FailureKind,
}

/// What a failed attempt prints and keeps. Of a request it keeps the facts
/// of the last response only (its HTTP status and the codes of GraphQL's
/// errors), never gh's or GitHub's words, which may name local paths or
/// more; it prints those, redacted, and reads its kind from them.
fn failure(failed: Failed, shown: &str, response: Option<Response>) -> Failure {
    match failed {
        Failed::Asking(error) => {
            let printed = redact(&error);
            let kind = classify(&printed);
            let recorded = match response {
                None => "gh could not be run".to_owned(),
                Some(Response { status: None, .. }) => "no answer came from GitHub".to_owned(),
                Some(Response {
                    status: Some(status),
                    errors,
                    ..
                }) => match errors.is_empty() {
                    true => format!("GitHub answered HTTP {status}"),
                    false => format!(
                        "GitHub answered HTTP {status} with errors {}",
                        errors.join(", ")
                    ),
                },
            };
            Failure {
                printed,
                recorded,
                kind,
            }
        }
        Failed::Writing(error) => Failure {
            printed: redact(&format!("{shown}: {error}")),
            recorded: redact(&format!("the snapshot: {error}")),
            kind: FailureKind::Other,
        },
    }
}

/// One fetch: an update when one is wanted and the snapshot allows it,
/// else whole; the snapshot written; what to print.
fn attempt(
    options: &mut FetchOptions,
    run: &mut Run,
    update_wanted: bool,
    previous: &Result<Option<Snapshot>, ReadError>,
    whole_after: &str,
    path: &Path,
    shown: &str,
) -> Result<String, Failed> {
    let whole = match update_wanted {
        false => None,
        true => Some(match previous {
            Ok(None) => "there is no snapshot".to_owned(),
            Err(error) if error.is_newer() => "the snapshot has a newer format".to_owned(),
            Err(_) => "the snapshot could not be read".to_owned(),
            Ok(Some(previous)) => {
                match update(options, run, previous, whole_after).map_err(Failed::Asking)? {
                    Update::Merged { snapshot, read } => {
                        write(path, &snapshot).map_err(Failed::Writing)?;
                        return Ok(format!(
                            "updated {shown} with {} changed since {}: {}",
                            plural_word(read, "item", "items"),
                            utc_minute(&previous.fetched_at),
                            range_line(&snapshot)
                        ));
                    }
                    Update::Whole { reason } => {
                        over_range(options, &previous.range);
                        reason
                    }
                }
            }
        }),
    };
    let snapshot = fetch(options, run).map_err(Failed::Asking)?;
    write(path, &snapshot).map_err(Failed::Writing)?;
    let why = whole
        .map(|reason| format!(" (fetched whole: {reason})"))
        .unwrap_or_default();
    Ok(format!("wrote {shown}{why}: {}", range_line(&snapshot)))
}

/// Whether the attempt `status` records began after `snapshot` was
/// fetched. Answers and refreshes read a failure only then, since the
/// status is written after the snapshot.
pub(crate) fn after_fetch(status: &RefreshStatus, snapshot: &Snapshot) -> bool {
    matches!(
        (seconds(&status.last_attempt_at), seconds(&snapshot.fetched_at)),
        (Some(attempt), Some(fetched)) if attempt > fetched
    )
}

/// When a failed attempt's backoff ends, if it has not.
fn waiting(status: &RefreshStatus, now: i64) -> Option<&str> {
    status.last_error.as_ref()?;
    status
        .next_retry_at
        .as_deref()
        .filter(|next| seconds(next).is_some_and(|at| at > now))
}

/// Two accounts both known and not the same one.
fn other_account(a: &Option<Account>, b: &Option<Account>) -> bool {
    match (a, b) {
        (Some(a), Some(b)) => match (a.id, b.id) {
            (Some(x), Some(y)) => x != y,
            _ => !a.login.eq_ignore_ascii_case(&b.login),
        },
        _ => false,
    }
}

/// When to try again after a failure, after GitHub's guidance: a
/// `retry-after` header first; at a limit spent, its reset; for the
/// primary limit, else, the reset its `rate_limit` endpoint gives, which
/// counts against no primary limit (never asked for a secondary one, which
/// it may count against and cannot tell the end of); else `backoff`, at
/// least a minute.
fn retry_at(
    kind: FailureKind,
    hints: Option<RateHints>,
    primary_reset: &mut dyn FnMut() -> Option<i64>,
    now: i64,
    backoff: i64,
) -> i64 {
    let fallback = now + backoff.max(60);
    let hints = hints.unwrap_or_default();
    let limited = matches!(
        kind,
        FailureKind::RateLimit | FailureKind::SecondaryRateLimit
    );
    if !limited {
        return fallback;
    }
    if let Some(after) = hints.retry_after.filter(|s| *s > 0) {
        return now + after;
    }
    if let Some(reset) = hints
        .reset
        .filter(|r| *r > now && hints.remaining == Some(0))
    {
        return reset;
    }
    match kind {
        FailureKind::RateLimit => primary_reset().filter(|r| *r > now).unwrap_or(fallback),
        _ => fallback,
    }
}

fn skipped(message: String) -> Fetched {
    Fetched {
        message,
        outcome: Outcome::Skipped,
    }
}

fn not_written(message: String) -> Fetched {
    Fetched {
        message,
        outcome: Outcome::NotWritten,
    }
}

/// Where the snapshot of a fetch is: `output`, else `.archmap/github.json`
/// under the root.
fn snapshot_file(root: &Path, request: &FetchRequest) -> std::path::PathBuf {
    match request.output {
        Some(path) => path.to_path_buf(),
        None => root.join(DEFAULT_PATH),
    }
}

/// The repository an automatic refresh updates when `origin` names no
/// github.com repository: the one the snapshot names, which a fetch with
/// --repo wrote, when it is of the owner and name `origin` names, on the
/// host `origin` names (an enterprise host), or for an `origin` over SSH
/// on the host SSH's configuration gives its alias (`ssh`). Else why not:
/// a snapshot anyone may have written into the project points the fetch at
/// no repository `origin` does not name, and an alias whose host cannot be
/// told is not guessed at.
fn snapshot_repository(
    (origin_host, owner, name): &(String, String, String),
    ssh: Option<fn(&str) -> Option<String>>,
    snapshot: &Result<Option<Snapshot>, ReadError>,
    shown: &str,
) -> Result<(String, String, String), String> {
    let snapshot = match snapshot {
        Ok(Some(snapshot)) => snapshot,
        Ok(None) => return Err(format!("there is none at {shown}")),
        Err(error) => return Err(format!("{shown}: {error}")),
    };
    let named = format!("{owner}/{name}");
    let same = snapshot.source == "github" && snapshot.repository.eq_ignore_ascii_case(&named);
    let parts = snapshot.repository.split_once('/').filter(|_| same);
    let Some((owner, name)) = parts else {
        return Err(format!(
            "the one at {shown} is of {}/{}, not {named}",
            snapshot.host, snapshot.repository
        ));
    };
    let host = &snapshot.host;
    if !host.eq_ignore_ascii_case(origin_host) {
        let why = match ssh.map(|resolve| resolve(origin_host)) {
            None => format!("`origin` names {origin_host}"),
            Some(None) => format!("ssh cannot tell which host {origin_host} is"),
            Some(Some(real)) if real.eq_ignore_ascii_case(host) => String::new(),
            Some(Some(real)) => format!("ssh takes {origin_host} to {real}"),
        };
        if !why.is_empty() {
            return Err(format!("the one at {shown} is of {host}, and {why}"));
        }
    }
    Ok((host.clone(), owner.to_owned(), name.to_owned()))
}

/// Whether gh holds a token for `host`, asked of its own store (`gh auth
/// token`), which reaches no network; `Err` when gh cannot be run, which
/// says nothing of a login.
fn holds_token(host: &str) -> Result<bool, String> {
    Command::new("gh")
        .args(["auth", "token", "--hostname", host])
        .env("GH_PROMPT_DISABLED", "1")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|status| status.success())
        .map_err(|error| match error.kind() {
            std::io::ErrorKind::NotFound => {
                "the gh CLI is not installed: see https://cli.github.com".to_owned()
            }
            _ => format!("running gh: {error}"),
        })
}

/// `path` relative to the root when it is inside it, with `/`.
fn relative(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
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
/// github.com included, must be named, and be one gh is logged in to (asked
/// once a fetch runs); the error says what `origin` names.
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
        // whether gh holds a login for it is asked once a fetch runs
        [host, owner, name] if valid(host) && valid(owner) && valid(name) => {
            Ok(((*host).to_owned(), (*owner).to_owned(), (*name).to_owned()))
        }
        _ => bail!("--repo takes OWNER/NAME or HOST/OWNER/NAME, not `{repo}`"),
    }
}

/// Whether gh reports a login for `host`: `Err` with what it printed when
/// it does not.
fn logged_in(host: &str) -> Result<(), String> {
    let out = Command::new("gh")
        .args(["auth", "status", "--hostname", host])
        .env("GH_PROMPT_DISABLED", "1")
        .env("NO_COLOR", "1")
        .output()
        .map_err(|error| match error.kind() {
            std::io::ErrorKind::NotFound => {
                "the gh CLI is not installed: see https://cli.github.com".to_owned()
            }
            _ => format!("running gh: {error}"),
        })?;
    if out.status.success() {
        return Ok(());
    }
    let mut printed = String::from_utf8_lossy(&out.stderr).into_owned();
    printed.push_str(&String::from_utf8_lossy(&out.stdout));
    Err(printed)
}

/// The failure of `gh auth status` printing `output` for `host`. Only no
/// login at all is an authentication failure: gh reports a host it cannot
/// reach as an invalid token, so that says nothing either way and is an
/// error of no known kind, and a timeout is the network's. What gh printed
/// is printed, redacted, and never kept.
fn login_failure(host: &str, output: &str) -> Failure {
    let lower = output.to_lowercase();
    if lower.contains("not logged in") {
        let recorded = format!("gh is not logged in to {host}");
        return Failure {
            printed: format!("{recorded}: `gh auth login --hostname {host}` first"),
            recorded,
            kind: FailureKind::Auth,
        };
    }
    let kind = match lower.contains("timeout") {
        true => FailureKind::Network,
        false => FailureKind::Other,
    };
    // the line under the host's that says what failed
    let line = output
        .lines()
        .map(|line| line.trim().trim_start_matches("X ").trim())
        .find(|line| !line.is_empty() && !line.eq_ignore_ascii_case(host))
        .unwrap_or("it printed nothing");
    let recorded = format!("gh auth status could not confirm a login for {host}");
    Failure {
        printed: format!("{recorded}: {}", redact(line)),
        recorded,
        kind,
    }
}

/// The date items must be updated since, and what set it: the one given,
/// else the oldest commit the history read holds, else 365 days ago; none
/// for every item.
fn since(root: &Path, request: &FetchRequest, now: i64) -> Result<(Option<String>, SinceRule)> {
    if request.all {
        return Ok((None, SinceRule::All));
    }
    if let Some(date) = request.since {
        return Ok((Some(given_since(date)?), SinceRule::Given));
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

/// `--since` as a UTC time.
fn given_since(date: &str) -> Result<String> {
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
    Ok(given)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::path::PathBuf;

    use serde_json::json;

    /// 2026-09-22 07:20 UTC
    const T0: i64 = 1_790_061_600;
    const HOUR: i64 = 3_600;

    /// GitHub as a test sees it: answers to a fetch of a repository with no
    /// items, as the account `viewer`, until `answered` requests have been
    /// answered and every later one fails with `error`; the rate-limit
    /// facts a failure leaves; and what was asked.
    struct World {
        now: i64,
        viewer: Option<(&'static str, u64)>,
        answered: usize,
        error: Option<&'static str>,
        response: Option<Response>,
        reset: Option<i64>,
        calls: Cell<usize>,
        resets: Cell<usize>,
    }

    fn world(now: i64) -> World {
        World {
            now,
            viewer: Some(("fetcher", 7)),
            answered: 0,
            error: None,
            response: None,
            reset: None,
            calls: Cell::new(0),
            resets: Cell::new(0),
        }
    }

    impl World {
        fn failing(now: i64, answered: usize, error: &'static str) -> World {
            World {
                answered,
                error: Some(error),
                ..world(now)
            }
        }

        fn env(&self) -> Env<'_> {
            Env {
                run: Box::new(|request| {
                    let asked = self.calls.get();
                    self.calls.set(asked + 1);
                    match self.error {
                        Some(error) if asked >= self.answered => Err(error.to_owned()),
                        _ => Ok(answer(request, self.viewer)),
                    }
                }),
                response: Box::new(|| self.response.clone()),
                primary_reset: Box::new(|| {
                    self.resets.set(self.resets.get() + 1);
                    self.reset
                }),
                logged_in: Box::new(|_| Ok(())),
                now: self.now,
            }
        }

        fn fetch(&self, root: &Path, request: &FetchRequest) -> Fetched {
            self.try_fetch(root, request).unwrap()
        }

        fn try_fetch(&self, root: &Path, request: &FetchRequest) -> Result<Fetched> {
            self.fetch_of(root, request, "shop")
        }

        /// A fetch of `acme/<name>`.
        fn fetch_of(&self, root: &Path, request: &FetchRequest, name: &str) -> Result<Fetched> {
            let repository = ("github.com".into(), "acme".into(), name.into());
            fetch_with(root, request, (repository, false), &mut self.env())
        }
    }

    fn answer(request: &Value, viewer: Option<(&str, u64)>) -> Value {
        let query = request["query"].as_str().unwrap_or_default();
        let empty = json!({"pageInfo": {"hasNextPage": false, "endCursor": null}, "nodes": []});
        if query.starts_with("query Schema") {
            let viewer = viewer.map(|(login, id)| json!({"login": login, "databaseId": id}));
            json!({"data": {"issue": {"fields": []}, "pull": {"fields": []}, "viewer": viewer}})
        } else if query.starts_with("query Issues") {
            let variables = &request["variables"];
            let repository =
                format!("{}/{}", variables["owner"], variables["name"]).replace('"', "");
            json!({"data": {"repository": {"nameWithOwner": repository, "issues": empty}}})
        } else if query.starts_with("query PullRequests") {
            json!({"data": {"repository": {"pullRequests": empty}}})
        } else {
            panic!("unexpected request: {query}")
        }
    }

    /// Every request names its date, so that no test reads the history:
    /// a process another test thread starts holds a copy of this thread's
    /// lock until it runs its program, and the lock would read as busy.
    fn plain() -> FetchRequest<'static> {
        FetchRequest {
            repo: None,
            since: Some("2026-01-01"),
            all: false,
            max_items: Some(100),
            titles: true,
            output: None,
            update: false,
            full_if_older_than: "24h",
            if_older_than: None,
        }
    }

    /// As a hook runs it: no bound given.
    fn automatic(limit: &'static str) -> FetchRequest<'static> {
        FetchRequest {
            if_older_than: Some(limit),
            max_items: None,
            ..plain()
        }
    }

    fn updating() -> FetchRequest<'static> {
        FetchRequest {
            update: true,
            ..plain()
        }
    }

    /// An empty root, with a snapshot fetched whole at `fetched` unless
    /// `None`.
    fn root(name: &str, fetched: Option<i64>) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("archmap-fetch-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        if let Some(at) = fetched {
            let done = world(at).fetch(&dir, &plain());
            assert_eq!(done.outcome, Outcome::Written, "{}", done.message);
        }
        dir
    }

    fn snapshot(root: &Path) -> Snapshot {
        read(&root.join(DEFAULT_PATH)).unwrap().unwrap()
    }

    fn status(root: &Path) -> Option<RefreshStatus> {
        read_status(&status_path(&root.join(DEFAULT_PATH)))
    }

    fn bytes(root: &Path) -> Vec<u8> {
        std::fs::read(root.join(DEFAULT_PATH)).unwrap()
    }

    #[test]
    fn a_snapshot_within_the_limit_is_not_fetched_again() {
        let root = root("fresh", Some(T0));
        let world = world(T0 + 1_799);
        let done = world.fetch(&root, &automatic("30m"));
        assert_eq!(
            done,
            skipped(
                "fresh: .archmap/github.json was fetched 2026-09-22 07:20 UTC, within 30m: \
                 nothing fetched"
                    .into()
            )
        );
        assert_eq!(world.calls.get(), 0);
        assert_eq!(snapshot(&root).fetched_at, utc(T0));
    }

    #[test]
    fn a_snapshot_past_the_limit_is_updated() {
        let root = root("stale", Some(T0));
        let world = world(T0 + 1_800);
        let done = world.fetch(&root, &automatic("30m"));
        assert_eq!(done.outcome, Outcome::Written);
        assert!(
            done.message.starts_with(
                "updated .archmap/github.json with 0 items changed since 2026-09-22 07:20 UTC: "
            ),
            "{}",
            done.message
        );
        assert!(world.calls.get() > 0);
        let after = snapshot(&root);
        assert_eq!(after.fetched_at, utc(T0 + 1_800));
        assert_eq!(after.whole_fetched_at, Some(utc(T0)));
        let status = status(&root).unwrap();
        assert_eq!(status.last_error, None);
        assert_eq!(status.last_attempt_at, utc(T0 + 1_800));
    }

    #[test]
    fn an_automatic_refresh_still_fetches_whole_past_the_whole_limit() {
        let root = root("whole-limit", Some(T0));
        let done = world(T0 + 25 * HOUR).fetch(&root, &automatic("30m"));
        assert!(
            done.message
                .contains(" (fetched whole: its last whole fetch is older than the limit): "),
            "{}",
            done.message
        );
        assert_eq!(snapshot(&root).whole_fetched_at, Some(utc(T0 + 25 * HOUR)));
        // with no snapshot, it fetches one whole
        let empty = self::root("whole-none", None);
        let done = world(T0).fetch(&empty, &automatic("30m"));
        assert!(
            done.message
                .contains(" (fetched whole: there is no snapshot): "),
            "{}",
            done.message
        );
        // a fetch with neither flag fetches whole, however fresh the snapshot
        let done = world(T0 + 60).fetch(&root, &plain());
        assert!(
            done.message.starts_with("wrote .archmap/github.json: "),
            "{}",
            done.message
        );
        assert_eq!(snapshot(&root).whole_fetched_at, Some(utc(T0 + 60)));
    }

    #[test]
    fn a_snapshot_fetched_otherwise_is_never_fresh_and_is_fetched_whole() {
        let root = root("otherwise", Some(T0));
        for (request, name, reason) in [
            (
                automatic("30m"),
                "web",
                "the snapshot is of another repository",
            ),
            (
                FetchRequest {
                    titles: false,
                    ..automatic("30m")
                },
                "shop",
                "the snapshot was fetched with the other title setting",
            ),
            (
                FetchRequest {
                    max_items: Some(50),
                    ..automatic("30m")
                },
                "shop",
                "the snapshot was fetched with another bound",
            ),
        ] {
            // the snapshot the root holds is a fresh one of acme/shop with
            // titles and a bound of 100
            let fresh = self::root(&format!("otherwise-{name}"), Some(T0));
            let world = world(T0 + 60);
            let done = world.fetch_of(&fresh, &request, name).unwrap();
            assert_eq!(done.outcome, Outcome::Written, "{}", done.message);
            assert!(
                done.message
                    .contains(&format!(" (fetched whole: {reason}): ")),
                "{}",
                done.message
            );
            assert!(world.calls.get() > 0);
        }
        // as asked, it is fresh
        assert!(world(T0 + 60)
            .fetch(&root, &automatic("30m"))
            .message
            .starts_with("fresh: "));
    }

    #[test]
    fn an_automatic_refresh_keeps_the_snapshots_title_setting_and_bound_unless_given() {
        let root = root("kept", None);
        let without = FetchRequest {
            titles: false,
            max_items: Some(50),
            ..plain()
        };
        world(T0).fetch(&root, &without);
        // within the limit it is fresh as it is
        let fresh = world(T0 + 60);
        assert!(fresh
            .fetch(&root, &automatic("30m"))
            .message
            .starts_with("fresh: "));
        assert_eq!(fresh.calls.get(), 0);
        // past it, an update that keeps both, never a whole fetch with titles
        let done = world(T0 + HOUR).fetch(&root, &automatic("30m"));
        assert!(done.message.starts_with("updated "), "{}", done.message);
        let after = snapshot(&root);
        assert!(after.no_titles);
        assert_eq!(after.range.bound, 50);
        // what the command gives still counts
        let done = world(T0 + 2 * HOUR).fetch(
            &root,
            &FetchRequest {
                max_items: Some(100),
                ..automatic("30m")
            },
        );
        assert!(
            done.message
                .contains(" (fetched whole: the snapshot was fetched with another bound): "),
            "{}",
            done.message
        );
        assert!(snapshot(&root).no_titles);
        // and a fetch asked for takes its own defaults, as before
        let done = world(T0 + 3 * HOUR).fetch(&root, &updating());
        assert!(
            done.message.contains(
                " (fetched whole: the snapshot was fetched with the other title setting): "
            ),
            "{}",
            done.message
        );
        assert!(!snapshot(&root).no_titles);
    }

    #[test]
    fn an_origin_that_names_no_github_com_repository_takes_the_snapshot_of_its_owner_and_name() {
        let root = root("alias", Some(T0));
        let path = root.join(DEFAULT_PATH);
        let shown = ".archmap/github.json";
        let origin = |host: &str, owner: &str, name: &str| (host.into(), owner.into(), name.into());
        let github: fn(&str) -> Option<String> = |_| Some("github.com".into());
        let itself: fn(&str) -> Option<String> = |alias| Some(alias.into());
        let silent: fn(&str) -> Option<String> = |_| None;
        let alias = origin("github-work", "Acme", "Shop");
        // an SSH alias of github.com: the snapshot's own repository
        assert_eq!(
            snapshot_repository(&alias, Some(github), &read(&path), shown),
            Ok(origin("github.com", "acme", "shop"))
        );
        // an alias SSH takes elsewhere, or names nothing for, or cannot tell
        assert_eq!(
            snapshot_repository(&alias, Some(itself), &read(&path), shown),
            Err(
                "the one at .archmap/github.json is of github.com, and ssh takes github-work to \
                 github-work"
                    .into()
            )
        );
        assert_eq!(
            snapshot_repository(&alias, Some(silent), &read(&path), shown),
            Err(
                "the one at .archmap/github.json is of github.com, and ssh cannot tell which \
                 host github-work is"
                    .into()
            )
        );
        // an enterprise origin never takes github.com's repository of its name
        let enterprise = origin("ghe.example.com", "acme", "shop");
        assert_eq!(
            snapshot_repository(&enterprise, None, &read(&path), shown),
            Err(
                "the one at .archmap/github.json is of github.com, and `origin` names \
                 ghe.example.com"
                    .into()
            )
        );
        assert_eq!(
            snapshot_repository(&enterprise, Some(itself), &read(&path), shown).unwrap_err(),
            "the one at .archmap/github.json is of github.com, and ssh takes ghe.example.com to \
             ghe.example.com"
        );
        // but takes its own host's
        let mut own = snapshot(&root);
        own.host = "ghe.example.com".into();
        assert_eq!(
            snapshot_repository(&enterprise, None, &Ok(Some(own)), shown),
            Ok(enterprise.clone())
        );
        // another repository, none, or one that cannot be read: why not
        assert_eq!(
            snapshot_repository(
                &origin("github-work", "acme", "web"),
                Some(github),
                &read(&path),
                shown
            ),
            Err("the one at .archmap/github.json is of github.com/acme/shop, not acme/web".into())
        );
        assert_eq!(
            snapshot_repository(&alias, Some(github), &Ok(None), shown),
            Err("there is none at .archmap/github.json".into())
        );
        let unreadable = Err(ReadError::Corrupt("expected value".into()));
        let why = snapshot_repository(&alias, Some(github), &unreadable, shown).unwrap_err();
        assert!(why.starts_with(".archmap/github.json: "), "{why}");
    }

    #[test]
    fn a_repository_taken_from_the_snapshot_is_checked_again_under_the_lock() {
        // another fetch replaced the snapshot after it was read
        let root = root("replaced", Some(T0));
        let world = world(T0 + HOUR);
        let repository = ("github.com".into(), "acme".into(), "web".into());
        let done = fetch_with(
            &root,
            &automatic("30m"),
            (repository, true),
            &mut world.env(),
        )
        .unwrap();
        assert_eq!(
            done,
            not_written(
                ".archmap/github.json is no longer of github.com/acme/web, which `origin` named \
                 through it: nothing fetched"
                    .into()
            )
        );
        assert_eq!(world.calls.get(), 0);
        assert_eq!(snapshot(&root).repository, "acme/shop");
        // still the same, it runs as any refresh does
        let repository = ("github.com".into(), "acme".into(), "shop".into());
        let done = fetch_with(
            &root,
            &automatic("30m"),
            (repository, true),
            &mut world.env(),
        )
        .unwrap();
        assert_eq!(done.outcome, Outcome::Written, "{}", done.message);
    }

    #[test]
    fn a_snapshot_whose_fetch_time_cannot_be_read_is_never_fresh() {
        let root = root("unreadable-time", Some(T0));
        let path = root.join(DEFAULT_PATH);
        let mut edited = snapshot(&root);
        edited.fetched_at = "yesterday".into();
        write(&path, &edited).unwrap();
        let world = world(T0 + 60);
        let done = world.fetch(&root, &automatic("30m"));
        assert_eq!(done.outcome, Outcome::Written);
        assert!(
            done.message
                .contains(" (fetched whole: the snapshot's fetch time is unreadable): "),
            "{}",
            done.message
        );
        assert_eq!(snapshot(&root).fetched_at, utc(T0 + 60));
    }

    #[test]
    fn another_fetch_holding_the_snapshot_stops_a_fetch_before_it_asks_anything() {
        let root = root("busy", Some(T0));
        let held = lock(&root.join(DEFAULT_PATH)).unwrap();
        assert!(matches!(held, Locked::Held(_)));
        let world = world(T0 + 2 * HOUR);
        // an automatic refresh has nothing to do
        assert_eq!(
            world.fetch(&root, &automatic("30m")),
            skipped(
                "busy: another archmap fetch is updating .archmap/github.json: nothing fetched"
                    .into()
            )
        );
        // a fetch asked for did not happen
        for request in [plain(), updating()] {
            assert_eq!(
                world.fetch(&root, &request),
                not_written(
                    "another archmap fetch is updating .archmap/github.json: nothing fetched"
                        .into()
                )
            );
        }
        assert_eq!(world.calls.get(), 0);
        assert_eq!(snapshot(&root).fetched_at, utc(T0));
        // the lock file stays, and the next fetch takes it
        drop(held);
        assert!(root.join(".archmap/github.json.lock").is_file());
        assert_eq!(
            world.fetch(&root, &automatic("30m")).outcome,
            Outcome::Written
        );
    }

    #[test]
    fn what_a_fetch_decides_it_decides_from_the_snapshot_and_status_under_the_lock() {
        let root = root("recheck", Some(T0));
        let path = root.join(DEFAULT_PATH);
        let later = world(T0 + 2 * HOUR);
        // while another fetch holds the lock, it writes a fresh snapshot
        let held = lock(&path).unwrap();
        let mut fresh = snapshot(&root);
        fresh.fetched_at = utc(T0 + 2 * HOUR - 60);
        write(&path, &fresh).unwrap();
        drop(held);
        assert!(later
            .fetch(&root, &automatic("30m"))
            .message
            .starts_with("fresh: "));
        // or fails and records it
        let held = lock(&path).unwrap();
        let mut stale = fresh.clone();
        stale.fetched_at = utc(T0);
        write(&path, &stale).unwrap();
        write_status(
            &status_path(&path),
            &RefreshStatus {
                schema: STATUS_SCHEMA,
                source: "github".into(),
                host: "github.com".into(),
                repository: "acme/shop".into(),
                account: None,
                last_attempt_at: utc(T0 + 2 * HOUR - 60),
                last_error: Some(RefreshError {
                    kind: FailureKind::Network,
                    message: "gh failed: timeout".into(),
                }),
                next_retry_at: Some(utc(T0 + 2 * HOUR + 240)),
                consecutive_failures: 1,
            },
        )
        .unwrap();
        drop(held);
        assert!(later
            .fetch(&root, &automatic("30m"))
            .message
            .starts_with("waiting: "));
        assert_eq!(later.calls.get(), 0);
    }

    #[test]
    fn a_lock_that_cannot_be_taken_is_an_error_not_another_fetch() {
        let root = root("lock-error", None);
        std::fs::write(root.join("file"), "").unwrap();
        let output = root.join("file/github.json");
        let world = world(T0);
        for request in [
            automatic("30m"),
            FetchRequest {
                output: Some(&output),
                ..plain()
            },
        ] {
            let request = FetchRequest {
                output: Some(&output),
                ..request
            };
            let error = world.try_fetch(&root, &request).unwrap_err().to_string();
            assert!(
                error.starts_with("file/github.json: creating its directory: "),
                "{error}"
            );
        }
        assert_eq!(world.calls.get(), 0);
    }

    #[test]
    fn a_failure_keeps_the_snapshot_and_waits_longer_each_time() {
        let root = root("failure", Some(T0));
        let before = bytes(&root);
        let first = T0 + 2 * HOUR;
        let world = World::failing(first, 1, "gh failed: HTTP 502: Bad Gateway");
        let done = world.fetch(&root, &automatic("30m"));
        assert_eq!(
            done,
            not_written(
                "gh failed: HTTP 502: Bad Gateway\n  .archmap/github.json is as it was; the \
                 failure (network) is recorded in .archmap/github.status.json: no automatic \
                 attempt before 2026-09-22 09:25 UTC"
                    .into()
            )
        );
        assert_eq!(bytes(&root), before);
        let recorded = status(&root).unwrap();
        assert_eq!(recorded.consecutive_failures, 1);
        assert_eq!(recorded.next_retry_at, Some(utc(first + 300)));
        assert_eq!(recorded.last_attempt_at, utc(first));
        assert_eq!(recorded.last_error.unwrap().kind, FailureKind::Network);
        // the viewer answered before the failure
        assert_eq!(recorded.account.unwrap().login, "fetcher");
        // within the backoff, nothing is asked
        let waiting = self::world(first + 299);
        let done = waiting.fetch(&root, &automatic("30m"));
        assert_eq!(
            done,
            skipped(
                "waiting: the last attempt, 2026-09-22 09:20 UTC, failed (network); no \
                 automatic attempt before 2026-09-22 09:25 UTC, and a change of account is not \
                 seen until then: nothing fetched (`archmap fetch github` tries now)"
                    .into()
            )
        );
        assert_eq!(waiting.calls.get(), 0);
        // past it, a second failure waits twice as long
        let second = first + 300;
        World::failing(second, 1, "gh failed: timeout").fetch(&root, &automatic("30m"));
        let recorded = status(&root).unwrap();
        assert_eq!(recorded.consecutive_failures, 2);
        assert_eq!(recorded.next_retry_at, Some(utc(second + 600)));
        assert_eq!(bytes(&root), before);
        // a fetch asked for runs within the backoff, and its success clears it
        let asked = self::world(second + 1);
        let done = asked.fetch(&root, &updating());
        assert_eq!(done.outcome, Outcome::Written, "{}", done.message);
        assert!(asked.calls.get() > 0);
        let recorded = status(&root).unwrap();
        assert_eq!(
            (
                recorded.last_error,
                recorded.next_retry_at,
                recorded.consecutive_failures
            ),
            (None, None, 0)
        );
        assert_eq!(
            self::world(second + 2)
                .fetch(&root, &automatic("30m"))
                .outcome,
            Outcome::Skipped
        );
    }

    #[test]
    fn a_host_gh_holds_no_login_for_fails_as_an_attempt_does_and_waits() {
        let root = root("login", None);
        let asked = Cell::new(0);
        let attempt = |now| {
            let world = world(now);
            let mut env = Env {
                logged_in: Box::new(|_| {
                    asked.set(asked.get() + 1);
                    Err(
                        "You are not logged into any GitHub hosts. To log in, run: gh auth login"
                            .into(),
                    )
                }),
                ..world.env()
            };
            let repository = ("ghe.example.com".into(), "acme".into(), "shop".into());
            let done = fetch_with(&root, &automatic("30m"), (repository, false), &mut env).unwrap();
            drop(env);
            assert_eq!(world.calls.get(), 0);
            done
        };
        let done = attempt(T0);
        assert_eq!(
            done,
            not_written(
                "gh is not logged in to ghe.example.com: `gh auth login --hostname \
                 ghe.example.com` first\n  .archmap/github.json is as it was; the failure \
                 (authentication) is recorded in .archmap/github.status.json: no automatic \
                 attempt before 2026-09-22 07:25 UTC"
                    .into()
            )
        );
        let recorded = status(&root).unwrap();
        assert_eq!(recorded.host, "ghe.example.com");
        assert_eq!(recorded.last_error.unwrap().kind, FailureKind::Auth);
        assert_eq!(asked.get(), 1);
        // the next refresh within the wait asks gh nothing, not even its login
        assert!(attempt(T0 + 60).message.starts_with("waiting: "));
        assert_eq!(asked.get(), 1);
    }

    #[test]
    fn a_failure_keeps_its_facts_and_prints_its_words_with_no_absolute_path_or_token() {
        let root = root("private", Some(T0));
        let world = World {
            response: Some(Response::default()),
            ..World::failing(
                T0 + 2 * HOUR,
                1,
                "gh failed: open /Users/some one/My Config/gh/hosts.yml: permission denied \
                 (ghp_0123456789abcdefABCDEF)",
            )
        };
        let done = world.fetch(&root, &automatic("30m"));
        assert!(
            done.message.starts_with(
                "gh failed: open [path]: permission denied ([redacted])\n  .archmap/github.json is \
                 as it was"
            ),
            "{}",
            done.message
        );
        // gh printed no HTTP answer: that is what is kept, not its words
        let recorded = status(&root).unwrap().last_error.unwrap().message;
        assert_eq!(recorded, "no answer came from GitHub");
        let file = std::fs::read_to_string(status_path(&root.join(DEFAULT_PATH))).unwrap();
        for leak in ["/Users/", "some one", "My Config", "hosts.yml", "ghp_"] {
            assert!(!file.contains(leak), "{leak}: {file}");
        }
        // an answer gives its status and its errors' codes
        let world = World {
            response: Some(Response {
                status: Some(404),
                errors: vec!["NOT_FOUND".into()],
                hints: RateHints::default(),
            }),
            ..World::failing(
                T0 + 3 * HOUR,
                1,
                "GitHub answered: Could not resolve to a Repository with the name 'acme/shop'.",
            )
        };
        world.fetch(&root, &plain());
        assert_eq!(
            status(&root).unwrap().last_error.unwrap().message,
            "GitHub answered HTTP 404 with errors NOT_FOUND"
        );
        // and gh that could not run, nothing
        World::failing(
            T0 + 4 * HOUR,
            0,
            "the gh CLI is not installed: see https://cli.github.com",
        )
        .fetch(&root, &plain());
        assert_eq!(
            status(&root).unwrap().last_error.unwrap().message,
            "gh could not be run"
        );
    }

    #[test]
    fn a_snapshot_that_cannot_be_written_keeps_the_file_systems_words_without_its_path() {
        // outside the root, where the snapshot is named by its absolute path
        let elsewhere = root("out dir", None);
        let output = elsewhere.join("github.json");
        std::fs::create_dir_all(output.join("in the way")).unwrap();
        let root = root("unwritable", None);
        let world = world(T0);
        let done = world.fetch(
            &root,
            &FetchRequest {
                output: Some(&output),
                ..plain()
            },
        );
        assert_eq!(done.outcome, Outcome::NotWritten, "{}", done.message);
        assert!(
            done.message.starts_with("[path]: writing it: "),
            "{}",
            done.message
        );
        let file = std::fs::read_to_string(status_path(&output)).unwrap();
        let recorded = read_status(&status_path(&output))
            .unwrap()
            .last_error
            .unwrap();
        assert_eq!(recorded.kind, FailureKind::Other);
        assert!(
            recorded.message.starts_with("the snapshot: writing it: "),
            "{}",
            recorded.message
        );
        assert!(
            !file.contains("out dir") && !file.contains("/var/") && !file.contains("/tmp/"),
            "{file}"
        );
    }

    #[test]
    fn a_login_gh_cannot_confirm_is_no_authentication_failure_without_a_sign_of_one() {
        let host = "ghe.example.com";
        let failed = |printed: &str, recorded: &str, kind| Failure {
            printed: printed.into(),
            recorded: recorded.into(),
            kind,
        };
        // what gh 2.102 prints with no login for the host
        assert_eq!(
            login_failure(
                host,
                "You are not logged into any GitHub hosts. To log in, run: gh auth login\n"
            ),
            failed(
                "gh is not logged in to ghe.example.com: `gh auth login --hostname \
                 ghe.example.com` first",
                "gh is not logged in to ghe.example.com",
                FailureKind::Auth
            )
        );
        // and for a token it could not use, whether the token is bad or the
        // host could not be reached: no sign of which; gh's words are
        // printed and never kept
        assert_eq!(
            login_failure(
                host,
                "ghe.example.com\n  X Failed to log in to ghe.example.com using token \
                 (GH_ENTERPRISE_TOKEN)\n  - Active account: true\n  - The token in \
                 GH_ENTERPRISE_TOKEN is invalid.\n"
            ),
            failed(
                "gh auth status could not confirm a login for ghe.example.com: Failed to log in \
                 to ghe.example.com using token (GH_ENTERPRISE_TOKEN)",
                "gh auth status could not confirm a login for ghe.example.com",
                FailureKind::Other
            )
        );
        assert_eq!(
            login_failure(
                host,
                "ghe.example.com\n  X Timeout trying to log in to ghe.example.com account \
                 someone (keyring)\n"
            )
            .kind,
            FailureKind::Network
        );
        assert_eq!(
            login_failure(host, "").printed,
            "gh auth status could not confirm a login for ghe.example.com: it printed nothing"
        );
        let missing = login_failure(
            host,
            "the gh CLI is not installed: see https://cli.github.com",
        );
        assert_eq!(missing.kind, FailureKind::Other);
    }

    #[test]
    fn the_backoff_stops_doubling_at_six_hours() {
        let root = root("cap", Some(T0));
        let mut now = T0 + 2 * HOUR;
        for _ in 0..10 {
            World::failing(now, 1, "gh failed: timeout").fetch(&root, &automatic("30m"));
            now = seconds(&status(&root).unwrap().next_retry_at.unwrap()).unwrap();
        }
        let recorded = status(&root).unwrap();
        assert_eq!(recorded.consecutive_failures, 10);
        assert_eq!(
            seconds(&recorded.next_retry_at.unwrap()).unwrap()
                - seconds(&recorded.last_attempt_at).unwrap(),
            MAX_BACKOFF
        );
    }

    #[test]
    fn a_rate_limit_waits_as_github_says_when_it_says() {
        let root = root("rate", Some(T0));
        let now = T0 + 2 * HOUR;
        // the response's retry-after first
        let world = World {
            response: Some(Response {
                status: Some(200),
                errors: vec!["RATE_LIMITED".into()],
                hints: RateHints {
                    retry_after: Some(120),
                    remaining: Some(0),
                    reset: Some(now + 900),
                },
            }),
            reset: Some(now + 1_800),
            ..World::failing(now, 1, "GraphQL: API rate limit exceeded for user ID 7.")
        };
        world.fetch(&root, &automatic("30m"));
        let recorded = status(&root).unwrap();
        // its facts, not its words, which name the account's id
        assert_eq!(
            recorded.last_error.unwrap(),
            RefreshError {
                kind: FailureKind::RateLimit,
                message: "GitHub answered HTTP 200 with errors RATE_LIMITED".into()
            }
        );
        assert_eq!(recorded.next_retry_at, Some(utc(now + 120)));
        assert_eq!(world.resets.get(), 0);
    }

    #[test]
    fn when_to_try_again_follows_the_headers_then_the_primary_reset_then_the_backoff() {
        let now = T0;
        let none = &mut || -> Option<i64> { panic!("asked the rate_limit endpoint") };
        let at = |retry_after, remaining, reset| {
            Some(RateHints {
                retry_after,
                remaining,
                reset,
            })
        };
        // retry-after, for either limit
        for kind in [FailureKind::RateLimit, FailureKind::SecondaryRateLimit] {
            assert_eq!(
                retry_at(kind, at(Some(90), None, None), none, now, 300),
                now + 90
            );
        }
        // a limit spent resets when its headers say
        assert_eq!(
            retry_at(
                FailureKind::SecondaryRateLimit,
                at(None, Some(0), Some(now + 700)),
                none,
                now,
                300
            ),
            now + 700
        );
        // a reset with requests left is not this limit's end
        assert_eq!(
            retry_at(
                FailureKind::SecondaryRateLimit,
                at(None, Some(5), Some(now + 700)),
                none,
                now,
                300
            ),
            now + 300
        );
        // without headers, the primary limit asks the endpoint, and the
        // secondary never does: it cannot tell that limit's end
        let asked = Cell::new(0);
        let mut endpoint = || {
            asked.set(asked.get() + 1);
            Some(now + 1_000)
        };
        assert_eq!(
            retry_at(FailureKind::RateLimit, None, &mut endpoint, now, 300),
            now + 1_000
        );
        assert_eq!(asked.get(), 1);
        assert_eq!(
            retry_at(
                FailureKind::SecondaryRateLimit,
                None,
                &mut endpoint,
                now,
                300
            ),
            now + 300
        );
        assert_eq!(asked.get(), 1);
        // an endpoint that answers nothing, or a reset past, waits the backoff
        assert_eq!(
            retry_at(FailureKind::RateLimit, None, &mut || None, now, 600),
            now + 600
        );
        assert_eq!(
            retry_at(
                FailureKind::RateLimit,
                None,
                &mut || Some(now - 1),
                now,
                600
            ),
            now + 600
        );
        // other failures wait the backoff, whatever the headers, and at least a minute
        assert_eq!(
            retry_at(
                FailureKind::Auth,
                at(Some(5), Some(0), Some(now + 9)),
                none,
                now,
                300
            ),
            now + 300
        );
        assert_eq!(
            retry_at(FailureKind::Network, None, none, now, 10),
            now + 60
        );
    }

    #[test]
    fn a_failure_before_the_viewer_answers_is_no_accounts_and_another_account_starts_again() {
        let root = root("account", Some(T0));
        let path = root.join(DEFAULT_PATH);
        let failed = |account: Option<Account>, failures| RefreshStatus {
            schema: STATUS_SCHEMA,
            source: "github".into(),
            host: "github.com".into(),
            repository: "acme/shop".into(),
            account,
            last_attempt_at: utc(T0 + HOUR),
            last_error: Some(RefreshError {
                kind: FailureKind::Auth,
                message: "gh failed: HTTP 401: Bad credentials".into(),
            }),
            next_retry_at: Some(utc(T0 + HOUR + 60)),
            consecutive_failures: failures,
        };
        let before = Some(Account {
            login: "before".into(),
            id: Some(1),
        });
        write_status(&status_path(&path), &failed(before.clone(), 3)).unwrap();
        // the first request fails: whose failure it is, is not known, so it
        // is recorded as no account's, and the backoff keeps growing
        let now = T0 + 2 * HOUR;
        World::failing(now, 0, "gh failed: error connecting to api.github.com")
            .fetch(&root, &automatic("30m"));
        let recorded = status(&root).unwrap();
        assert_eq!(recorded.account, None);
        assert_eq!(recorded.consecutive_failures, 4);
        assert_eq!(recorded.last_error.unwrap().kind, FailureKind::Network);
        // another account answers and fails: its failures count from one
        write_status(&status_path(&path), &failed(before, 3)).unwrap();
        World::failing(now, 1, "gh failed: timeout").fetch(&root, &automatic("30m"));
        let recorded = status(&root).unwrap();
        assert_eq!(recorded.consecutive_failures, 1);
        assert_eq!(recorded.account.unwrap().id, Some(7));
    }

    #[test]
    fn a_failure_no_later_than_the_snapshot_is_past_and_neither_waits_nor_counts() {
        // a fetch wrote the snapshot after the failure and stopped before
        // its status
        let root = root("past-failure", Some(T0 + HOUR));
        let path = root.join(DEFAULT_PATH);
        write_status(
            &status_path(&path),
            &RefreshStatus {
                schema: STATUS_SCHEMA,
                source: "github".into(),
                host: "github.com".into(),
                repository: "acme/shop".into(),
                account: None,
                last_attempt_at: utc(T0),
                last_error: Some(RefreshError {
                    kind: FailureKind::Network,
                    message: "gh failed: timeout".into(),
                }),
                next_retry_at: Some(utc(T0 + 6 * HOUR)),
                consecutive_failures: 6,
            },
        )
        .unwrap();
        let world = World::failing(T0 + 2 * HOUR, 1, "gh failed: timeout");
        let done = world.fetch(&root, &automatic("30m"));
        assert_eq!(done.outcome, Outcome::NotWritten, "{}", done.message);
        assert!(world.calls.get() > 0);
        assert_eq!(status(&root).unwrap().consecutive_failures, 1);
    }

    #[test]
    fn a_status_of_another_repository_is_not_this_ones_backoff() {
        let root = root("other-repository", Some(T0));
        let path = root.join(DEFAULT_PATH);
        write_status(
            &status_path(&path),
            &RefreshStatus {
                schema: STATUS_SCHEMA,
                source: "github".into(),
                host: "github.com".into(),
                repository: "acme/web".into(),
                account: None,
                last_attempt_at: utc(T0 + HOUR),
                last_error: Some(RefreshError {
                    kind: FailureKind::Network,
                    message: "gh failed: timeout".into(),
                }),
                next_retry_at: Some(utc(T0 + 9 * HOUR)),
                consecutive_failures: 5,
            },
        )
        .unwrap();
        let world = World::failing(T0 + 2 * HOUR, 1, "gh failed: timeout");
        world.fetch(&root, &automatic("30m"));
        assert!(world.calls.get() > 0);
        let recorded = status(&root).unwrap();
        assert_eq!(
            (recorded.repository.as_str(), recorded.consecutive_failures),
            ("acme/shop", 1)
        );
    }

    #[test]
    fn an_automatic_refresh_never_overwrites_a_snapshot_it_cannot_read() {
        let root = root("unreadable", None);
        let path = root.join(DEFAULT_PATH);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        for (text, says) in [
            (
                "{\"schema\": 2}",
                "snapshot schema 2, and this archmap reads 1",
            ),
            ("{", "no snapshot: "),
        ] {
            std::fs::write(&path, text).unwrap();
            let world = world(T0);
            let done = world.fetch(&root, &automatic("30m"));
            assert_eq!(done.outcome, Outcome::NotWritten);
            assert!(
                done.message.starts_with(".archmap/github.json: "),
                "{}",
                done.message
            );
            assert!(done.message.contains(says), "{}", done.message);
            assert!(
                done.message.ends_with(
                    ": an automatic refresh never overwrites a snapshot it cannot read, so \
                     nothing was fetched; `archmap fetch github` fetches it again"
                ),
                "{}",
                done.message
            );
            assert_eq!(world.calls.get(), 0);
            assert_eq!(std::fs::read_to_string(&path).unwrap(), text);
            assert_eq!(status(&root), None);
        }
        // one that cannot be read at all
        std::fs::remove_file(&path).unwrap();
        std::fs::create_dir(&path).unwrap();
        let done = world(T0).fetch(&root, &automatic("30m"));
        assert!(
            done.message.starts_with(".archmap/github.json: not read: "),
            "{}",
            done.message
        );
        std::fs::remove_dir(&path).unwrap();
        // a fetch asked for replaces a newer format, and says so
        std::fs::write(&path, "{\"schema\": 2}").unwrap();
        let done = world(T0).fetch(&root, &updating());
        assert!(
            done.message
                .contains(" (fetched whole: the snapshot has a newer format): "),
            "{}",
            done.message
        );
    }

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
