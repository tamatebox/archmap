//! The `Work` section of `impact`, and of `query` on a file or component:
//! the pull requests and items the work snapshot links to the commits that
//! changed the target, those `Changed in the same commits` counts. Every
//! step shown is an observed link (a pull request's commit list or merge
//! commit, matched by SHA; a link type the tracker records); nothing here is
//! a new fact, and items are never merged into one list of related work.

use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet};

use archmap_core::co_change::CommitRef;
use archmap_core::work::{Item, ItemKind, Ref, Relation, RelationType, Snapshot};
use archmap_core::ArchitectureGraph;

use crate::co_change::{self, Changed};
use crate::views::{
    CoChangeSection, CommitLink, FromCommits, LinkEnd, LinkedItem, LinkedPull, Other, WorkCoverage,
    WorkLinks, WorkSection, WorkState,
};
use crate::work_line::range_line;
use crate::Workspace;

/// The section for the target that `co`, its co-change view, reads.
pub(crate) fn section<'a>(ws: &'a Workspace, co: &CoChangeSection) -> WorkSection<'a> {
    let state = match ws.snapshot() {
        Ok(None) => WorkState::NoSnapshot,
        Err(error) => WorkState::Unreadable {
            error: error.clone(),
        },
        Ok(Some(snapshot)) => match &co.view {
            None => WorkState::NoHistory,
            Some(view) => {
                // the merges of the history read, which the view never counts
                let merges: BTreeSet<&str> = ws
                    .history()
                    .commits
                    .iter()
                    .filter(|c| c.is_merge())
                    .map(|c| c.id.as_str())
                    .collect();
                let commits = &view.target_commits;
                WorkState::Read(links(snapshot, commits, view.target_large, &merges))
            }
        },
    };
    WorkSection {
        snapshot: ws.snapshot_path(),
        state,
        label: co.label.clone(),
    }
}

/// The section for `query`, which reads the history only when a snapshot
/// exists.
pub(crate) fn for_query<'a>(
    ws: &'a Workspace,
    full: &ArchitectureGraph,
    changed: Changed,
) -> WorkSection<'a> {
    match ws.snapshot() {
        Ok(Some(_)) => section(ws, &co_change::section(ws.history(), full, changed)),
        Ok(None) => WorkSection {
            snapshot: ws.snapshot_path(),
            state: WorkState::NoSnapshot,
            label: String::new(),
        },
        Err(error) => WorkSection {
            snapshot: ws.snapshot_path(),
            state: WorkState::Unreadable {
                error: error.clone(),
            },
            label: String::new(),
        },
    }
}

/// The pull requests and items that link `commits`, the counted commits
/// that changed the target, newest first.
fn links<'a>(
    snapshot: &'a Snapshot,
    commits: &[CommitRef],
    large: usize,
    merges: &BTreeSet<&str>,
) -> WorkLinks<'a> {
    // the pull requests whose commit list or merge commit holds a SHA
    let mut pulls_of: BTreeMap<&str, Vec<(&Item, bool)>> = BTreeMap::new();
    for item in snapshot
        .items
        .iter()
        .filter(|i| i.kind == ItemKind::PullRequest)
    {
        for sha in &item.commits {
            pulls_of.entry(sha).or_default().push((item, false));
        }
        if let Some(sha) = &item.merge_commit {
            pulls_of.entry(sha).or_default().push((item, true));
        }
    }
    // the links of each item, with whether it is the `from` end, and the
    // items each commit links
    let mut item_links: BTreeMap<u64, Vec<(&Relation, bool)>> = BTreeMap::new();
    let mut commit_links: BTreeMap<&str, Vec<(&Relation, &Ref)>> = BTreeMap::new();
    for relation in &snapshot.relations {
        match (&relation.from, &relation.to) {
            (
                Ref::Commit {
                    commit,
                    repository: None,
                },
                other,
            )
            | (
                other,
                Ref::Commit {
                    commit,
                    repository: None,
                },
            ) => {
                if !matches!(other, Ref::Commit { .. }) {
                    commit_links
                        .entry(commit)
                        .or_default()
                        .push((relation, other));
                }
            }
            (from, to) => {
                if let Ref::Item { item } = from {
                    item_links.entry(*item).or_default().push((relation, true));
                }
                if let Ref::Item { item } = to {
                    item_links.entry(*item).or_default().push((relation, false));
                }
            }
        }
    }

    let mut pulls: BTreeMap<u64, LinkedPull> = BTreeMap::new();
    let mut from_commits: BTreeMap<&Ref, Vec<CommitLink>> = BTreeMap::new();
    let mut unlinked = Vec::new();
    for commit in commits {
        let mut linked = false;
        for &(item, by_merge) in pulls_of.get(commit.id.as_str()).into_iter().flatten() {
            linked = true;
            let pull = pulls
                .entry(item.number)
                .or_insert_with(|| pull_of(snapshot, item, &item_links));
            match by_merge {
                true => pull.by_merge_commit.push(commit.clone()),
                false => pull.by_commit_list.push(commit.clone()),
            }
        }
        for &(relation, other) in commit_links.get(commit.id.as_str()).into_iter().flatten() {
            linked = true;
            from_commits.entry(other).or_default().push(CommitLink {
                kind: relation.kind,
                commit: commit.clone(),
            });
        }
        if !linked {
            unlinked.push(commit.clone());
        }
    }
    // a true merge's merge commit is no commit counted: the pull request's
    // own fact, apart from the commits that connect it
    for pull in pulls.values_mut() {
        let merge = pull.item.and_then(|i| i.merge_commit.as_deref());
        pull.merged_as = merge.filter(|m| merges.contains(m));
    }
    let mut pulls: Vec<LinkedPull> = pulls.into_values().collect();
    pulls.sort_by_key(|p| {
        (
            Reverse(newest(&p.by_commit_list, &p.by_merge_commit)),
            p.number,
        )
    });
    let mut from_commits: Vec<FromCommits> = from_commits
        .into_iter()
        .map(|(other, by)| FromCommits {
            item: brief(snapshot, other),
            by,
        })
        .collect();
    from_commits.sort_by_key(|f| {
        let time = f.by.iter().map(|l| l.commit.time).max();
        (Reverse(time), number(f.item.other))
    });
    // the range starts where the bound stopped the fetch, when it did
    let range = &snapshot.range;
    let since = match range.reached_bound {
        true => range.effective_since.as_deref(),
        false => range.updated_since.as_deref(),
    };
    let older = since.map_or(0, |since| {
        unlinked
            .iter()
            .filter(|c| archmap_scan::work::utc(c.time).as_str() < since)
            .count()
    });
    WorkLinks {
        commits: commits.len(),
        linked: commits.len() - unlinked.len(),
        large,
        pull_requests: pulls,
        from_commits,
        unlinked,
        older,
        coverage: WorkCoverage {
            source: &snapshot.source,
            repository: &snapshot.repository,
            fetched_at: &snapshot.fetched_at,
            range: &snapshot.range,
            line: range_line(snapshot),
        },
    }
}

/// A pull request with the items it links, closing ones first.
fn pull_of<'a>(
    snapshot: &'a Snapshot,
    item: &'a Item,
    item_links: &BTreeMap<u64, Vec<(&'a Relation, bool)>>,
) -> LinkedPull<'a> {
    // each item once, with every link between them: the pull request's
    // side, in the snapshot's direction: it closes or links an issue,
    // closed one, references an item or is referenced by one
    let mut linked: BTreeMap<&Ref, BTreeSet<LinkEnd>> = BTreeMap::new();
    for &(relation, from) in item_links.get(&item.number).into_iter().flatten() {
        let other = match (relation.kind, from) {
            (RelationType::Closes | RelationType::Linked, true) => &relation.to,
            (RelationType::ClosedBy, false) => &relation.from,
            (RelationType::CrossReferenced, true) => &relation.to,
            (RelationType::CrossReferenced, false) => &relation.from,
            _ => continue,
        };
        linked.entry(other).or_default().insert(LinkEnd {
            kind: relation.kind,
            end: if from { "from" } else { "to" },
        });
    }
    let mut items: Vec<LinkedItem> = linked
        .into_iter()
        .map(|(other, links)| LinkedItem {
            item: brief(snapshot, other),
            links: links.into_iter().collect(),
        })
        .collect();
    // closing links first, then by number
    items.sort_by_key(|l| {
        let first = l.links.iter().map(|e| (order(e.kind), e.end)).min();
        (first, number(l.item.other))
    });
    LinkedPull {
        number: item.number,
        state: item.state,
        title: item.title.as_deref(),
        by_commit_list: Vec::new(),
        by_merge_commit: Vec::new(),
        merged_as: None,
        items,
        item: Some(item),
    }
}

/// The other end of a link, with what the snapshot holds of it.
fn brief<'a>(snapshot: &'a Snapshot, other: &'a Ref) -> Other<'a> {
    let found = match other {
        Ref::Item { item } => snapshot.item(*item),
        _ => None,
    };
    Other {
        other,
        kind: found.map(|i| i.kind),
        state: found.map(|i| i.state),
        title: found.and_then(|i| i.title.as_deref()),
        found,
    }
}

/// Link types in the order a pull request lists them: what it closes,
/// then what it was linked to, then cross-references.
fn order(kind: RelationType) -> u8 {
    match kind {
        RelationType::Closes => 0,
        RelationType::ClosedBy => 1,
        RelationType::Linked => 2,
        _ => 3,
    }
}

fn number(other: &Ref) -> (u8, u64) {
    match other {
        Ref::Item { item } => (0, *item),
        Ref::Foreign { number, .. } => (1, *number),
        Ref::Commit { .. } => (2, 0),
    }
}

/// The newest committer time among a pull request's connecting commits.
fn newest(list: &[CommitRef], merge: &[CommitRef]) -> i64 {
    list.iter().chain(merge).map(|c| c.time).max().unwrap_or(0)
}
