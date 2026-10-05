//! `query '#N'`: an issue or a pull request from the work snapshot, the
//! links the tracker records for it, and its commits against the local
//! history. Every link shown is one the snapshot observed; nothing is
//! derived here but the match of a commit by its SHA.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write;
use std::path::Path;

use anyhow::{bail, Result};
use archmap_core::history::{History, HistoryState};
use archmap_core::work::{
    Item, ItemKind, ItemState, NotVisible, Ref, Relation, RelationType, SeenFrom, Snapshot,
};
use archmap_scan::history::LocalCommit;
use serde::Serialize;

use crate::co_change::{date, short};
use crate::work_line::{day, plural_word, range_line};
use crate::{Format, Workspace};

/// Items per list the text shows; `verbose` lifts it.
const MAX_ITEMS: usize = 10;
/// Unmatched commits looked up in the repository per answer; the rest are
/// counted.
const MAX_LOOKUPS: usize = 50;

/// An item a target names: `#12`, or `owner/name#12`.
pub(crate) struct WorkTarget<'t> {
    repository: Option<&'t str>,
    number: u64,
}

/// `target` as an item, when it is one: `#N` (quoted in a shell, where `#`
/// after a space starts a comment) or `owner/name#N`, unless a file of that
/// name exists.
pub(crate) fn target<'t>(root: &Path, target: &'t str) -> Option<WorkTarget<'t>> {
    let (before, digits) = target.rsplit_once('#')?;
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let number = digits.parse().ok()?;
    let repository = match before {
        "" => None,
        repository if is_repository(repository) && !root.join(target).exists() => Some(repository),
        _ => return None,
    };
    Some(WorkTarget { repository, number })
}

fn is_repository(name: &str) -> bool {
    let part = |p: &str| {
        !p.is_empty()
            && p.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b == b'.')
    };
    name.split_once('/')
        .is_some_and(|(o, n)| part(o) && part(n))
}

/// The answer for `target`, as text or JSON.
pub(crate) fn answer(
    ws: &Workspace,
    target: WorkTarget,
    requested: &str,
    format: Format,
    verbose: bool,
) -> Result<String> {
    let snapshot = match ws.snapshot() {
        Ok(Some(snapshot)) => snapshot,
        Ok(None) => return no_snapshot(ws, requested, format),
        Err(error) => bail!("{error}"),
    };
    if let Some(repository) = target.repository {
        if !repository.eq_ignore_ascii_case(&snapshot.repository) {
            return elsewhere(snapshot, requested, format);
        }
    }
    let view = view(ws, snapshot, target.number, requested);
    Ok(match format {
        Format::Json => crate::json(&view)?,
        Format::Text => text(&view, snapshot, ws.history(), verbose),
    })
}

/// What `query` says of an item.
#[derive(Debug, Serialize)]
struct WorkView<'a> {
    requested: &'a str,
    repository: &'a str,
    number: u64,
    /// The item, when the snapshot's range holds it.
    #[serde(skip_serializing_if = "Option::is_none")]
    item: Option<&'a Item>,
    /// Its links, each from the end the item is.
    relations: Vec<Linked<'a>>,
    /// A pull request's commits against the local history.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    commits: Vec<LocalMatch>,
    #[serde(skip_serializing_if = "Option::is_none")]
    merge_commit: Option<LocalMatch>,
    /// Per relation type fetched, the links this item's end cannot see.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    not_seen: Vec<NotSeen>,
    coverage: Coverage<'a>,
}

/// A link of the item.
#[derive(Debug, Serialize)]
struct Linked<'a> {
    #[serde(rename = "type")]
    kind: RelationType,
    /// `from` when the item is the relation's `from` end.
    end: End,
    /// The other end.
    other: &'a Ref,
    /// The other end, when the snapshot holds it.
    #[serde(skip_serializing_if = "Option::is_none")]
    other_item: Option<Brief<'a>>,
    /// A commit end against the local history.
    #[serde(skip_serializing_if = "Option::is_none")]
    local: Option<LocalMatch>,
    observed: &'a [String],
    #[serde(skip_serializing_if = "Option::is_none")]
    at: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    will_close: Option<bool>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
enum End {
    From,
    To,
}

/// Another item, as a link names it.
#[derive(Debug, Serialize)]
struct Brief<'a> {
    kind: ItemKind,
    state: ItemState,
    #[serde(skip_serializing_if = "Option::is_none")]
    title: Option<&'a str>,
}

/// A commit against the local history: matched by its SHA, or why not.
#[derive(Debug, Clone, Serialize)]
struct LocalMatch {
    sha: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    matched_by: Option<&'static str>,
    /// Its committer time, when matched.
    #[serde(skip_serializing_if = "Option::is_none")]
    time: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<&'static str>,
}

/// Links a type records on the other end only, which this item's end
/// cannot see when that end is outside the range.
#[derive(Debug, Serialize)]
struct NotSeen {
    #[serde(rename = "type")]
    kind: RelationType,
    /// The end this item would be.
    end: End,
}

#[derive(Debug, Serialize)]
struct Coverage<'a> {
    source: &'a str,
    fetched_at: &'a str,
    range: &'a archmap_core::work::Range,
    relation_types: &'a [RelationType],
    #[serde(skip_serializing_if = "<[_]>::is_empty")]
    unavailable: &'a [RelationType],
    #[serde(skip_serializing_if = "NotVisible::is_empty")]
    not_visible: &'a NotVisible,
    /// Ends of the item's links that are outside the snapshot's range.
    outside_range: usize,
    /// Ends of the item's links in other repositories.
    other_repositories: usize,
}

fn view<'a>(
    ws: &Workspace,
    snapshot: &'a Snapshot,
    number: u64,
    requested: &'a str,
) -> WorkView<'a> {
    let item = snapshot.item(number);
    let this = Ref::Item { item: number };
    let mut relations: Vec<(&Relation, End)> = snapshot
        .relations
        .iter()
        .filter_map(|r| match (r.from == this, r.to == this) {
            (true, _) => Some((r, End::From)),
            (false, true) => Some((r, End::To)),
            _ => None,
        })
        .collect();
    relations.sort_by(|(a, ea), (b, eb)| (a.kind, *ea, &a.at).cmp(&(b.kind, *eb, &b.at)));
    // every commit the answer names, matched at once
    let mut shas: BTreeSet<&str> = BTreeSet::new();
    if let Some(item) = item {
        shas.extend(item.commits.iter().map(String::as_str));
        shas.extend(item.merge_commit.as_deref());
    }
    for (relation, end) in &relations {
        if let Ref::Commit {
            commit,
            repository: None,
        } = other(relation, *end)
        {
            shas.insert(commit);
        }
    }
    let matches = local_matches(ws, &shas);
    let local = |sha: &str| matches.get(sha).cloned();
    let (mut outside_range, mut other_repositories) = (0, 0);
    let linked: Vec<Linked> = relations
        .iter()
        .map(|&(relation, end)| {
            let other = other(relation, end);
            let other_item = match other {
                Ref::Item { item } => {
                    let found = snapshot.item(*item);
                    outside_range += usize::from(found.is_none());
                    found.map(|i| Brief {
                        kind: i.kind,
                        state: i.state,
                        title: i.title.as_deref(),
                    })
                }
                Ref::Foreign { .. } => {
                    other_repositories += 1;
                    None
                }
                Ref::Commit { .. } => None,
            };
            let local = match other {
                Ref::Commit {
                    commit,
                    repository: None,
                } => local(commit),
                Ref::Commit { commit, .. } => Some(LocalMatch {
                    sha: commit.clone(),
                    matched_by: None,
                    time: None,
                    reason: Some("foreign"),
                }),
                _ => None,
            };
            Linked {
                kind: relation.kind,
                end,
                other,
                other_item,
                local,
                observed: &relation.observed,
                at: relation.at.as_deref(),
                will_close: relation.will_close,
            }
        })
        .collect();
    let commits = item
        .map(|i| i.commits.iter().filter_map(|c| local(c)).collect())
        .unwrap_or_default();
    let merge_commit = item.and_then(|i| i.merge_commit.as_deref()).and_then(local);
    // a type seen from one end only: this item, as the other end, sees none
    // of the links whose seeing end is outside the range; only the ends an
    // item of its kind can be (a pull request closes, an issue is
    // duplicated)
    let kind = item.map(|i| i.kind);
    let not_seen = snapshot
        .relation_types
        .iter()
        .filter_map(|&relation| {
            let end = match relation.seen_from() {
                SeenFrom::Both => return None,
                SeenFrom::From => End::To,
                SeenFrom::To => End::From,
            };
            let applies = match relation {
                RelationType::ClosedBy => kind == Some(ItemKind::PullRequest),
                RelationType::DuplicateOf => kind == Some(ItemKind::Issue),
                RelationType::CrossReferenced => kind.is_some(),
                _ => false,
            };
            applies.then_some(NotSeen {
                kind: relation,
                end,
            })
        })
        .collect();
    WorkView {
        requested,
        repository: &snapshot.repository,
        number,
        item,
        relations: linked,
        commits,
        merge_commit,
        not_seen,
        coverage: Coverage {
            source: &snapshot.source,
            fetched_at: &snapshot.fetched_at,
            range: &snapshot.range,
            relation_types: &snapshot.relation_types,
            unavailable: &snapshot.unavailable,
            not_visible: &snapshot.not_visible,
            outside_range,
            other_repositories,
        },
    }
}

/// The end of `relation` that is not the item.
fn other(relation: &Relation, end: End) -> &Ref {
    match end {
        End::From => &relation.to,
        End::To => &relation.from,
    }
}

/// Each commit against the history read: matched by SHA, else looked up in
/// the repository (at most [`MAX_LOOKUPS`]).
fn local_matches(ws: &Workspace, shas: &BTreeSet<&str>) -> BTreeMap<String, LocalMatch> {
    let history = ws.history();
    let read: BTreeMap<&str, i64> = history
        .commits
        .iter()
        .map(|c| (c.id.as_str(), c.time))
        .collect();
    let mut found = BTreeMap::new();
    let mut unmatched = Vec::new();
    for &sha in shas {
        match read.get(sha) {
            Some(&time) => {
                found.insert(
                    sha.to_owned(),
                    LocalMatch {
                        sha: sha.to_owned(),
                        matched_by: Some("sha"),
                        time: Some(time),
                        reason: None,
                    },
                );
            }
            None => unmatched.push(sha.to_owned()),
        }
    }
    let readable = matches!(history.state, HistoryState::Read { .. });
    let asked: Vec<String> = unmatched.iter().take(MAX_LOOKUPS).cloned().collect();
    let looked = readable
        .then(|| archmap_scan::history::local_commits(ws.root(), &asked))
        .flatten();
    for sha in unmatched {
        let reason = match (readable, looked.as_ref().map(|l| l.get(&sha))) {
            (false, _) => "no_history",
            (true, Some(Some(LocalCommit::Missing))) => "no_local_commit_with_same_sha",
            (true, Some(Some(LocalCommit::BeyondRead))) => "beyond_history_read",
            (true, Some(Some(LocalCommit::NotInHead))) => "not_in_head",
            _ => "not_looked_up",
        };
        found.insert(
            sha.clone(),
            LocalMatch {
                sha,
                matched_by: None,
                time: None,
                reason: Some(reason),
            },
        );
    }
    found
}

/// The answer as text.
fn text(view: &WorkView, snapshot: &Snapshot, history: &History, verbose: bool) -> String {
    let cap = match verbose {
        true => usize::MAX,
        false => MAX_ITEMS,
    };
    let shallow = matches!(history.state, HistoryState::Read { shallow: true, .. });
    let mut out = String::new();
    let mut truncated = false;
    match view.item {
        Some(item) => {
            let title = item
                .title
                .as_deref()
                .map(|t| format!(": {t}"))
                .unwrap_or_default();
            let _ = writeln!(out, "#{} {}{title}", item.number, kind_word(item.kind));
            let _ = writeln!(out, "  {}", state_line(item));
        }
        None => {
            let _ = writeln!(out, "#{}: outside the fetched range", view.number);
        }
    }
    let _ = writeln!(out, "  snapshot: {}", range_line(snapshot));

    if let Some(item) = view.item.filter(|i| i.kind == ItemKind::PullRequest) {
        let matched = view
            .commits
            .iter()
            .filter(|c| c.matched_by.is_some())
            .count();
        let mut heading = format!(
            "\nCommits: {}, {matched} in the local history",
            item.commit_count.unwrap_or(item.commits.len())
        );
        if item.commit_count.is_some_and(|n| n > item.commits.len()) {
            let _ = write!(heading, " (the tracker lists {})", item.commits.len());
        }
        let _ = writeln!(out, "{heading}");
        for commit in view.commits.iter().take(cap) {
            let _ = writeln!(out, "  {}", commit_line(commit, shallow));
        }
        truncated |= view.commits.len() > cap;
        if view.commits.len() > cap {
            let _ = writeln!(out, "  +{} more", view.commits.len() - cap);
        }
        if let Some(merge) = &view.merge_commit {
            let _ = writeln!(out, "Merge commit: {}", commit_line(merge, shallow));
        }
    }

    // the links by type and end, in a fixed order
    let mut groups: BTreeMap<(RelationType, End), Vec<&Linked>> = BTreeMap::new();
    for linked in &view.relations {
        groups
            .entry((linked.kind, linked.end))
            .or_default()
            .push(linked);
    }
    if !groups.is_empty() {
        out.push('\n');
    }
    let reopened = view.item.is_some_and(|i| i.state == ItemState::Open);
    for ((kind, end), list) in &groups {
        let mut heading = heading(*kind, *end).to_owned();
        if *kind == RelationType::ClosedBy && *end == End::From && reopened {
            heading.push_str(" (open now)");
        }
        let names: Vec<String> = list
            .iter()
            .take(cap)
            .map(|l| other_line(l, &snapshot.repository, shallow))
            .collect();
        truncated |= list.len() > cap;
        let more = match list.len() > cap {
            true => format!(", +{} more", list.len() - cap),
            false => String::new(),
        };
        let _ = writeln!(out, "{heading}: {}{more}", names.join("; "));
    }
    if groups.is_empty() && view.item.is_some() {
        let _ = writeln!(out, "\nLinks: none of the types fetched");
    }

    let mut not_traced = Vec::new();
    // a range that holds every item leaves only other repositories unseen
    let whole = snapshot.range.updated_since.is_none() && !snapshot.range.reached_bound;
    for gap in &view.not_seen {
        not_traced.push(format!("  {}", not_seen_line(gap, view.number, whole)));
    }
    if view.coverage.outside_range > 0 {
        not_traced.push(format!(
            "  outside the range: {} of these links name items the snapshot does not hold",
            view.coverage.outside_range
        ));
    }
    if !snapshot.unavailable.is_empty() {
        let names: Vec<&str> = snapshot.unavailable.iter().map(|t| t.as_str()).collect();
        not_traced.push(format!(
            "  not available on this host: {}",
            names.join(", ")
        ));
    }
    let hidden = &snapshot.not_visible;
    let mut unseen: Vec<String> = hidden
        .links
        .iter()
        .map(|(kind, n)| {
            plural_word(
                *n,
                &format!("{} end", kind.as_str()),
                &format!("{} ends", kind.as_str()),
            )
        })
        .collect();
    if hidden.events > 0 {
        unseen.push(plural_word(
            hidden.events,
            "timeline event",
            "timeline events",
        ));
    }
    if hidden.items > 0 {
        unseen.push(plural_word(hidden.items, "item", "items"));
    }
    if !unseen.is_empty() {
        not_traced.push(format!(
            "  not visible: {} the fetching account could not see",
            unseen.join(", ")
        ));
    }
    if !not_traced.is_empty() {
        let _ = writeln!(out, "\n{}", crate::query_text::NOT_TRACED);
        for line in not_traced {
            let _ = writeln!(out, "{line}");
        }
    }
    if truncated {
        let _ = writeln!(
            out,
            "\nLists are capped; JSON lists every entry with all evidence."
        );
    }
    out
}

fn kind_word(kind: ItemKind) -> &'static str {
    match kind {
        ItemKind::Issue => "issue",
        ItemKind::PullRequest => "pull request",
    }
}

fn state_word(state: ItemState) -> &'static str {
    match state {
        ItemState::Open => "open",
        ItemState::Closed => "closed",
        ItemState::Merged => "merged",
    }
}

/// `merged 2026-09-20`, `closed as completed 2026-09-20`, `open`.
fn state_line(item: &Item) -> String {
    let day = |t: &Option<String>| t.as_deref().map(day).unwrap_or_default().to_owned();
    match item.state {
        ItemState::Open => "open".to_owned(),
        ItemState::Merged => format!("merged {}", day(&item.merged_at)),
        ItemState::Closed => match item.state_reason.as_deref() {
            Some(reason) => format!(
                "closed as {} {}",
                reason.replace('_', " "),
                day(&item.closed_at)
            ),
            None => format!("closed {}", day(&item.closed_at)),
        },
    }
}

/// What a link's other end is: `#12 issue, closed`, `acme/web#4 (another
/// repository)`, `#45 (outside the fetched range)`, a commit with its local
/// match, and the link's time and will-close mark.
fn other_line(linked: &Linked, repository: &str, shallow: bool) -> String {
    let mut line = match linked.other {
        Ref::Item { item } => match &linked.other_item {
            Some(brief) => format!(
                "#{item} {}, {}",
                kind_word(brief.kind),
                state_word(brief.state)
            ),
            None => format!("#{item} (outside the fetched range)"),
        },
        Ref::Foreign {
            repository: other,
            number,
        } if other != repository => {
            format!("{other}#{number} (another repository)")
        }
        Ref::Foreign { number, .. } => format!("#{number}"),
        Ref::Commit { commit, repository } => match (&linked.local, repository) {
            (Some(local), None) => commit_line(local, shallow),
            (_, Some(other)) => format!("{} (a commit of {other})", short(commit)),
            (None, None) => short(commit).to_owned(),
        },
    };
    // a commit end gives its own date
    if let Some(at) = linked
        .at
        .filter(|_| !matches!(linked.other, Ref::Commit { .. }))
    {
        let _ = write!(line, ", {}", day(at));
    }
    if linked.will_close == Some(true) {
        line.push_str(" (will close)");
    }
    line
}

/// `a1b2c3d matched by sha (2026-09-18)`, or why it is not.
fn commit_line(commit: &LocalMatch, shallow: bool) -> String {
    let sha = short(&commit.sha);
    match (commit.matched_by, commit.reason) {
        (Some(_), _) => format!(
            "{sha} matched by sha ({})",
            commit.time.map(date).unwrap_or_default()
        ),
        (None, Some("no_local_commit_with_same_sha")) if shallow => {
            format!("{sha} no local commit with the same sha (the clone is shallow)")
        }
        (None, Some("no_local_commit_with_same_sha")) => {
            format!("{sha} no local commit with the same sha")
        }
        (None, Some("beyond_history_read")) => {
            format!("{sha} an ancestor of HEAD beyond the history read")
        }
        (None, Some("not_in_head")) => format!("{sha} in the repository, not in HEAD's history"),
        (None, Some("no_history")) => format!("{sha} not looked up (no local history)"),
        (None, Some("foreign")) => format!("{sha} a commit of another repository"),
        _ => format!("{sha} not looked up (more than {MAX_LOOKUPS} unmatched)"),
    }
}

/// The heading of the links of `kind` from the item's `end`.
fn heading(kind: RelationType, end: End) -> &'static str {
    match (kind, end) {
        (RelationType::SubIssue, End::From) => "Sub-issues",
        (RelationType::SubIssue, End::To) => "Sub-issue of",
        (RelationType::BlockedBy, End::From) => "Blocked by",
        (RelationType::BlockedBy, End::To) => "Blocks",
        (RelationType::Closes, End::From) => "Closes",
        (RelationType::Closes, End::To) => "Closing references from",
        (RelationType::Linked, End::From) => "Linked to",
        (RelationType::Linked, End::To) => "Linked from",
        (RelationType::ClosedBy, End::From) => "Closed by",
        (RelationType::ClosedBy, End::To) => "Closed",
        (RelationType::CrossReferenced, End::From) => "Cross-references",
        (RelationType::CrossReferenced, End::To) => "Cross-referenced by",
        (RelationType::Referenced, End::From) => "References",
        (RelationType::Referenced, End::To) => "Referenced by commits",
        (RelationType::DuplicateOf, End::From) => "Duplicate of",
        (RelationType::DuplicateOf, End::To) => "Duplicates",
    }
}

/// What a type seen from one end hides from an item at the other: the
/// links whose recording end is outside the range, or, when the range holds
/// every item, in another repository.
fn not_seen_line(gap: &NotSeen, number: u64, whole: bool) -> String {
    let beyond = match whole {
        true => "in other repositories",
        false => "outside the range",
    };
    match gap.kind {
        RelationType::CrossReferenced => format!(
            "cross-references: seen on the referenced item's timeline, so references from \
             #{number} to items {beyond} are not seen"
        ),
        RelationType::ClosedBy => format!(
            "closings: seen on the closed issue's timeline, so issues {beyond} that #{number} \
             closed are not seen"
        ),
        RelationType::DuplicateOf => format!(
            "duplicates: seen on the duplicate, so duplicates of #{number} {beyond} are not seen"
        ),
        kind => format!(
            "{}: seen from one end only, so links of #{number} {beyond} are not seen",
            kind.as_str()
        ),
    }
}

/// Without a snapshot: the answer says so and where it looked.
fn no_snapshot(ws: &Workspace, requested: &str, format: Format) -> Result<String> {
    let path = ws.snapshot_path();
    Ok(match format {
        Format::Json => crate::json(&serde_json::json!({
            "requested": requested,
            "snapshot": null,
            "looked_at": path,
        }))?,
        Format::Text => format!(
            "{requested}: no work snapshot\n  looked at {path} (`archmap fetch github` writes one)\n"
        ),
    })
}

/// A repository the snapshot does not hold.
fn elsewhere(snapshot: &Snapshot, requested: &str, format: Format) -> Result<String> {
    Ok(match format {
        Format::Json => crate::json(&serde_json::json!({
            "requested": requested,
            "repository": snapshot.repository,
            "in_snapshot": false,
        }))?,
        Format::Text => format!(
            "{requested}: not in this snapshot (it holds {})\n",
            snapshot.repository
        ),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_target_is_an_item_only_when_written_as_one() {
        let root = Path::new("/nonexistent-root");
        let t = target(root, "#12").unwrap();
        assert_eq!((t.repository, t.number), (None, 12));
        let t = target(root, "acme/shop#7").unwrap();
        assert_eq!((t.repository, t.number), (Some("acme/shop"), 7));
        for not in ["#", "#x", "#1a", "src/a.ts", "a/b/c#3", "acme#3", "C#"] {
            assert!(target(root, not).is_none(), "{not}");
        }
    }
}
