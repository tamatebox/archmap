//! `Work` as text: the pull requests and items the snapshot links to the
//! commits that changed the target, as [`crate::work_section`] builds it,
//! capped unless `verbose`.

use std::collections::BTreeSet;
use std::fmt::Write;

use archmap_core::co_change::CommitRef;
use archmap_core::work::{ItemKind, Ref, RelationType};

use crate::views::{LinkEnd, Other, WorkSection, WorkState};
use crate::work_line::{date, kind_word, plural_word, short, state_line};

/// What the text shows; `verbose` lifts every cap.
const MAX_PULLS: usize = 5;
const MAX_FROM_COMMITS: usize = 3;
const MAX_ITEMS: usize = 3;
const MAX_COMMITS: usize = 2;
const MAX_UNLINKED: usize = 3;

/// The section as text; whether a list was capped.
pub(crate) fn render(out: &mut String, section: &WorkSection, verbose: bool) -> bool {
    let caps = |n: usize| if verbose { usize::MAX } else { n };
    let links = match &section.state {
        WorkState::NoSnapshot => {
            let _ = writeln!(out, "\nWork: none (no snapshot at {})", section.snapshot);
            return false;
        }
        WorkState::Unreadable { error } => {
            let _ = writeln!(out, "\nWork: not read ({error})");
            return false;
        }
        WorkState::NoHistory => {
            let _ = writeln!(out, "\nWork: none (no local history to match commits in)");
            return false;
        }
        WorkState::Read(links) => links,
    };
    let mut truncated = false;
    // the issues reached, through a pull request or straight from a commit
    let issues = links
        .pull_requests
        .iter()
        .flat_map(|p| p.items.iter().map(|l| &l.item))
        .chain(links.from_commits.iter().map(|f| &f.item))
        .filter(|o| o.kind == Some(ItemKind::Issue))
        .map(|o| o.other)
        .collect::<BTreeSet<_>>()
        .len();
    let _ = writeln!(
        out,
        "\nWork: {} of the {} that changed {} {} linked to {} and {}",
        links.linked,
        plural_word(links.commits, "commit", "commits"),
        section.label,
        if links.linked == 1 { "is" } else { "are" },
        plural_word(links.pull_requests.len(), "pull request", "pull requests"),
        plural_word(issues, "issue", "issues"),
    );
    if links.large > 0 {
        let _ = writeln!(
            out,
            "  {} over 30 files not followed",
            plural_word(links.large, "commit", "commits")
        );
    }
    if !links.pull_requests.is_empty() {
        let shown = links.pull_requests.len().min(caps(MAX_PULLS));
        truncated |= shown < links.pull_requests.len();
        let _ = writeln!(
            out,
            "  pull requests: {}",
            count(links.pull_requests.len(), shown)
        );
        for pull in links.pull_requests.iter().take(shown) {
            let title = pull.title.map(|t| format!(": {t}")).unwrap_or_default();
            let state = pull.item.map(state_line).unwrap_or_default();
            let _ = writeln!(out, "  #{} pull request, {state}{title}", pull.number);
            let mut ways = Vec::new();
            for (list, word) in [
                (&pull.by_commit_list, "by commit list"),
                (&pull.by_merge_commit, "by merge commit"),
            ] {
                if list.is_empty() {
                    continue;
                }
                let commits: Vec<String> = list
                    .iter()
                    .take(caps(MAX_COMMITS))
                    .map(commit_text)
                    .collect();
                truncated |= commits.len() < list.len();
                ways.push(format!("{word}: {}", more(&commits, list.len())));
            }
            if let Some(merge) = pull.merged_as {
                ways.push(format!(
                    "merged as {}, a merge commit, not among the commits counted",
                    short(merge)
                ));
            }
            let _ = writeln!(out, "    {}", ways.join("; "));
            if !pull.items.is_empty() {
                let shown: Vec<String> = pull
                    .items
                    .iter()
                    .take(caps(MAX_ITEMS))
                    .map(|l| {
                        let verbs: Vec<&str> = l.links.iter().map(|e| verb(*e)).collect();
                        // a colon parts several verbs from the item
                        match verbs.as_slice() {
                            [one] => format!("{one} {}", other_text(&l.item)),
                            _ => format!("{}: {}", verbs.join(", "), other_text(&l.item)),
                        }
                    })
                    .collect();
                truncated |= shown.len() < pull.items.len();
                // titles may hold commas
                let mut line = shown.join("; ");
                if shown.len() < pull.items.len() {
                    let _ = write!(line, "; +{} more", pull.items.len() - shown.len());
                }
                let _ = writeln!(out, "    {line}");
            }
        }
    }
    if !links.from_commits.is_empty() {
        let shown = links.from_commits.len().min(caps(MAX_FROM_COMMITS));
        truncated |= shown < links.from_commits.len();
        let _ = writeln!(
            out,
            "  linked from commits: {}",
            count(links.from_commits.len(), shown)
        );
        for from in links.from_commits.iter().take(shown) {
            let _ = writeln!(out, "  {}", other_text(&from.item));
            let by: Vec<String> = from
                .by
                .iter()
                .take(caps(MAX_COMMITS))
                .map(|l| {
                    let how = match l.kind {
                        RelationType::ClosedBy => "closed by commit",
                        _ => "referenced in commit",
                    };
                    format!("{how} {}", commit_text(&l.commit))
                })
                .collect();
            truncated |= by.len() < from.by.len();
            let _ = writeln!(out, "    {}", more(&by, from.by.len()));
        }
    }
    if !links.unlinked.is_empty() {
        let shown: Vec<String> = links
            .unlinked
            .iter()
            .take(caps(MAX_UNLINKED))
            .map(commit_text)
            .collect();
        truncated |= shown.len() < links.unlinked.len();
        let what = match links.unlinked.len() {
            1 => "1 commit is linked to no pull request or item".to_owned(),
            n => format!("{n} commits are linked to no pull request or item"),
        };
        let mut line = format!(
            "  {what} in the snapshot by SHA (after a squash or rebase merge, a pull \
             request's own commits have other SHAs): {}",
            more(&shown, links.unlinked.len())
        );
        if links.older > 0 {
            let _ = write!(
                line,
                "; {} older than the snapshot's range, whose pull requests it may not hold",
                if links.older == 1 {
                    "1 is".to_owned()
                } else {
                    format!("{} are", links.older)
                }
            );
        }
        let _ = writeln!(out, "{line}");
    }
    let _ = writeln!(
        out,
        "  work: {}; states as of the fetch",
        links.coverage.line
    );
    truncated
}

/// `5, showing 3`, or the count alone when all are shown.
fn count(total: usize, shown: usize) -> String {
    match shown < total {
        true => format!("{total}, showing {shown}"),
        false => total.to_string(),
    }
}

/// `f0e9bdf 2026-01-01`.
fn commit_text(commit: &CommitRef) -> String {
    format!("{} {}", short(&commit.id), date(commit.time))
}

/// What a pull request does with a linked item, in the snapshot's
/// direction: a cross-reference goes from the item that references to the
/// one it references.
fn verb(link: LinkEnd) -> &'static str {
    match (link.kind, link.end) {
        (RelationType::Closes, _) => "closes",
        (RelationType::ClosedBy, _) => "closed",
        (RelationType::Linked, _) => "linked to",
        (RelationType::CrossReferenced, "from") => "cross-references",
        _ => "cross-referenced by",
    }
}

/// `#12 issue, closed: Refunds round down`, `acme/web#4`, `#45 (outside
/// the snapshot's range)`.
fn other_text(other: &Other) -> String {
    match (other.other, other.found) {
        (Ref::Item { item }, Some(found)) => {
            let title = found
                .title
                .as_deref()
                .map(|t| format!(": {t}"))
                .unwrap_or_default();
            format!(
                "#{item} {}, {}{title}",
                kind_word(found.kind),
                state_line(found)
            )
        }
        (Ref::Item { item }, None) => format!("#{item} (outside the snapshot's range)"),
        (Ref::Foreign { repository, number }, _) => format!("{repository}#{number}"),
        (Ref::Commit { commit, .. }, _) => format!("commit {}", short(commit)),
    }
}

/// `a, b, +3 more`, as `query` writes a capped list.
fn more(shown: &[String], total: usize) -> String {
    let mut line = shown.join(", ");
    if total > shown.len() {
        let _ = write!(line, ", +{} more", total - shown.len());
    }
    line
}
