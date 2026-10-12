//! Fetch a GitHub snapshot through the GraphQL API, with the gh CLI as the
//! only transport: gh holds the token (`GH_TOKEN`, `GITHUB_TOKEN` or its
//! login) and archmap never sees it. Only an explicit fetch runs this;
//! nothing else in archmap reaches the network.
//!
//! The fetch asks the schema first and leaves out the link types whose
//! fields the host lacks, reads the issues and pull requests updated since a
//! date, newest first up to a bound, pages every nested connection to its
//! end, and fails whole on any error: a snapshot is complete within its
//! range or absent.

use std::collections::BTreeSet;
use std::io::Write;
use std::process::{Command, Stdio};

use archmap_core::work::{
    Account, FailureKind, Item, ItemKind, ItemState, NotVisible, Range, Ref, Relation,
    RelationType, SinceRule, Snapshot, WORK_SCHEMA,
};
use serde_json::{json, Value};

/// Issues and pull requests per page; nested connections per page.
const ISSUES_PAGE: usize = 50;
const PULLS_PAGE: usize = 25;
const LINKS_PAGE: usize = 20;
const EVENTS_PAGE: usize = 50;
const COMMITS_PAGE: usize = 100;

/// What to fetch.
#[derive(Debug, Clone)]
pub struct FetchOptions {
    pub host: String,
    pub owner: String,
    pub name: String,
    /// Items updated since this time (RFC 3339), and what set it; `None`
    /// for every item.
    pub since: Option<String>,
    pub since_rule: SinceRule,
    /// Items read per kind at most.
    pub max_items: usize,
    pub titles: bool,
    /// When the fetch starts (RFC 3339, UTC).
    pub now: String,
    /// Items updated since this time are read again after both walks, so
    /// one updated during the fetch, which moves behind the walk's cursor,
    /// is not lost: the start, less a margin for clocks that disagree.
    pub reread_since: String,
}

/// One GraphQL request (`{"query", "variables"}`) to its response's JSON.
pub type Run<'a> = dyn FnMut(&Value) -> Result<Value, String> + 'a;

/// What GitHub's response headers said of its rate limits.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RateHints {
    /// `retry-after`: seconds to wait before retrying.
    pub retry_after: Option<i64>,
    /// `x-ratelimit-remaining`.
    pub remaining: Option<i64>,
    /// `x-ratelimit-reset`: when the primary limit resets, in seconds since
    /// the epoch.
    pub reset: Option<i64>,
}

/// What gh's last call gave, as facts safe to keep and show: the HTTP
/// status, the codes of the errors GitHub's GraphQL answer names, and the
/// rate-limit headers; never a message's words.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Response {
    /// `None` when no HTTP answer came.
    pub status: Option<u16>,
    /// Each error's `type` or `extensions.code` (`RATE_LIMITED`,
    /// `NOT_FOUND`, `undefinedField`), sorted, once each.
    pub errors: Vec<String>,
    pub hints: RateHints,
}

/// The gh CLI as the transport for a host: `gh api graphql --include
/// --input -`, the request on stdin, prompts off. It keeps what the last
/// response was, failed or not (gh prints the headers of a failed one
/// too), so a failure can say when to retry and be recorded without gh's
/// words. Its error message is passed on, without token shapes; nothing of
/// its environment is.
#[derive(Debug)]
pub struct Gh {
    host: String,
    /// What the last call gave; `None` when gh could not be run.
    pub last: Option<Response>,
}

impl Gh {
    pub fn new(host: &str) -> Self {
        Gh {
            host: host.to_owned(),
            last: None,
        }
    }

    /// One request.
    pub fn call(&mut self, request: &Value) -> Result<Value, String> {
        self.last = None;
        let host = self.host.as_str();
        let mut child = Command::new("gh")
            .args([
                "api",
                "graphql",
                "--include",
                "--hostname",
                host,
                "--input",
                "-",
            ])
            .env("GH_PROMPT_DISABLED", "1")
            .env("NO_COLOR", "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|error| match error.kind() {
                std::io::ErrorKind::NotFound => {
                    "the gh CLI is not installed: see https://cli.github.com".to_owned()
                }
                _ => format!("running gh: {error}"),
            })?;
        let body = serde_json::to_vec(request).map_err(|e| e.to_string())?;
        child
            .stdin
            .take()
            .ok_or("gh took no input")?
            .write_all(&body)
            .map_err(|e| format!("writing to gh: {e}"))?;
        let out = child
            .wait_with_output()
            .map_err(|e| format!("running gh: {e}"))?;
        let (headers, answer) = response(&out.stdout);
        self.last = Some(Response {
            status: status_code(&out.stdout),
            errors: error_codes(answer),
            hints: rate_hints(&headers),
        });
        if !out.status.success() {
            let message = String::from_utf8_lossy(&out.stderr);
            let first = redact(message.lines().next().unwrap_or("").trim());
            // the way out of a login problem, for that problem only
            return Err(match classify(&first) {
                FailureKind::Auth => format!(
                    "gh failed: {first} (log in with `gh auth login --hostname {host}`, or set \
                     GH_TOKEN)"
                ),
                _ => format!("gh failed: {first}"),
            });
        }
        serde_json::from_slice(answer).map_err(|e| format!("gh answered no JSON: {e}"))
    }
}

/// The headers and the body of what `gh api --include` printed: a status
/// line and headers up to a blank line, then the body; all body when it
/// starts otherwise.
pub fn response(out: &[u8]) -> (Vec<(String, String)>, &[u8]) {
    if !out.starts_with(b"HTTP/") {
        return (Vec::new(), out);
    }
    let split = [&b"\r\n\r\n"[..], &b"\n\n"[..]]
        .iter()
        .filter_map(|sep| {
            out.windows(sep.len())
                .position(|w| w == *sep)
                .map(|at| (at, sep.len()))
        })
        .min();
    let Some((at, len)) = split else {
        return (Vec::new(), &out[out.len()..]);
    };
    let head = String::from_utf8_lossy(&out[..at]);
    let headers = head
        .lines()
        .skip(1)
        .filter_map(|line| line.split_once(':'))
        .map(|(name, value)| (name.trim().to_ascii_lowercase(), value.trim().to_owned()))
        .collect();
    (headers, &out[at + len..])
}

/// The HTTP status of what `gh api --include` printed.
pub fn status_code(out: &[u8]) -> Option<u16> {
    if !out.starts_with(b"HTTP/") {
        return None;
    }
    let line = out.split(|b| *b == b'\n').next()?;
    let line = String::from_utf8_lossy(line);
    line.split_whitespace().nth(1)?.parse().ok()
}

/// The codes of the errors a GraphQL answer names: each error's `type`, else
/// its `extensions.code`, when it is a plain identifier of at most 40
/// characters; never their messages.
pub fn error_codes(body: &[u8]) -> Vec<String> {
    let Ok(answer) = serde_json::from_slice::<Value>(body) else {
        return Vec::new();
    };
    let mut codes: Vec<String> = answer["errors"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|e| e["type"].as_str().or(e["extensions"]["code"].as_str()))
        .filter(|code| {
            !code.is_empty()
                && code.len() <= 40
                && code.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
        })
        .map(str::to_owned)
        .collect();
    codes.sort();
    codes.dedup();
    codes
}

/// The rate-limit headers among `headers`.
pub fn rate_hints(headers: &[(String, String)]) -> RateHints {
    let number = |name: &str| {
        headers
            .iter()
            .find(|(n, _)| n == name)
            .and_then(|(_, v)| v.parse::<i64>().ok())
    };
    RateHints {
        retry_after: number("retry-after"),
        remaining: number("x-ratelimit-remaining"),
        reset: number("x-ratelimit-reset"),
    }
}

/// When GitHub's GraphQL rate limit of the account gh uses on `host`
/// resets (seconds since the epoch), asked of the REST `rate_limit`
/// endpoint, which counts against no primary limit, though it may against
/// the secondary one; `None` when it cannot tell.
pub fn rate_limit_reset(host: &str) -> Option<i64> {
    let out = Command::new("gh")
        .args(["api", "rate_limit", "--hostname", host])
        .env("GH_PROMPT_DISABLED", "1")
        .env("NO_COLOR", "1")
        .stdin(Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let answer: Value = serde_json::from_slice(&out.stdout).ok()?;
    answer["resources"]["graphql"]["reset"].as_i64()
}

/// What a failed fetch ran into, as far as its message tells: the words
/// GitHub and gh use for a rate limit, a login or access problem, and the
/// network or the servers; anything else is `Other`.
pub fn classify(message: &str) -> FailureKind {
    let m = message.to_ascii_lowercase();
    let any = |words: &[&str]| words.iter().any(|w| m.contains(w));
    if any(&["secondary rate limit", "abuse detection"]) {
        FailureKind::SecondaryRateLimit
    } else if any(&["rate limit", "ratelimit", "rate_limited"]) {
        FailureKind::RateLimit
    } else if any(&[
        "http 401",
        "bad credentials",
        "requires authentication",
        "not logged in",
        "gh auth login",
        "authentication",
        "http 403",
        "resource not accessible",
        "saml",
        "token",
    ]) {
        FailureKind::Auth
    } else if any(&[
        // a host name not found; GraphQL says `Could not resolve to a
        // Repository` of a repository missing or hidden, which is no network's
        "could not resolve host",
        "no such host",
        "timeout",
        "timed out",
        "connect",
        "network",
        "http 500",
        "http 502",
        "http 503",
        "http 504",
        "unexpected eof",
        "tls",
    ]) {
        FailureKind::Network
    } else {
        FailureKind::Other
    }
}

/// `message` without anything shaped like a GitHub token or another long
/// token, the credentials of a URL, or an absolute path of the local file
/// system (`[path]`), as a status file or an answer may keep it.
pub fn redact(message: &str) -> String {
    const PREFIXES: [&str; 6] = ["github_pat_", "ghp_", "gho_", "ghu_", "ghs_", "ghr_"];
    let mut out = String::with_capacity(message.len());
    let mut rest = message;
    while !rest.is_empty() {
        let found = PREFIXES
            .iter()
            .filter_map(|p| rest.find(p).map(|at| (at, p.len())))
            .min();
        let Some((at, len)) = found else {
            out.push_str(rest);
            break;
        };
        out.push_str(&rest[..at]);
        let tail = &rest[at + len..];
        let end = tail
            .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
            .unwrap_or(tail.len());
        out.push_str("[redacted]");
        rest = &tail[end..];
    }
    // `scheme://user:secret@host` keeps `scheme://host`
    let mut cleaned = String::with_capacity(out.len());
    let mut rest = out.as_str();
    while let Some(at) = rest.find("://") {
        cleaned.push_str(&rest[..at + 3]);
        let after = &rest[at + 3..];
        let authority_end = after
            .find(|c: char| c == '/' || c.is_whitespace())
            .unwrap_or(after.len());
        rest = match after[..authority_end].rfind('@') {
            Some(at) => &after[at + 1..],
            None => after,
        };
    }
    cleaned.push_str(rest);
    without_paths(&without_long_tokens(&cleaned))
}

/// `message` with each word of 32 or more letters, digits, `_` and `-`
/// that holds both letters and digits as `[redacted]`: a token of a shape
/// not known, as an enterprise host or another tool may give.
fn without_long_tokens(message: &str) -> String {
    let mut out = String::with_capacity(message.len());
    let mut word = String::new();
    let flush = |word: &mut String, out: &mut String| {
        let secret = word.chars().count() >= 32
            && word.chars().any(|c| c.is_ascii_alphabetic())
            && word.chars().any(|c| c.is_ascii_digit());
        out.push_str(if secret { "[redacted]" } else { word });
        word.clear();
    };
    for c in message.chars() {
        if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
            word.push(c);
        } else {
            flush(&mut word, &mut out);
            out.push(c);
        }
    }
    flush(&mut word, &mut out);
    out
}

/// `message` with each absolute path of the local file system as `[path]`:
/// a Unix path, a Windows path or a `file:` URL that starts a word, up to
/// a quote, a bracket, `,`, `;`, `:`, ` (` or the end of the line. A path
/// may hold spaces, so the words after one on its line go with it unless
/// one of those ends it first. A path relative to the root stays, and so
/// does a URL of the network.
fn without_paths(message: &str) -> String {
    let chars: Vec<char> = message.chars().collect();
    let starts_word =
        |i: usize| i == 0 || chars[i - 1].is_whitespace() || "([{\"'`=<,".contains(chars[i - 1]);
    let delimits = |c: char| ")]}\"'`>,;:".contains(c);
    let ends = |c: char| c.is_whitespace() || delimits(c);
    // where a path that runs to `i` stops
    let ends_at = |i: usize| {
        let c = chars[i];
        c == '\n' || c == '\r' || delimits(c) || (c == ' ' && chars.get(i + 1) == Some(&'('))
    };
    let at = |i: usize| chars.get(i).copied();
    let mut out = String::with_capacity(message.len());
    let mut i = 0;
    while i < chars.len() {
        let rest: String = chars[i..chars.len().min(i + 7)].iter().collect();
        // how much of the start to pass before a `:` ends the path
        let start = if !starts_word(i) {
            None
        } else if chars[i] == '/' && at(i + 1).is_some_and(|c| c != '/' && !ends(c)) {
            Some(1)
        } else if chars[i].is_ascii_alphabetic()
            && at(i + 1) == Some(':')
            && matches!(at(i + 2), Some('\\' | '/'))
        {
            Some(3)
        } else if rest.eq_ignore_ascii_case("file://") {
            Some(7)
        } else {
            None
        };
        let Some(skip) = start else {
            out.push(chars[i]);
            i += 1;
            continue;
        };
        let mut end = i + skip;
        while end < chars.len() && !ends_at(end) {
            end += 1;
        }
        out.push_str("[path]");
        i = end;
    }
    out
}

/// What the host offers and who the fetch runs as.
struct Probe {
    types: BTreeSet<RelationType>,
    unavailable: Vec<RelationType>,
    account: Option<Account>,
}

/// Ask the host which link types it offers and which account answers.
fn probe(run: &mut Run) -> Result<Probe, String> {
    let (fields, account) = schema(run)?;
    let has = |owner: &str, field: &str| fields.contains(&(owner.to_owned(), field.to_owned()));
    let closing = has("Issue", "closedByPullRequestsReferences.userLinkedOnly")
        && has("PullRequest", "closingIssuesReferences.userLinkedOnly");
    let offered: Vec<(RelationType, bool)> = vec![
        (
            RelationType::SubIssue,
            has("Issue", "subIssues") && has("Issue", "parent"),
        ),
        (
            RelationType::BlockedBy,
            has("Issue", "blockedBy") && has("Issue", "blocking"),
        ),
        (RelationType::Closes, closing),
        (RelationType::Linked, closing),
        (RelationType::ClosedBy, true),
        (RelationType::CrossReferenced, true),
        (RelationType::Referenced, true),
        (RelationType::DuplicateOf, has("Issue", "duplicateOf")),
    ];
    let types: BTreeSet<RelationType> = offered
        .iter()
        .filter(|(_, ok)| *ok)
        .map(|(t, _)| *t)
        .collect();
    let unavailable: Vec<RelationType> = offered
        .iter()
        .filter(|(_, ok)| !*ok)
        .map(|(t, _)| *t)
        .collect();
    Ok(Probe {
        types,
        unavailable,
        account,
    })
}

/// Fetch a snapshot whole.
pub fn fetch(options: &FetchOptions, run: &mut Run) -> Result<Snapshot, String> {
    let probe = probe(run)?;
    let mut fetch = Fetch::new(options, probe.types.clone());
    let since = options.since.as_deref();
    let issues_bounded = fetch.issues(run, since, options.max_items)?;
    let pulls_bounded = fetch.pulls(run, since, options.max_items)?;
    fetch.again(run)?;
    let reached_bound = issues_bounded.is_some() || pulls_bounded.is_some();
    let effective_since = [issues_bounded, pulls_bounded].into_iter().flatten().max();
    let range = Range {
        updated_since: options.since.clone(),
        since_rule: options.since_rule,
        bound: options.max_items,
        reached_bound,
        effective_since,
        issues: 0,
        pull_requests: 0,
    };
    Ok(fetch.snapshot(probe, range, Some(options.now.clone())))
}

/// What an update gave: the snapshot with the items updated since its
/// fetch merged in, or why it must be fetched whole instead.
#[derive(Debug)]
pub enum Update {
    Merged {
        snapshot: Box<Snapshot>,
        read: usize,
    },
    Whole {
        reason: String,
    },
}

/// Update `previous` with the issues and pull requests updated since its
/// fetch began, less five minutes. It is fetched whole instead when it
/// was fetched another way: by another account, from another host or
/// repository, with other link types or another title setting or bound,
/// by an archmap that recorded no whole fetch or account; when its last
/// whole fetch began before `whole_after`; and when more items changed
/// than the bound reads.
pub fn update(
    options: &FetchOptions,
    run: &mut Run,
    previous: &Snapshot,
    whole_after: &str,
) -> Result<Update, String> {
    let whole = |reason: &str| {
        Ok(Update::Whole {
            reason: reason.to_owned(),
        })
    };
    let Some(whole_at) = previous.whole_fetched_at.as_deref() else {
        return whole("the snapshot records no whole fetch");
    };
    if whole_at < whole_after {
        return whole("its last whole fetch is older than the limit");
    }
    let repository = format!("{}/{}", options.owner, options.name);
    let asked = (options.titles, options.max_items);
    if let Some(reason) = fetched_otherwise(previous, &options.host, &repository, asked) {
        return whole(reason);
    }
    let Some(since) = crate::work::seconds(&previous.fetched_at) else {
        return whole("the snapshot's fetch time is unreadable");
    };
    let probe = probe(run)?;
    match (&probe.account, &previous.account) {
        (Some(now), Some(then)) if same_account(now, then) => {}
        (_, None) => return whole("the snapshot records no account"),
        _ => return whole("the snapshot was fetched as another account"),
    }
    if previous.relation_types != probe.types.iter().copied().collect::<Vec<_>>()
        || previous.unavailable != probe.unavailable
    {
        return whole("the host offers other link types than when the snapshot was fetched");
    }
    let mut fetch = Fetch::new(options, probe.types.clone());
    let since = crate::work::utc(since - 300);
    // more changed than the bound reads: an update would miss some
    let bounded = "more items changed since its fetch than the bound reads";
    if fetch
        .issues(run, Some(&since), options.max_items)?
        .is_some()
    {
        return whole(bounded);
    }
    if fetch.pulls(run, Some(&since), options.max_items)?.is_some() {
        return whole(bounded);
    }
    fetch.again(run)?;
    let read = {
        let mut numbers: Vec<u64> = fetch.items.iter().map(|i| i.number).collect();
        numbers.sort_unstable();
        numbers.dedup();
        numbers.len()
    };
    let changes = fetch.snapshot(probe, previous.range.clone(), None);
    let mut snapshot = Box::new(previous.clone());
    snapshot.merge(changes);
    Ok(Update::Merged { snapshot, read })
}

/// Why `previous` is not a snapshot of what a fetch of `repository` on
/// `host` with `(titles, max_items)` asks for, as far as the file tells:
/// another source, host or repository, the other title setting or another
/// bound. `None` when it is.
pub fn fetched_otherwise(
    previous: &Snapshot,
    host: &str,
    repository: &str,
    (titles, max_items): (bool, usize),
) -> Option<&'static str> {
    if previous.source != "github"
        || previous.host != host
        || !previous.repository.eq_ignore_ascii_case(repository)
    {
        Some("the snapshot is of another repository")
    } else if previous.no_titles == titles {
        Some("the snapshot was fetched with the other title setting")
    } else if previous.range.bound != max_items {
        Some("the snapshot was fetched with another bound")
    } else {
        None
    }
}

/// The same account: by its numeric id when both have one, which stays
/// when the login is renamed, else by login.
fn same_account(a: &Account, b: &Account) -> bool {
    match (a.id, b.id) {
        (Some(x), Some(y)) => x == y,
        _ => a.login.eq_ignore_ascii_case(&b.login),
    }
}

/// The fields of `Issue` and `PullRequest` the host offers, as `(type,
/// field)`, and `(type, field.argument)` for their arguments.
type Fields = BTreeSet<(String, String)>;

/// The fields the host offers, and the account that answers.
fn schema(run: &mut Run) -> Result<(Fields, Option<Account>), String> {
    let request = json!({
        "query": "query Schema { issue: __type(name: \"Issue\") { fields { name args { name } } } \
                  pull: __type(name: \"PullRequest\") { fields { name args { name } } } \
                  viewer { login databaseId } }"
    });
    let answer = data(run(&request)?)?;
    let account = answer["viewer"]["login"].as_str().map(|login| Account {
        login: login.to_owned(),
        id: answer["viewer"]["databaseId"].as_u64(),
    });
    let mut found = BTreeSet::new();
    for (key, owner) in [("issue", "Issue"), ("pull", "PullRequest")] {
        for field in answer[key]["fields"].as_array().into_iter().flatten() {
            let Some(name) = field["name"].as_str() else {
                continue;
            };
            found.insert((owner.to_owned(), name.to_owned()));
            for arg in field["args"].as_array().into_iter().flatten() {
                if let Some(arg) = arg["name"].as_str() {
                    found.insert((owner.to_owned(), format!("{name}.{arg}")));
                }
            }
        }
    }
    Ok((found, account))
}

/// A response's `data`, or its errors as one message, each with where in
/// the answer it arose: one item the account cannot read fails the fetch,
/// and the path says which.
fn data(response: Value) -> Result<Value, String> {
    if let Some(errors) = response.get("errors").and_then(Value::as_array) {
        let messages: Vec<String> = errors
            .iter()
            .map(|e| {
                let message = e["message"].as_str().unwrap_or("an error");
                let path: Vec<String> = e["path"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|p| match p {
                        Value::String(s) => s.clone(),
                        other => other.to_string(),
                    })
                    .collect();
                match path.is_empty() {
                    true => message.to_owned(),
                    false => format!("{message} (at {})", path.join(".")),
                }
            })
            .collect();
        return Err(format!("GitHub answered: {}", messages.join("; ")));
    }
    response
        .get("data")
        .cloned()
        .ok_or_else(|| "GitHub answered without data".to_owned())
}

struct Fetch<'o> {
    options: &'o FetchOptions,
    types: BTreeSet<RelationType>,
    repository: String,
    items: Vec<Item>,
    relations: Vec<Relation>,
    /// Items no page could show.
    not_visible: NotVisible,
    /// What the item being read has that the account cannot see.
    current: NotVisible,
}

impl<'o> Fetch<'o> {
    fn new(options: &'o FetchOptions, types: BTreeSet<RelationType>) -> Self {
        Fetch {
            options,
            types,
            repository: format!("{}/{}", options.owner, options.name),
            items: Vec::new(),
            relations: Vec::new(),
            not_visible: NotVisible::default(),
            current: NotVisible::default(),
        }
    }

    /// What changed while the walks ran, read again: the last read stays.
    fn again(&mut self, run: &mut Run) -> Result<(), String> {
        let again = self.options.reread_since.clone();
        self.issues(run, Some(&again), usize::MAX)?;
        self.pulls(run, Some(&again), usize::MAX)?;
        Ok(())
    }

    /// The items and links read as a snapshot over `range`; `whole_at` for
    /// a whole fetch.
    fn snapshot(self, probe: Probe, range: Range, whole_at: Option<String>) -> Snapshot {
        let mut snapshot = Snapshot {
            schema: WORK_SCHEMA,
            source: "github".to_owned(),
            host: self.options.host.clone(),
            repository: self.repository,
            fetched_at: self.options.now.clone(),
            whole_fetched_at: whole_at,
            account: probe.account,
            range,
            relation_types: self.types.iter().copied().collect(),
            unavailable: probe.unavailable,
            no_titles: !self.options.titles,
            items: self.items,
            relations: self.relations,
            not_visible: NotVisible::default(),
        };
        snapshot.normalize();
        // an item read twice counts once: its last read stays
        let walked = self.not_visible.items;
        snapshot.not_visible = snapshot.items_not_visible();
        snapshot.not_visible.items = walked;
        snapshot.range.issues = snapshot.count(ItemKind::Issue);
        snapshot.range.pull_requests = snapshot.count(ItemKind::PullRequest);
        snapshot
    }
}

/// A nested connection to page on from a node, with what its nodes are.
struct Nested {
    /// The connection as the query writes it, alias first
    /// (`closes: closedByPullRequestsReferences`).
    field: &'static str,
    /// Its arguments beyond `first` and `after`.
    args: &'static str,
    /// The selection of one node.
    selection: &'static str,
}

const REF: &str = "number repository { nameWithOwner }";
const EVENT: &str = "__typename \
    ... on CrossReferencedEvent { referencedAt willCloseTarget source { __typename \
      ... on Issue { number repository { nameWithOwner } } \
      ... on PullRequest { number repository { nameWithOwner } } } } \
    ... on ReferencedEvent { createdAt commit { oid } commitRepository { nameWithOwner } } \
    ... on ClosedEvent { createdAt closer { __typename ... on Commit { oid repository { nameWithOwner } } \
      ... on PullRequest { number repository { nameWithOwner } } } }";

impl Fetch<'_> {
    fn has(&self, kind: RelationType) -> bool {
        self.types.contains(&kind)
    }

    /// The nested connections an issue's page asks for.
    fn issue_connections(&self) -> Vec<Nested> {
        let mut nested = Vec::new();
        if self.has(RelationType::SubIssue) {
            nested.push(Nested {
                field: "subIssues",
                args: "",
                selection: REF,
            });
        }
        if self.has(RelationType::BlockedBy) {
            nested.push(Nested {
                field: "blockedBy",
                args: "",
                selection: REF,
            });
            nested.push(Nested {
                field: "blocking",
                args: "",
                selection: REF,
            });
        }
        if self.has(RelationType::Closes) {
            nested.push(Nested {
                field: "closes: closedByPullRequestsReferences",
                args: ", includeClosedPrs: true, excludeUserLinked: true",
                selection: REF,
            });
            nested.push(Nested {
                field: "linked: closedByPullRequestsReferences",
                args: ", includeClosedPrs: true, userLinkedOnly: true",
                selection: REF,
            });
        }
        nested.push(Nested {
            field: "events: timelineItems",
            args: ", itemTypes: [CROSS_REFERENCED_EVENT, REFERENCED_EVENT, CLOSED_EVENT]",
            selection: EVENT,
        });
        nested
    }

    fn pull_connections(&self) -> Vec<Nested> {
        let mut nested = vec![Nested {
            field: "commits",
            args: "",
            selection: "commit { oid }",
        }];
        if self.has(RelationType::Closes) {
            nested.push(Nested {
                field: "closes: closingIssuesReferences",
                args: ", excludeUserLinked: true",
                selection: REF,
            });
            nested.push(Nested {
                field: "linked: closingIssuesReferences",
                args: ", userLinkedOnly: true",
                selection: REF,
            });
        }
        nested.push(Nested {
            field: "events: timelineItems",
            args: ", itemTypes: [CROSS_REFERENCED_EVENT, REFERENCED_EVENT]",
            selection: EVENT,
        });
        nested
    }

    /// Read the issues updated since `since`, at most `bound`; when the
    /// bound stopped the read, the oldest update read.
    fn issues(
        &mut self,
        run: &mut Run,
        since: Option<&str>,
        bound: usize,
    ) -> Result<Option<String>, String> {
        let mut scalars = String::from(
            "id number title state stateReason(enableDuplicate: true) createdAt updatedAt closedAt",
        );
        if self.has(RelationType::SubIssue) {
            scalars.push_str(&format!(" parent {{ {REF} }}"));
        }
        if self.has(RelationType::DuplicateOf) {
            scalars.push_str(&format!(" duplicateOf {{ {REF} }}"));
        }
        let nested = self.issue_connections();
        let query = format!(
            "query Issues($owner: String!, $name: String!, $since: DateTime, $cursor: String) {{ \
             repository(owner: $owner, name: $name) {{ nameWithOwner \
             issues(first: {ISSUES_PAGE}, after: $cursor, orderBy: {{field: UPDATED_AT, \
             direction: DESC}}, filterBy: {{since: $since}}) {{ pageInfo {{ hasNextPage endCursor }} \
             nodes {{ {scalars} {} }} }} }} }}",
            connections(&nested, false)
        );
        let mut cursor: Option<String> = None;
        let mut count = 0;
        loop {
            let variables = json!({
                "owner": self.options.owner,
                "name": self.options.name,
                "since": since,
                "cursor": cursor,
            });
            let answer = data(run(&json!({"query": query, "variables": variables}))?)?;
            let repository = &answer["repository"];
            if let Some(name) = repository["nameWithOwner"].as_str() {
                self.repository = name.to_owned();
            }
            let page = &repository["issues"];
            for node in page["nodes"].as_array().into_iter().flatten() {
                if node.is_null() || node["number"].as_u64().is_none() {
                    self.not_visible.items += 1;
                    continue;
                }
                if count == bound {
                    // one more exists: the bound stops the read here
                    let oldest = self
                        .items
                        .iter()
                        .filter(|i| i.kind == ItemKind::Issue)
                        .map(|i| i.updated_at.clone())
                        .min();
                    return Ok(oldest);
                }
                let node = self.complete(run, node, &nested, "Issue")?;
                self.issue(&node);
                count += 1;
            }
            match next(page) {
                Some(next) => cursor = Some(next),
                None => return Ok(None),
            }
        }
    }

    /// Read the pull requests, newest update first, up to the first one
    /// updated before `since` and at most `bound`; when the bound stopped
    /// the read, the oldest update read.
    fn pulls(
        &mut self,
        run: &mut Run,
        since: Option<&str>,
        bound: usize,
    ) -> Result<Option<String>, String> {
        let nested = self.pull_connections();
        let query = format!(
            "query PullRequests($owner: String!, $name: String!, $cursor: String) {{ \
             repository(owner: $owner, name: $name) {{ \
             pullRequests(first: {PULLS_PAGE}, after: $cursor, orderBy: {{field: UPDATED_AT, \
             direction: DESC}}) {{ pageInfo {{ hasNextPage endCursor }} \
             nodes {{ id number title state createdAt updatedAt closedAt mergedAt \
             mergeCommit {{ oid }} {} }} }} }} }}",
            connections(&nested, true)
        );
        let mut cursor: Option<String> = None;
        let mut count = 0;
        loop {
            let variables = json!({
                "owner": self.options.owner,
                "name": self.options.name,
                "cursor": cursor,
            });
            let answer = data(run(&json!({"query": query, "variables": variables}))?)?;
            let page = &answer["repository"]["pullRequests"];
            for node in page["nodes"].as_array().into_iter().flatten() {
                if node.is_null() || node["number"].as_u64().is_none() {
                    self.not_visible.items += 1;
                    continue;
                }
                let updated = node["updatedAt"].as_str().unwrap_or("");
                if since.is_some_and(|since| updated < since) {
                    return Ok(None);
                }
                if count == bound {
                    let oldest = self
                        .items
                        .iter()
                        .filter(|i| i.kind == ItemKind::PullRequest)
                        .map(|i| i.updated_at.clone())
                        .min();
                    return Ok(oldest);
                }
                let node = self.complete(run, node, &nested, "PullRequest")?;
                self.pull(&node);
                count += 1;
            }
            match next(page) {
                Some(next) => cursor = Some(next),
                None => return Ok(None),
            }
        }
    }

    /// `node` with every nested connection paged to its end.
    fn complete(
        &self,
        run: &mut Run,
        node: &Value,
        nested: &[Nested],
        kind: &str,
    ) -> Result<Value, String> {
        let mut node = node.clone();
        for connection in nested {
            let key = connection.field.split(':').next().unwrap_or("").trim();
            let mut cursor = next(&node[key]);
            while let Some(after) = cursor {
                let query = format!(
                    "query More($id: ID!, $cursor: String) {{ node(id: $id) {{ ... on {kind} {{ \
                     {}(first: {}, after: $cursor{}) {{ pageInfo {{ hasNextPage endCursor }} \
                     nodes {{ {} }} }} }} }} }}",
                    connection.field,
                    page_size(connection.field),
                    connection.args,
                    connection.selection
                );
                let variables = json!({"id": node["id"], "cursor": after});
                let answer = data(run(&json!({"query": query, "variables": variables}))?)?;
                let more = &answer["node"][key];
                let extra = more["nodes"].as_array().cloned().unwrap_or_default();
                if let Some(nodes) = node[key]["nodes"].as_array_mut() {
                    nodes.extend(extra);
                }
                cursor = next(more);
            }
        }
        Ok(node)
    }

    fn reference(&mut self, value: &Value, kind: RelationType) -> Option<Ref> {
        let (Some(number), Some(repository)) = (
            value["number"].as_u64(),
            value["repository"]["nameWithOwner"].as_str(),
        ) else {
            *self.current.links.entry(kind).or_default() += 1;
            return None;
        };
        Some(match repository.eq_ignore_ascii_case(&self.repository) {
            true => Ref::Item { item: number },
            false => Ref::Foreign {
                repository: repository.to_owned(),
                number,
            },
        })
    }

    fn commit(&self, oid: &str, repository: Option<&str>) -> Ref {
        Ref::Commit {
            commit: oid.to_owned(),
            repository: repository
                .filter(|r| !r.eq_ignore_ascii_case(&self.repository))
                .map(str::to_owned),
        }
    }

    fn link(&mut self, kind: RelationType, from: Ref, to: Ref, observed: &str) -> &mut Relation {
        self.relations.push(Relation {
            kind,
            from,
            to,
            observed: vec![observed.to_owned()],
            at: None,
            will_close: None,
        });
        self.relations.last_mut().expect("just pushed")
    }

    /// Each node of `connection` as a reference, the unreadable ones
    /// counted for `kind`.
    fn refs(&mut self, connection: &Value, kind: RelationType) -> Vec<Ref> {
        connection["nodes"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .filter_map(|node| self.reference(node, kind))
            .collect()
    }

    fn issue(&mut self, node: &Value) {
        // a page skips a node without a number, counting it
        let number = node["number"].as_u64().unwrap_or_default();
        let this = Ref::Item { item: number };
        let closed = node["state"].as_str() == Some("CLOSED");
        self.items.push(Item {
            kind: ItemKind::Issue,
            number,
            id: string(&node["id"]),
            title: self.title(node),
            state: match closed {
                true => ItemState::Closed,
                false => ItemState::Open,
            },
            state_reason: node["stateReason"]
                .as_str()
                .filter(|_| closed)
                .map(str::to_ascii_lowercase),
            created_at: string(&node["createdAt"]),
            updated_at: string(&node["updatedAt"]),
            closed_at: node["closedAt"].as_str().map(str::to_owned),
            merged_at: None,
            merge_commit: None,
            commits: Vec::new(),
            commit_count: None,
            not_visible: NotVisible::default(),
        });
        if self.has(RelationType::SubIssue) {
            if !node["parent"].is_null() {
                if let Some(parent) = self.reference(&node["parent"], RelationType::SubIssue) {
                    self.link(RelationType::SubIssue, parent, this.clone(), "Issue.parent");
                }
            }
            for child in self.refs(&node["subIssues"], RelationType::SubIssue) {
                self.link(
                    RelationType::SubIssue,
                    this.clone(),
                    child,
                    "Issue.subIssues",
                );
            }
        }
        if self.has(RelationType::BlockedBy) {
            for blocker in self.refs(&node["blockedBy"], RelationType::BlockedBy) {
                self.link(
                    RelationType::BlockedBy,
                    this.clone(),
                    blocker,
                    "Issue.blockedBy",
                );
            }
            for blocked in self.refs(&node["blocking"], RelationType::BlockedBy) {
                self.link(
                    RelationType::BlockedBy,
                    blocked,
                    this.clone(),
                    "Issue.blocking",
                );
            }
        }
        if self.has(RelationType::DuplicateOf) && !node["duplicateOf"].is_null() {
            if let Some(canonical) = self.reference(&node["duplicateOf"], RelationType::DuplicateOf)
            {
                self.link(
                    RelationType::DuplicateOf,
                    this.clone(),
                    canonical,
                    "Issue.duplicateOf",
                );
            }
        }
        if self.has(RelationType::Closes) {
            for pull in self.refs(&node["closes"], RelationType::Closes) {
                self.link(
                    RelationType::Closes,
                    pull,
                    this.clone(),
                    "Issue.closedByPullRequestsReferences(excludeUserLinked)",
                );
            }
            for pull in self.refs(&node["linked"], RelationType::Linked) {
                self.link(
                    RelationType::Linked,
                    pull,
                    this.clone(),
                    "Issue.closedByPullRequestsReferences(userLinkedOnly)",
                );
            }
        }
        self.events(&node["events"], &this);
        self.close_item();
    }

    /// Give the item just read what of it the account could not see.
    fn close_item(&mut self) {
        let unseen = std::mem::take(&mut self.current);
        if let Some(item) = self.items.last_mut() {
            item.not_visible = unseen;
        }
    }

    fn pull(&mut self, node: &Value) {
        // a page skips a node without a number, counting it
        let number = node["number"].as_u64().unwrap_or_default();
        let this = Ref::Item { item: number };
        let commits: Vec<String> = node["commits"]["nodes"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|c| c["commit"]["oid"].as_str().map(str::to_owned))
            .collect();
        self.items.push(Item {
            kind: ItemKind::PullRequest,
            number,
            id: string(&node["id"]),
            title: self.title(node),
            state: match node["state"].as_str() {
                Some("MERGED") => ItemState::Merged,
                Some("CLOSED") => ItemState::Closed,
                _ => ItemState::Open,
            },
            state_reason: None,
            created_at: string(&node["createdAt"]),
            updated_at: string(&node["updatedAt"]),
            closed_at: node["closedAt"].as_str().map(str::to_owned),
            merged_at: node["mergedAt"].as_str().map(str::to_owned),
            merge_commit: node["mergeCommit"]["oid"].as_str().map(str::to_owned),
            commit_count: node["commits"]["totalCount"].as_u64().map(|n| n as usize),
            commits,
            not_visible: NotVisible::default(),
        });
        if self.has(RelationType::Closes) {
            for issue in self.refs(&node["closes"], RelationType::Closes) {
                self.link(
                    RelationType::Closes,
                    this.clone(),
                    issue,
                    "PullRequest.closingIssuesReferences(excludeUserLinked)",
                );
            }
            for issue in self.refs(&node["linked"], RelationType::Linked) {
                self.link(
                    RelationType::Linked,
                    this.clone(),
                    issue,
                    "PullRequest.closingIssuesReferences(userLinkedOnly)",
                );
            }
        }
        self.events(&node["events"], &this);
        self.close_item();
    }

    /// The links an item's timeline records: cross-references to it,
    /// commits that reference it, and, for an issue, what closed it.
    fn events(&mut self, events: &Value, this: &Ref) {
        for event in events["nodes"].as_array().cloned().unwrap_or_default() {
            match event["__typename"].as_str() {
                Some("CrossReferencedEvent") => {
                    let Some(source) =
                        self.reference(&event["source"], RelationType::CrossReferenced)
                    else {
                        continue;
                    };
                    let link = self.link(
                        RelationType::CrossReferenced,
                        source,
                        this.clone(),
                        "CrossReferencedEvent",
                    );
                    link.at = event["referencedAt"].as_str().map(str::to_owned);
                    link.will_close = event["willCloseTarget"].as_bool();
                }
                Some("ReferencedEvent") => {
                    let Some(oid) = event["commit"]["oid"].as_str() else {
                        *self
                            .current
                            .links
                            .entry(RelationType::Referenced)
                            .or_default() += 1;
                        continue;
                    };
                    let commit =
                        self.commit(oid, event["commitRepository"]["nameWithOwner"].as_str());
                    let link = self.link(
                        RelationType::Referenced,
                        commit,
                        this.clone(),
                        "ReferencedEvent",
                    );
                    link.at = event["createdAt"].as_str().map(str::to_owned);
                }
                Some("ClosedEvent") => {
                    let closer = &event["closer"];
                    let to = match closer["__typename"].as_str() {
                        Some("Commit") => closer["oid"].as_str().map(|oid| {
                            self.commit(oid, closer["repository"]["nameWithOwner"].as_str())
                        }),
                        Some("PullRequest") => self.reference(closer, RelationType::ClosedBy),
                        // closed by hand, or by a project
                        _ => None,
                    };
                    if let Some(to) = to {
                        let link = self.link(
                            RelationType::ClosedBy,
                            this.clone(),
                            to,
                            "ClosedEvent.closer",
                        );
                        link.at = event["createdAt"].as_str().map(str::to_owned);
                    }
                }
                // a node the account cannot see, of any type asked for
                _ => self.current.events += 1,
            }
        }
    }

    fn title(&self, node: &Value) -> Option<String> {
        self.options
            .titles
            .then(|| node["title"].as_str().map(str::to_owned))
            .flatten()
    }
}

/// The nested connections of a page's node, each with its page info.
fn connections(nested: &[Nested], pulls: bool) -> String {
    nested
        .iter()
        .map(|c| {
            let count = match (pulls, c.field) {
                (true, "commits") => " totalCount",
                _ => "",
            };
            format!(
                "{}(first: {}{}) {{{count} pageInfo {{ hasNextPage endCursor }} nodes {{ {} }} }}",
                c.field,
                page_size(c.field),
                c.args,
                c.selection
            )
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn page_size(field: &str) -> usize {
    match field {
        "commits" => COMMITS_PAGE,
        f if f.starts_with("events") => EVENTS_PAGE,
        _ => LINKS_PAGE,
    }
}

/// The cursor of a connection's next page, when it has one.
fn next(connection: &Value) -> Option<String> {
    let info = &connection["pageInfo"];
    match info["hasNextPage"].as_bool() {
        Some(true) => info["endCursor"].as_str().map(str::to_owned),
        _ => None,
    }
}

fn string(value: &Value) -> String {
    value.as_str().unwrap_or_default().to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_failure_is_classified_by_its_words() {
        for (message, kind) in [
            (
                "GraphQL: API rate limit exceeded for user ID 1.",
                FailureKind::RateLimit,
            ),
            (
                "You have exceeded a secondary rate limit.",
                FailureKind::SecondaryRateLimit,
            ),
            ("gh failed: HTTP 401: Bad credentials", FailureKind::Auth),
            (
                "To get started with GitHub CLI, please run:  gh auth login",
                FailureKind::Auth,
            ),
            (
                "gh failed: error connecting to api.github.com",
                FailureKind::Network,
            ),
            ("gh failed: HTTP 502: Bad Gateway", FailureKind::Network),
            // a repository that does not exist or is hidden is no network failure
            (
                "gh failed: GraphQL: Could not resolve to a Repository with the name \
                 'acme/shopx'. (repository)",
                FailureKind::Other,
            ),
            (
                "gh failed: dial tcp: lookup api.github.com: no such host",
                FailureKind::Network,
            ),
            (
                "curl: (6) Could not resolve host: ghe.example.com",
                FailureKind::Network,
            ),
            ("something else", FailureKind::Other),
        ] {
            assert_eq!(classify(message), kind, "{message}");
        }
    }

    #[test]
    fn a_message_keeps_no_absolute_path_and_no_long_token() {
        for (message, kept) in [
            (
                "gh failed: open /Users/someone/.config/gh/hosts.yml: permission denied",
                "gh failed: open [path]: permission denied",
            ),
            (
                "[path]: writing it: Permission denied (os error 13)",
                "[path]: writing it: Permission denied (os error 13)",
            ),
            (
                "/srv/someone/shop/github.json: writing it: Read-only file system",
                "[path]: writing it: Read-only file system",
            ),
            (
                "writing it: C:\\Users\\someone\\shop\\github.json (os error 5)",
                "writing it: [path] (os error 5)",
            ),
            (
                "reading 'file:///home/someone/x.json' failed",
                "reading '[path]' failed",
            ),
            // a path with spaces goes whole, up to what ends it
            (
                "gh failed: open /Users/some one/My Config/gh/hosts.yml: permission denied",
                "gh failed: open [path]: permission denied",
            ),
            (
                "writing it: C:\\Users\\some one\\shop\\github.json (os error 5)",
                "writing it: [path] (os error 5)",
            ),
            (
                "could not read /home/some one/x.json\nsecond line",
                "could not read [path]\nsecond line",
            ),
            (
                "open \"/Users/some one/x.json\" and /tmp/other dir/y.json",
                "open \"[path]\" and [path]",
            ),
            (
                "bad credential 0123456789abcdef0123456789abcdef01234567 given",
                "bad credential [redacted] given",
            ),
            // what stays: paths below the root, URLs, addresses, words
            (
                ".archmap/github.json: writing it: No space left on device",
                ".archmap/github.json: writing it: No space left on device",
            ),
            (
                "Post \"https://api.github.com/graphql\": proxyconnect tcp: dial tcp \
                 127.0.0.1:9: connect: connection refused",
                "Post \"https://api.github.com/graphql\": proxyconnect tcp: dial tcp \
                 127.0.0.1:9: connect: connection refused",
            ),
            (
                "gh failed: HTTP 404: Not Found (https://ghe.example.com/api/graphql) a / b",
                "gh failed: HTTP 404: Not Found (https://ghe.example.com/api/graphql) a / b",
            ),
            (
                "the token in GH_ENTERPRISE_TOKEN is invalid",
                "the token in GH_ENTERPRISE_TOKEN is invalid",
            ),
        ] {
            assert_eq!(redact(message), kept, "{message}");
        }
    }

    #[test]
    fn a_response_gives_its_status_and_its_errors_codes_never_their_words() {
        let out = b"HTTP/2.0 200 OK\r\nX-Ratelimit-Remaining: 0\r\n\r\n{\"errors\": [\
            {\"type\": \"RATE_LIMITED\", \"message\": \"API rate limit exceeded for user ID 7.\"},\
            {\"extensions\": {\"code\": \"undefinedField\"}, \"message\": \"Field 'x' doesn't exist\"},\
            {\"type\": \"RATE_LIMITED\"},\
            {\"type\": \"not a code\"}]}";
        assert_eq!(status_code(out), Some(200));
        let (_, body) = response(out);
        assert_eq!(error_codes(body), ["RATE_LIMITED", "undefinedField"]);
        assert_eq!(
            status_code(b"HTTP/2.0 401 Unauthorized\r\n\r\n{}"),
            Some(401)
        );
        // no HTTP answer: gh printed nothing, or a body alone
        assert_eq!(status_code(b""), None);
        assert_eq!(status_code(b"{\"data\": {}}"), None);
        assert!(error_codes(b"not json").is_empty());
    }

    #[test]
    fn a_message_keeps_no_token_or_credentials() {
        assert_eq!(
            redact("bad token ghp_ABCdef123 and github_pat_11AA_bb, then ghs_x."),
            "bad token [redacted] and [redacted], then [redacted]."
        );
        assert_eq!(
            redact("fetching https://ci:secret@ghe.example.com/api failed"),
            "fetching https://ghe.example.com/api failed"
        );
        assert_eq!(redact("no secret here"), "no secret here");
    }

    #[test]
    fn a_response_with_headers_gives_its_rate_limits_and_its_body() {
        let out = b"HTTP/2.0 200 OK\r\nX-Ratelimit-Remaining: 0\r\nX-Ratelimit-Reset: 1790000000\r\nRetry-After: 60\r\n\r\n{\"data\":{}}";
        let (headers, body) = response(out);
        assert_eq!(body, b"{\"data\":{}}");
        assert_eq!(
            rate_hints(&headers),
            RateHints {
                retry_after: Some(60),
                remaining: Some(0),
                reset: Some(1_790_000_000)
            }
        );
        // a body alone, as an older gh or a stub prints it
        let (headers, body) = response(b"{\"data\":{}}");
        assert!(headers.is_empty());
        assert_eq!(body, b"{\"data\":{}}");
    }
}
