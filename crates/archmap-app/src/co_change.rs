//! `impact`'s files changed in the same commits as the target: a view of the
//! committed history, computed when `impact` asks. Changing together is a
//! fact of the history, never proof of a dependency: a feature, a format
//! run or a rename can tie files.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write;

use archmap_core::co_change::{co_change, CoChange, CoChanged, CommitRef, NoCommit, Settings};
use archmap_core::history::{History, HistoryState, Renames};
use archmap_core::{ArchitectureGraph, Component, ComponentId};

use crate::not_traced::HistoryGaps;
use crate::query_text::{plural, with_more};
use crate::views::{CoChangeSection, HistoryCoverage};

/// Files and commits per file the text shows; `verbose` lifts both.
const MAX_FILES: usize = 5;
const MAX_COMMITS: usize = 2;

/// What changes, as the view reads it.
pub(crate) enum Changed<'a> {
    /// A file, or the file that defines a symbol.
    File { path: &'a str, symbol: bool },
    /// A component: HEAD's files it or a component inside it owns.
    Component(&'a Component),
}

/// The section for `changed`.
pub(crate) fn section<'a>(
    history: &'a History,
    full: &ArchitectureGraph,
    changed: Changed,
) -> CoChangeSection<'a> {
    let (targets, label) = match changed {
        Changed::File { path, symbol } => {
            let label = match symbol {
                true => format!("its file {path}"),
                false => path.to_owned(),
            };
            (BTreeSet::from([path.to_owned()]), label)
        }
        Changed::Component(component) => {
            let files = component_files(history, full, component);
            let name = crate::query_text::display(full, &component.id);
            let label = format!("{name}'s {}", plural(files.len(), "file"));
            (files, label)
        }
    };
    let view = matches!(history.state, HistoryState::Read { .. })
        .then(|| co_change(history, &targets, &Settings::default()));
    CoChangeSection {
        history: HistoryCoverage {
            state: &history.state,
            prefix: &history.prefix,
            git_version: history.git_version.as_deref(),
            bound: history.bound,
            reached_bound: history.reached_bound,
            renames: &history.renames,
            skipped_paths: history.skipped_paths,
        },
        target_paths: targets.into_iter().collect(),
        view,
        label,
    }
}

/// HEAD's paths under `component`'s path that it or a component inside it
/// owns: its code, and the configuration and data beside it.
fn component_files(
    history: &History,
    full: &ArchitectureGraph,
    component: &Component,
) -> BTreeSet<String> {
    let Some(path) = component.path.as_deref() else {
        return BTreeSet::new();
    };
    let dir = path.trim_start_matches("./").trim_end_matches('/');
    let whole = dir.is_empty() || dir == ".";
    let under = history.head_files.keys().map(String::as_str).filter(|p| {
        whole
            || p.strip_prefix(dir)
                .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
    });
    let mut inside: BTreeMap<&ComponentId, bool> = BTreeMap::new();
    full.components_for_paths(under)
        .into_iter()
        .filter(|(_, owner)| {
            owner.is_some_and(|o| {
                *inside
                    .entry(&o.id)
                    .or_insert_with(|| full.containment_path(&o.id).contains(&component.id))
            })
        })
        .map(|(p, _)| p.to_owned())
        .collect()
}

/// What in the history read may hide files changed with the target, for
/// `Not traced`.
pub(crate) fn gaps(history: &History) -> Option<HistoryGaps> {
    let HistoryState::Read {
        shallow, partial, ..
    } = &history.state
    else {
        return None;
    };
    let mut gaps = Vec::new();
    if *shallow {
        gaps.push("the clone is shallow, so the history ends at its depth");
    }
    if history.reached_bound {
        gaps.push("older commits were not read");
    }
    match history.renames {
        Renames::NotDetected if *partial => {
            gaps.push("renames are not detected in a partial clone")
        }
        Renames::Detected {
            inexact_skipped: true,
            ..
        } => gaps.push("renames among too many files were found only when exact"),
        _ => {}
    }
    (!gaps.is_empty()).then_some(HistoryGaps { gaps })
}

/// The section as text; whether a list was capped.
pub(crate) fn render(out: &mut String, section: &CoChangeSection, verbose: bool) -> bool {
    let (max_files, max_commits) = match verbose {
        true => (usize::MAX, usize::MAX),
        false => (MAX_FILES, MAX_COMMITS),
    };
    let heading = "Changed in the same commits";
    let Some(view) = &section.view else {
        let why = unread(section.history.state);
        let _ = writeln!(out, "\n{heading}: not read ({why})");
        return false;
    };
    let mut truncated = false;
    // a file HEAD no longer holds is in JSON only
    let files: Vec<&CoChanged> = view.files.iter().filter(|f| f.in_head).collect();
    // a merge's own changes (conflict resolutions, edits while merging) are
    // not read: say which commits the counts are of
    let kind = match view.counts.merges {
        0 => "commit",
        _ => "non-merge commit",
    };
    let commits = plural(view.target_commits.len(), kind);
    match (&view.none, files.is_empty()) {
        (Some(none), _) => {
            let why = no_commit(none, &section.label, view.settings.max_files, kind);
            let _ = writeln!(out, "\n{heading}: none: {why}");
        }
        (None, true) => {
            let nothing = match view.files.is_empty() {
                true => "they changed nothing else",
                false => "they changed nothing else HEAD holds",
            };
            let _ = writeln!(
                out,
                "\n{heading}: none, in the {commits} that changed {}: {nothing}",
                section.label
            );
        }
        (None, false) => {
            let shown = files.len().min(max_files);
            truncated |= shown < files.len();
            let mut count = plural(files.len(), "file");
            if shown < files.len() {
                let _ = write!(count, ", showing {shown}");
            }
            // the counts per file read as commits it shares, out of the
            // target's and out of its own
            let _ = writeln!(
                out,
                "\n{heading} (history, not imports): {count}, in the {commits} that changed {}; \
                 per file: commits shared, of the target's and of its own",
                section.label
            );
            for file in files.iter().take(shown) {
                let listed: Vec<String> = file
                    .commits
                    .iter()
                    .take(max_commits)
                    .map(|c| commit(c, file))
                    .collect();
                truncated |= listed.len() < file.commits.len();
                let mark = match file.submodule {
                    true => " (submodule)",
                    false => "",
                };
                let shared = file.commits.len();
                let _ = writeln!(
                    out,
                    "  {}{mark}  {shared} of the target's {}, {shared} of its own {}: {}",
                    file.path,
                    view.target_commits.len(),
                    file.own,
                    with_more(&listed, shared)
                );
            }
        }
    }
    let _ = writeln!(out, "  history: {}", history_line(section, view));
    truncated
}

/// `3e1f0a2 2026-01-14`, with the path the file had then when it was
/// another.
fn commit(c: &CommitRef, file: &CoChanged) -> String {
    let mut line = format!("{} {}", short(&c.id), date(c.time));
    if let Some(path) = file.earlier.get(&c.id) {
        let _ = write!(line, " (then {path})");
    }
    line
}

pub(crate) fn short(id: &str) -> &str {
    id.get(..7).unwrap_or(id)
}

/// Why the history was not read.
fn unread(state: &HistoryState) -> String {
    match state {
        HistoryState::NotGit => "not a git repository".to_owned(),
        HistoryState::GitMissing => "git not found".to_owned(),
        HistoryState::DubiousOwnership => "dubious ownership: see git's safe.directory".to_owned(),
        HistoryState::NoCommits => "no commits".to_owned(),
        HistoryState::Unreadable { reason } => format!("unreadable: {reason}"),
        HistoryState::Read { .. } => "read".to_owned(),
    }
}

/// Why no counted commit changed the target; `kind` names the commits
/// read for changes.
fn no_commit(none: &NoCommit, label: &str, max_files: usize, kind: &str) -> String {
    match none {
        NoCommit::NotInHead => format!(
            "HEAD does not hold {label}, so no commit changed it yet (uncommitted changes are \
             not read)"
        ),
        NoCommit::NoneRead { older: true } => {
            format!("no {kind} read changed {label}, and older commits were not read")
        }
        NoCommit::NoneRead { older: false } => format!("no {kind} changed {label}"),
        NoCommit::OnlyLeftOut { large } => format!(
            "{label} changed only in commits left out ({} over {max_files} files)",
            plural(*large, "commit")
        ),
    }
}

/// `HEAD 8c4e2d0, full clone; 9 commits read, 7 counted; left out 1 over 30
/// files, 1 merge; renames -M50%`.
fn history_line(section: &CoChangeSection, view: &CoChange) -> String {
    let history = &section.history;
    let mut line = String::new();
    if let HistoryState::Read {
        head,
        shallow,
        partial,
    } = history.state
    {
        let clone = match (shallow, partial) {
            (true, true) => "shallow partial clone",
            (true, false) => "shallow clone",
            (false, true) => "partial clone",
            (false, false) => "full clone",
        };
        let _ = write!(line, "HEAD {}, {clone}", short(head));
    }
    if !history.prefix.is_empty() {
        let _ = write!(line, ", root {} of the repository", history.prefix);
    }
    let read = plural(view.counts.read, "commit");
    let _ = write!(line, "; {read} read");
    if history.reached_bound {
        let _ = write!(line, " (the first {} git lists)", history.bound);
    }
    let _ = write!(line, ", {} counted", view.counts.counted);
    let mut left_out = Vec::new();
    if view.counts.large > 0 {
        left_out.push(format!(
            "{} over {} files",
            view.counts.large, view.settings.max_files
        ));
    }
    if view.counts.merges > 0 {
        left_out.push(plural(view.counts.merges, "merge"));
    }
    if view.counts.boundaries > 0 {
        left_out.push(format!(
            "{} at the shallow boundary",
            view.counts.boundaries
        ));
    }
    if !left_out.is_empty() {
        let _ = write!(line, "; left out {}", left_out.join(", "));
    }
    match history.renames {
        Renames::Detected {
            similarity,
            inexact_skipped,
            ..
        } => {
            let _ = write!(line, "; renames -M{similarity}%");
            if *inexact_skipped {
                line.push_str(", inexact ones skipped");
            }
        }
        Renames::NotDetected => line.push_str("; renames not detected"),
    }
    if history.skipped_paths > 0 {
        let _ = write!(
            line,
            "; {} not UTF-8 skipped",
            plural(history.skipped_paths, "path")
        );
    }
    if view.files.len() > 1 {
        line.push_str("; files by shared commits over the mean of both counts");
    }
    line
}

/// The UTC date of a time in seconds since the epoch, as `2026-01-14`.
pub(crate) fn date(time: i64) -> String {
    // days to a civil date, after Howard Hinnant's `civil_from_days`
    let z = time.div_euclid(86_400) + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates_are_utc_calendar_days() {
        assert_eq!(date(0), "1970-01-01");
        assert_eq!(date(951_782_400), "2000-02-29");
        assert_eq!(date(1_768_348_800), "2026-01-14");
        assert_eq!(date(1_768_435_199), "2026-01-14");
        assert_eq!(date(-86_400), "1969-12-31");
    }
}
