//! `query '#N'`'s `Code`: the files an item's work changed, through the
//! commits the history read holds. A pull request starts from its own
//! commit list and merge commit, kept apart; an issue from the pull
//! requests and commits it links, each by the link types the snapshot
//! records. Every step is an observed link or a match by SHA; files are
//! named as the commits wrote them and, after a rename, as HEAD holds them.

use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write;

use archmap_core::co_change::{commit_files, CommitFile};
use archmap_core::history::{History, HistoryState};
use archmap_core::work::{Item, ItemKind, ItemState, Ref, RelationType, Snapshot};
use archmap_core::{ArchitectureGraph, ComponentId};
use serde::Serialize;

use crate::work_line::{date, plural_word, short};

/// Files listed per list in text; `verbose` lifts it.
const MAX_FILES: usize = 10;
/// Pull requests and commits listed in text.
const MAX_WAYS: usize = 10;

/// The files an item's work changed.
#[derive(Debug, Serialize)]
pub(crate) struct Code<'a> {
    /// The files, each once by the path HEAD holds it at (or its last
    /// path), over every way.
    files: usize,
    /// Per component at the depth, the files it holds.
    components: Vec<ComponentFiles>,
    /// Files no component holds.
    #[serde(skip_serializing_if = "is_zero")]
    no_component: usize,
    /// Of the files, those reached only through `cross_referenced` or
    /// `referenced` links, which say that work mentions the item.
    #[serde(skip_serializing_if = "is_zero")]
    only_mentions: usize,
    /// The pull requests and commits the work reached code through: for
    /// a pull request, itself; those that close or are linked to the item
    /// first.
    through: Vec<Way<'a>>,
}

#[derive(Debug, Serialize)]
struct ComponentFiles {
    id: ComponentId,
    name: String,
    files: usize,
}

/// A pull request or a commit, with the commits of it that the history
/// read holds and their files.
#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
enum Way<'a> {
    PullRequest {
        number: u64,
        state: ItemState,
        #[serde(skip_serializing_if = "Option::is_none")]
        title: Option<&'a str>,
        /// The link types that reach it from the item asked about; none for
        /// that item itself.
        #[serde(skip_serializing_if = "Vec::is_empty")]
        links: Vec<RelationType>,
        /// The commits the tracker counts for it.
        commits: usize,
        /// The commits of its commit list that the history read holds.
        by_commit_list: Vec<CommitFiles>,
        /// Its merge commit, when the history read holds it and it is no
        /// merge.
        #[serde(skip_serializing_if = "Option::is_none")]
        by_merge_commit: Option<CommitFiles>,
        /// Its merge commit, when the history read holds it as a merge,
        /// whose own changes are not read.
        #[serde(skip_serializing_if = "Option::is_none")]
        merged_as: Option<&'a str>,
    },
    Commit {
        links: Vec<RelationType>,
        #[serde(flatten)]
        commit: LinkedCommit<'a>,
    },
}

/// A commit an item links: with its files when the history read holds it,
/// else by its SHA alone.
#[derive(Debug, Serialize)]
#[serde(untagged)]
enum LinkedCommit<'a> {
    Read(CommitFiles),
    NotRead { id: &'a str },
}

/// A commit of the history read and the files it changed.
#[derive(Debug, Serialize)]
struct CommitFiles {
    id: String,
    time: i64,
    files: Vec<CommitFile>,
    /// Files past the ones the history read keeps.
    #[serde(skip_serializing_if = "is_zero")]
    omitted: usize,
}

/// The code item `number`'s work changed, as far as the history read
/// holds its commits; `None` when no history was read.
pub(crate) fn code<'a>(
    snapshot: &'a Snapshot,
    history: &History,
    full: &ArchitectureGraph,
    depth: usize,
    number: u64,
) -> Option<Code<'a>> {
    if !matches!(history.state, HistoryState::Read { .. }) {
        return None;
    }
    let mut pulls: BTreeMap<u64, (&Item, BTreeSet<RelationType>)> = BTreeMap::new();
    let mut commits: BTreeMap<&str, BTreeSet<RelationType>> = BTreeMap::new();
    match snapshot.item(number) {
        Some(item) if item.kind == ItemKind::PullRequest => {
            pulls.insert(number, (item, BTreeSet::new()));
        }
        _ => {
            // an issue's pull requests and commits, by the links that tie
            // work to it: closing, linking and cross-references
            let this = Ref::Item { item: number };
            for relation in &snapshot.relations {
                let (other, issue_is_from) = match (&relation.from, &relation.to) {
                    (from, to) if *from == this => (to, true),
                    (from, to) if *to == this => (from, false),
                    _ => continue,
                };
                match (relation.kind, issue_is_from, other) {
                    (RelationType::Closes | RelationType::Linked, false, Ref::Item { item })
                    | (RelationType::ClosedBy, true, Ref::Item { item })
                    | (RelationType::CrossReferenced, _, Ref::Item { item }) => {
                        if let Some(pull) = snapshot
                            .item(*item)
                            .filter(|i| i.kind == ItemKind::PullRequest)
                        {
                            pulls
                                .entry(*item)
                                .or_insert_with(|| (pull, BTreeSet::new()))
                                .1
                                .insert(relation.kind);
                        }
                    }
                    (
                        RelationType::ClosedBy | RelationType::Referenced,
                        _,
                        Ref::Commit {
                            commit,
                            repository: None,
                        },
                    ) => {
                        commits.entry(commit).or_default().insert(relation.kind);
                    }
                    _ => {}
                }
            }
        }
    }

    // every commit named, against the history read
    let read: BTreeMap<&str, &archmap_core::history::Commit> =
        history.commits.iter().map(|c| (c.id.as_str(), c)).collect();
    let named: BTreeSet<&str> = pulls
        .values()
        .flat_map(|(item, _)| {
            item.commits
                .iter()
                .map(String::as_str)
                .chain(item.merge_commit.as_deref())
        })
        .chain(commits.keys().copied())
        .filter(|sha| read.contains_key(sha))
        .collect();
    let files_of = commit_files(history, &named);
    let take = |sha: &str| -> Option<CommitFiles> {
        let commit = read.get(sha)?;
        Some(CommitFiles {
            id: commit.id.clone(),
            time: commit.time,
            files: files_of.get(sha).cloned().unwrap_or_default(),
            omitted: commit.omitted,
        })
    };

    let mut ways: Vec<Way> = Vec::new();
    for (number, (item, links)) in pulls {
        let mut by_commit_list: Vec<CommitFiles> = Vec::new();
        for sha in &item.commits {
            if let Some(found) = take(sha) {
                by_commit_list.push(found);
            }
        }
        by_commit_list.sort_by_key(|c| (Reverse(c.time), c.id.clone()));
        let merge = item.merge_commit.as_deref();
        let true_merge = merge.filter(|m| read.get(m).is_some_and(|c| c.is_merge()));
        let by_merge_commit = match true_merge {
            Some(_) => None,
            None => merge.and_then(take),
        };
        ways.push(Way::PullRequest {
            number,
            state: item.state,
            title: item.title.as_deref(),
            links: links.into_iter().collect(),
            commits: item.commit_count.unwrap_or(item.commits.len()),
            by_commit_list,
            by_merge_commit,
            merged_as: true_merge,
        });
    }
    for (id, links) in commits {
        ways.push(Way::Commit {
            links: links.into_iter().collect(),
            commit: match take(id) {
                Some(found) => LinkedCommit::Read(found),
                None => LinkedCommit::NotRead { id },
            },
        });
    }
    // the ways that close or are linked to the item, then those that
    // mention it; pull requests, then commits, the newest first, what
    // matched nothing last
    ways.sort_by_key(|w| match w {
        Way::PullRequest { number, .. } => {
            (w.mentions(), 0, Reverse(newest(w)), *number, String::new())
        }
        Way::Commit { commit, .. } => (
            w.mentions(),
            1,
            Reverse(newest(w)),
            0,
            commit.id().to_owned(),
        ),
    });

    // each file once, by its path now, and the components that hold them
    let paths_of = |mentions: bool| -> BTreeSet<&str> {
        ways.iter()
            .filter(|w| mentions || !w.mentions())
            .flat_map(|w| w.commits())
            .flat_map(|c| c.files.iter().map(now))
            .collect()
    };
    let paths = paths_of(true);
    let only_mentions = paths.len() - paths_of(false).len();
    let mut by_component: BTreeMap<ComponentId, usize> = BTreeMap::new();
    let mut no_component = 0;
    for (_, owner) in full.components_for_paths(paths.iter().copied()) {
        match owner {
            Some(c) => {
                *by_component
                    .entry(full.ancestor_at(&c.id, depth))
                    .or_default() += 1
            }
            None => no_component += 1,
        }
    }
    let mut components: Vec<ComponentFiles> = by_component
        .into_iter()
        .map(|(id, files)| ComponentFiles {
            name: crate::query_text::display(full, &id).to_owned(),
            id,
            files,
        })
        .collect();
    components.sort_by(|a, b| b.files.cmp(&a.files).then(a.name.cmp(&b.name)));
    Some(Code {
        files: paths.len(),
        components,
        no_component,
        only_mentions,
        through: ways,
    })
}

impl Way<'_> {
    /// Only `cross_referenced` or `referenced` links reach it: work that
    /// mentions the item, the loosest links the tracker records.
    fn mentions(&self) -> bool {
        let links = match self {
            Way::PullRequest { links, .. } | Way::Commit { links, .. } => links,
        };
        !links.is_empty()
            && links
                .iter()
                .all(|l| matches!(l, RelationType::CrossReferenced | RelationType::Referenced))
    }

    /// Its commits that the history read holds.
    fn commits(&self) -> Vec<&CommitFiles> {
        match self {
            Way::PullRequest {
                by_commit_list,
                by_merge_commit,
                ..
            } => by_commit_list.iter().chain(by_merge_commit).collect(),
            Way::Commit {
                commit: LinkedCommit::Read(found),
                ..
            } => vec![found],
            Way::Commit { .. } => Vec::new(),
        }
    }
}

impl LinkedCommit<'_> {
    fn id(&self) -> &str {
        match self {
            LinkedCommit::Read(found) => &found.id,
            LinkedCommit::NotRead { id } => id,
        }
    }
}

/// The newest committer time among a way's commits that the history read
/// holds.
fn newest(way: &Way) -> Option<i64> {
    way.commits().iter().map(|c| c.time).max()
}

/// The path HEAD holds a file at, or its last path.
fn now(file: &CommitFile) -> &str {
    file.now.as_deref().unwrap_or(&file.path)
}

/// `Code` as text, before `Not traced`; whether a list was capped.
pub(crate) fn render(out: &mut String, code: &Code, verbose: bool) -> bool {
    let cap = |n: usize| if verbose { usize::MAX } else { n };
    let pulls = code
        .through
        .iter()
        .filter(|w| matches!(w, Way::PullRequest { .. }))
        .count();
    let commits = code.through.len() - pulls;
    let components = code.components.len() + usize::from(code.no_component > 0);
    let mut heading = match code.files {
        0 => "\nCode: no file in the local history".to_owned(),
        files => format!(
            "\nCode: {} in {}",
            plural_word(files, "file", "files"),
            plural_word(components, "component", "components")
        ),
    };
    // an issue names the ways it reached code
    let own = matches!(
        code.through.first(),
        Some(Way::PullRequest { links, .. }) if links.is_empty()
    );
    if !own && !code.through.is_empty() {
        let mut ways = Vec::new();
        if pulls > 0 {
            ways.push(plural_word(pulls, "pull request", "pull requests"));
        }
        if commits > 0 {
            ways.push(plural_word(commits, "commit", "commits"));
        }
        let _ = write!(heading, ", through {}", ways.join(" and "));
    }
    match code.only_mentions {
        0 => {}
        n if n == code.files => {
            heading.push_str("; all only through cross_referenced or referenced links")
        }
        n => {
            let _ = write!(
                heading,
                "; {n} of them only through cross_referenced or referenced links"
            );
        }
    }
    let _ = writeln!(out, "{heading}");
    if code.through.is_empty() {
        let _ = writeln!(
            out,
            "  (no pull request or commit is linked to it by closes, linked, closed_by, \
             cross_referenced or referenced)"
        );
        return false;
    }
    let mut truncated = false;
    for way in code.through.iter().take(cap(MAX_WAYS)) {
        match way {
            Way::PullRequest {
                number,
                links,
                commits,
                by_commit_list,
                by_merge_commit,
                merged_as,
                ..
            } if links.is_empty() => {
                // the pull request asked about: its commits by kind, apart
                if !by_commit_list.is_empty() {
                    let list: Vec<&CommitFiles> = by_commit_list.iter().collect();
                    truncated |= files_line(out, "  by commit list: ", &list, cap(MAX_FILES));
                }
                if let Some(merge) = by_merge_commit {
                    truncated |= files_line(out, "  by merge commit: ", &[merge], cap(MAX_FILES));
                }
                if let Some(merge) = merged_as {
                    let _ = writeln!(
                        out,
                        "  merged as {}, a merge commit, whose own changes are not read",
                        short(merge)
                    );
                }
                if by_commit_list.is_empty() && by_merge_commit.is_none() && merged_as.is_none() {
                    let _ = writeln!(out, "  no commit of #{number} is in the local history");
                }
            }
            Way::PullRequest {
                number,
                links,
                commits,
                by_commit_list,
                by_merge_commit,
                merged_as,
                ..
            } => {
                let links: Vec<&str> = links.iter().map(|l| l.as_str()).collect();
                let mut line = format!(
                    "  through #{number} pull request ({}), {} of its {} in the local history",
                    links.join(", "),
                    by_commit_list.len(),
                    plural_word(*commits, "commit", "commits")
                );
                if by_merge_commit.is_some() {
                    line.push_str(", and its merge commit");
                }
                if let Some(merge) = merged_as {
                    let _ = write!(
                        line,
                        ", merged as {}, a merge commit whose own changes are not read",
                        short(merge)
                    );
                }
                let list: Vec<&CommitFiles> =
                    by_commit_list.iter().chain(by_merge_commit).collect();
                if list.is_empty() {
                    let _ = writeln!(out, "{line}");
                } else {
                    let _ = writeln!(out, "{line}:");
                    truncated |= files_line(out, "    ", &list, cap(MAX_FILES));
                }
            }
            Way::Commit { links, commit } => {
                let links: Vec<&str> = links.iter().map(|l| l.as_str()).collect();
                match commit {
                    LinkedCommit::Read(found) => {
                        let _ = writeln!(
                            out,
                            "  through commit {} {} ({}):",
                            short(&found.id),
                            date(found.time),
                            links.join(", ")
                        );
                        truncated |= files_line(out, "    ", &[found], cap(MAX_FILES));
                    }
                    LinkedCommit::NotRead { id } => {
                        let _ = writeln!(
                            out,
                            "  through commit {} ({}), not in the local history",
                            short(id),
                            links.join(", ")
                        );
                    }
                }
            }
        }
    }
    if code.through.len() > cap(MAX_WAYS) {
        truncated = true;
        let _ = writeln!(out, "  +{} more", code.through.len() - cap(MAX_WAYS));
    }
    if code.files > 0 {
        let mut parts: Vec<String> = code
            .components
            .iter()
            .map(|c| format!("{} {}", c.name, plural_word(c.files, "file", "files")))
            .collect();
        if code.no_component > 0 {
            parts.push(format!(
                "{} in no component",
                plural_word(code.no_component, "file", "files")
            ));
        }
        let _ = writeln!(out, "  components: {}", parts.join(", "));
    }
    truncated
}

/// The files of `commits`, each once by its path now, sorted, on one
/// line after `lead`; a file a later commit moved shows the path written
/// and `now <path>`. Whether the list was capped.
fn files_line(out: &mut String, lead: &str, commits: &[&CommitFiles], cap: usize) -> bool {
    let mut files: BTreeMap<&str, Shown> = BTreeMap::new();
    let mut omitted = 0;
    for commit in commits {
        omitted += commit.omitted;
        for file in &commit.files {
            let shown = files.entry(now(file)).or_default();
            shown.in_head |= file.in_head;
            match &file.now {
                Some(_) => shown.written = Some(&file.path),
                None => shown.as_now = true,
            }
        }
    }
    let names: Vec<String> = files
        .iter()
        .take(cap)
        .map(|(now, shown)| {
            let mut name = match (shown.as_now, shown.written) {
                (false, Some(written)) => format!("{written} now {now}"),
                _ => (*now).to_owned(),
            };
            if !shown.in_head {
                name.push_str(" not in HEAD");
            }
            name
        })
        .collect();
    let mut line = format!("{lead}{}", names.join(", "));
    if names.is_empty() {
        line.push_str("no file under the root");
    }
    if files.len() > cap {
        let _ = write!(line, ", +{} more", files.len() - cap);
    }
    if omitted > 0 {
        let _ = write!(
            line,
            ", and {} the history read keeps no path of",
            plural_word(omitted, "file", "files")
        );
    }
    let _ = writeln!(out, "{line}");
    files.len() > cap
}

/// How a file shows: by its path now when a commit wrote that, else by
/// the path a commit wrote.
#[derive(Default)]
struct Shown<'a> {
    as_now: bool,
    written: Option<&'a str>,
    in_head: bool,
}

/// `1 of the snapshot's 2 merged pull requests matches no commit of the
/// history read`, after `lead`.
pub(crate) fn unmatched_line(lead: &str, unmatched: Unmatched) -> String {
    format!(
        "{} of {lead}{} {} no commit of the history read",
        unmatched.unmatched,
        plural_word(
            unmatched.merged,
            "merged pull request",
            "merged pull requests"
        ),
        if unmatched.unmatched == 1 {
            "matches"
        } else {
            "match"
        }
    )
}

/// Merged pull requests no commit of the history read matches.
#[derive(Debug, Clone, Copy, Serialize)]
pub(crate) struct Unmatched {
    pub(crate) merged: usize,
    pub(crate) unmatched: usize,
}

/// How many of the snapshot's merged pull requests match no commit of
/// the history read, by commit list or merge commit; `None` when no
/// history was read.
pub(crate) fn unmatched(snapshot: &Snapshot, history: &History) -> Option<Unmatched> {
    if !matches!(history.state, HistoryState::Read { .. }) {
        return None;
    }
    let read: BTreeSet<&str> = history.commits.iter().map(|c| c.id.as_str()).collect();
    let merged: Vec<&Item> = snapshot
        .items
        .iter()
        .filter(|i| i.kind == ItemKind::PullRequest && i.state == ItemState::Merged)
        .collect();
    let unmatched = merged
        .iter()
        .filter(|i| {
            !i.commits
                .iter()
                .map(String::as_str)
                .chain(i.merge_commit.as_deref())
                .any(|sha| read.contains(sha))
        })
        .count();
    Some(Unmatched {
        merged: merged.len(),
        unmatched,
    })
}

fn is_zero(n: &usize) -> bool {
    *n == 0
}
