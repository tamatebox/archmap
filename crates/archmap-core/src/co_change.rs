//! Files changed in the same commits as a target: a view computed from the
//! history's facts when a command asks, with settings of its own. Nothing
//! here is stored; changing a setting computes the view again.
//!
//! Counted are the commits that are no merge, no shallow boundary and that
//! change at most [`Settings::max_files`] files under the root. A file
//! counts once per counted commit that also changes a target file, however
//! many target files that commit changes. Renames git detected are
//! followed from every commit read, counted or not, along the read
//! commits in topological order (children first, ties in git's print
//! order): an older change to a path counts for the file it was renamed
//! to, and an add of a path ends what that path named before. Where the
//! ancestry between two commits runs through commits that were not read,
//! git's print order stands.

use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;

use crate::history::{ChangeKind, Commit, HeadEntry, History};

/// How the view is computed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Settings {
    /// Commits that change more files than this under the root are left
    /// out: a format run, a vendoring, a mass rename.
    pub max_files: usize,
    /// Count an older path's changes for the file it was renamed to.
    pub follow_renames: bool,
    /// Count a move without edits (similarity 100) as a change.
    pub pure_moves_count: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            max_files: 30,
            follow_renames: true,
            pure_moves_count: false,
        }
    }
}

/// A commit, by its SHA and committer time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CommitRef {
    pub id: String,
    pub time: i64,
}

/// A file changed in the same commits as the target.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CoChanged {
    /// Its path at HEAD, or its last path when HEAD no longer holds it.
    pub path: String,
    /// The counted commits that changed it and the target, newest first.
    pub commits: Vec<CommitRef>,
    /// The counted commits that changed it at all.
    pub own: usize,
    /// HEAD holds it.
    pub in_head: bool,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub submodule: bool,
    /// Earlier paths it had in the shared commits, by commit.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub earlier: BTreeMap<String, String>,
}

/// Why no counted commit changed the target.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", tag = "why")]
pub enum NoCommit {
    /// HEAD holds none of the target's files: not committed yet.
    NotInHead,
    /// It changed only in commits left out for their size.
    OnlyLeftOut { large: usize },
    /// No commit read changed it; `older` when the read stopped at its
    /// bound or at a shallow clone's boundary, whose changes are not read,
    /// so older commits may have.
    NoneRead { older: bool },
}

/// The commits read and how many were counted or left out.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct Counts {
    pub read: usize,
    pub counted: usize,
    pub large: usize,
    pub merges: usize,
    pub boundaries: usize,
}

/// The view for one target.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CoChange {
    pub settings: Settings,
    pub counts: Counts,
    /// The counted commits that changed a target file, newest first.
    pub target_commits: Vec<CommitRef>,
    /// The target's files those commits changed.
    pub target_files: usize,
    /// The commits that changed a target file and were left out for their
    /// size.
    #[serde(skip_serializing_if = "is_zero")]
    pub target_large: usize,
    /// Other files changed in those commits, by their shared commits over
    /// the mean of the target's count and their own (code-maat's degree),
    /// highest first, so a file that changes in most commits sinks; then
    /// most shared commits, then path.
    pub files: Vec<CoChanged>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub none: Option<NoCommit>,
}

/// Files changed in the same commits as the files `targets` names (paths
/// relative to the root, as HEAD holds them).
pub fn co_change(history: &History, targets: &BTreeSet<String>, settings: &Settings) -> CoChange {
    let order = topological(&history.commits);
    let mut identity = Identity::default();
    let mut counts = Counts {
        read: history.commits.len(),
        ..Counts::default()
    };
    // per counted commit (newest first): the identities it changed
    let mut counted: Vec<(&Commit, BTreeMap<String, String>)> = Vec::new();
    let mut large_touching = 0;
    for &i in &order {
        let commit = &history.commits[i];
        // the identities of the paths as this commit leaves them
        let mut changed: BTreeMap<String, String> = BTreeMap::new();
        for change in &commit.changes {
            let pure_move = matches!(
                change.kind,
                ChangeKind::Renamed {
                    similarity: 100,
                    ..
                }
            );
            if pure_move && !settings.pure_moves_count {
                continue;
            }
            let key = identity.of(&change.path, settings.follow_renames);
            changed.insert(key, change.path.clone());
        }
        identity.after(commit, settings.follow_renames);
        let size = commit.changes.len() + commit.omitted;
        if commit.is_merge() {
            counts.merges += 1;
        } else if commit.boundary {
            counts.boundaries += 1;
        } else if size > settings.max_files {
            counts.large += 1;
            if changed.keys().any(|k| targets.contains(k)) {
                large_touching += 1;
            }
        } else {
            counts.counted += 1;
            counted.push((commit, changed));
        }
    }

    let mut own: BTreeMap<&str, usize> = BTreeMap::new();
    for (_, changed) in &counted {
        for key in changed.keys() {
            *own.entry(key).or_default() += 1;
        }
    }
    let mut target_commits = Vec::new();
    let mut target_files: BTreeSet<&str> = BTreeSet::new();
    let mut shared: BTreeMap<&str, Shared> = BTreeMap::new();
    for (commit, changed) in &counted {
        let hits: Vec<&str> = changed
            .keys()
            .map(String::as_str)
            .filter(|k| targets.contains(*k))
            .collect();
        if hits.is_empty() {
            continue;
        }
        target_files.extend(hits);
        let reference = CommitRef {
            id: commit.id.clone(),
            time: commit.time,
        };
        target_commits.push(reference.clone());
        for (key, path) in changed {
            if targets.contains(key) {
                continue;
            }
            let entry = shared.entry(key.as_str()).or_insert_with(|| Shared {
                commits: Vec::new(),
                earlier: BTreeMap::new(),
            });
            entry.commits.push(reference.clone());
            if path != key {
                entry.earlier.insert(commit.id.clone(), path.clone());
            }
        }
    }
    let mut files: Vec<CoChanged> = shared
        .into_iter()
        .map(|(key, found)| {
            let entry = history.head_files.get(key);
            CoChanged {
                in_head: entry.is_some(),
                submodule: entry == Some(&HeadEntry::Submodule),
                own: own.get(key).copied().unwrap_or(0),
                path: identity.display(key),
                commits: found.commits,
                earlier: found.earlier,
            }
        })
        .collect();
    // code-maat's degree, shared commits over the mean of the two counts,
    // compared without division: hubs that change in most commits sink
    let total = target_commits.len();
    files.sort_by(|a, b| {
        let (sa, sb) = (a.commits.len(), b.commits.len());
        (sb * (total + a.own))
            .cmp(&(sa * (total + b.own)))
            .then(sb.cmp(&sa))
            .then(a.path.cmp(&b.path))
    });

    let none = target_commits.is_empty().then(|| {
        if !targets.iter().any(|t| history.head_files.contains_key(t)) {
            NoCommit::NotInHead
        } else if large_touching > 0 {
            NoCommit::OnlyLeftOut {
                large: large_touching,
            }
        } else {
            NoCommit::NoneRead {
                older: history.reached_bound || counts.boundaries > 0,
            }
        }
    });
    CoChange {
        settings: *settings,
        counts,
        target_files: target_files.len(),
        target_large: large_touching,
        target_commits,
        files,
        none,
    }
}

/// A file a commit changed: the path it wrote, and where HEAD holds that
/// file now.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CommitFile {
    /// The path the commit wrote.
    pub path: String,
    /// Its path at HEAD, or its last path when HEAD no longer holds it,
    /// when later commits moved it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub now: Option<String>,
    /// HEAD holds it, at `now` when that is set.
    pub in_head: bool,
}

/// The files each of `commits` changed under the root, every change kept
/// (moves without edits and large commits too), with the renames git
/// detected followed to HEAD along the commits read, as [`co_change`]
/// follows them. A commit the history read does not hold is absent.
pub fn commit_files(
    history: &History,
    commits: &BTreeSet<&str>,
) -> BTreeMap<String, Vec<CommitFile>> {
    let mut identity = Identity::default();
    let mut files = BTreeMap::new();
    for i in topological(&history.commits) {
        let commit = &history.commits[i];
        if commits.contains(commit.id.as_str()) {
            let changed = commit
                .changes
                .iter()
                .map(|change| {
                    let key = identity.of(&change.path, true);
                    let shown = identity.display(&key);
                    CommitFile {
                        in_head: history.head_files.contains_key(&key),
                        now: (shown != change.path).then_some(shown),
                        path: change.path.clone(),
                    }
                })
                .collect();
            files.insert(commit.id.clone(), changed);
        }
        identity.after(commit, true);
    }
    files
}

/// What a file shares with the target: the shared commits newest first,
/// and its earlier paths in them.
struct Shared {
    commits: Vec<CommitRef>,
    earlier: BTreeMap<String, String>,
}

/// What a path names at a point of the walk from HEAD: its own HEAD path,
/// the path it was renamed to, or an earlier file at that path.
#[derive(Default)]
struct Identity {
    /// A path, as older commits write it -> the identity it names.
    alias: BTreeMap<String, String>,
    /// Identities of files that HEAD does not hold at their path -> the
    /// path to show.
    shown: BTreeMap<String, String>,
}

impl Identity {
    fn of(&self, path: &str, follow: bool) -> String {
        match follow {
            true => self
                .alias
                .get(path)
                .cloned()
                .unwrap_or_else(|| path.to_owned()),
            false => path.to_owned(),
        }
    }

    /// From here on (older commits), `path` names an earlier file: one that
    /// was at that path before `commit` added or moved a file there.
    fn ends(&mut self, path: &str, commit: &str) {
        let earlier = format!("{path}\u{0}{commit}");
        self.shown.insert(earlier.clone(), path.to_owned());
        self.alias.insert(path.to_owned(), earlier);
    }

    /// What older commits' paths name, after `commit`: a rename carries
    /// the old path to the file it became when `follow`, and an add or a
    /// rename ends what its path named before.
    fn after(&mut self, commit: &Commit, follow: bool) {
        for change in &commit.changes {
            match &change.kind {
                ChangeKind::Renamed { from, .. } if follow => {
                    let key = self.of(&change.path, true);
                    self.ends(&change.path, &commit.id);
                    self.alias.insert(from.clone(), key);
                }
                ChangeKind::Added => self.ends(&change.path, &commit.id),
                _ => {}
            }
        }
    }

    fn display(&self, key: &str) -> String {
        self.shown
            .get(key)
            .cloned()
            .unwrap_or_else(|| key.to_owned())
    }
}

/// The commits' indices, children before parents (Kahn's algorithm over
/// the parent links among them), ties in their print order.
fn topological(commits: &[Commit]) -> Vec<usize> {
    let index: BTreeMap<&str, usize> = commits
        .iter()
        .enumerate()
        .map(|(i, c)| (c.id.as_str(), i))
        .collect();
    let mut children = vec![0usize; commits.len()];
    for commit in commits {
        for parent in &commit.parents {
            if let Some(&p) = index.get(parent.as_str()) {
                children[p] += 1;
            }
        }
    }
    let mut ready: BTreeSet<usize> = (0..commits.len()).filter(|&i| children[i] == 0).collect();
    let mut order = Vec::with_capacity(commits.len());
    while let Some(i) = ready.pop_first() {
        order.push(i);
        for parent in &commits[i].parents {
            if let Some(&p) = index.get(parent.as_str()) {
                children[p] -= 1;
                if children[p] == 0 {
                    ready.insert(p);
                }
            }
        }
    }
    order
}

fn is_zero(n: &usize) -> bool {
    *n == 0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::history::{Change, HistoryState, Renames};

    fn commit(id: &str, parents: &[&str], changes: &[(&str, ChangeKind)]) -> Commit {
        Commit {
            id: id.into(),
            parents: parents.iter().map(|p| (*p).into()).collect(),
            time: 0,
            boundary: false,
            changes: changes
                .iter()
                .map(|(path, kind)| Change {
                    path: (*path).into(),
                    kind: kind.clone(),
                })
                .collect(),
            omitted: 0,
        }
    }

    fn history(commits: Vec<Commit>, head: &[&str]) -> History {
        let mut history = History::unread(
            HistoryState::Read {
                head: commits[0].id.clone(),
                shallow: false,
                partial: false,
            },
            100,
        );
        history.renames = Renames::Detected {
            similarity: 50,
            limit: 1000,
            inexact_skipped: false,
        };
        history.commits = commits;
        history.head_files = head
            .iter()
            .map(|p| ((*p).to_owned(), HeadEntry::File))
            .collect();
        history
    }

    fn targets(paths: &[&str]) -> BTreeSet<String> {
        paths.iter().map(|p| (*p).to_owned()).collect()
    }

    fn shown(view: &CoChange) -> Vec<(String, usize, usize)> {
        view.files
            .iter()
            .map(|f| (f.path.clone(), f.commits.len(), f.own))
            .collect()
    }

    use ChangeKind::{Added as A, Deleted as D, Modified as M};

    #[test]
    fn a_file_counts_once_per_commit_and_shows_its_own_count() {
        let h = history(
            vec![
                commit("c4", &["c3"], &[("rates.yaml", M), ("lock", M)]),
                commit(
                    "c3",
                    &["c2"],
                    &[("price.ts", M), ("rates.yaml", M), ("lock", M)],
                ),
                commit(
                    "c2",
                    &["c1"],
                    &[("price.ts", M), ("cart.ts", M), ("lock", M)],
                ),
                commit(
                    "c1",
                    &[],
                    &[("price.ts", A), ("rates.yaml", A), ("cart.ts", A)],
                ),
            ],
            &["price.ts", "rates.yaml", "cart.ts", "lock"],
        );
        // a component's two files in one commit count that commit once
        let view = co_change(&h, &targets(&["price.ts", "cart.ts"]), &Settings::default());
        assert_eq!(view.target_commits.len(), 3);
        assert_eq!(view.target_files, 2);
        // as close and as many shared commits: by path
        assert_eq!(
            shown(&view),
            [("lock".into(), 2, 3), ("rates.yaml".into(), 2, 3)]
        );
        assert_eq!(view.counts.counted, 4);
    }

    #[test]
    fn a_hub_that_changes_in_most_commits_sinks_below_a_partner() {
        let mut commits = vec![
            commit("c6", &["c5"], &[("hub.md", M)]),
            commit("c5", &["c4"], &[("hub.md", M)]),
            commit("c4", &["c3"], &[("hub.md", M)]),
            commit("c3", &["c2"], &[("t.ts", M), ("hub.md", M)]),
            commit("c2", &["c1"], &[("t.ts", M), ("p.ts", M), ("hub.md", M)]),
        ];
        commits.push(commit(
            "c1",
            &[],
            &[("t.ts", A), ("p.ts", A), ("hub.md", A)],
        ));
        let h = history(commits, &["t.ts", "p.ts", "hub.md"]);
        let view = co_change(&h, &targets(&["t.ts"]), &Settings::default());
        // 2 shared over the mean of 3 and 2, against 3 over the mean of 3 and 6
        assert_eq!(
            shown(&view),
            [("p.ts".into(), 2, 2), ("hub.md".into(), 3, 6)]
        );
    }

    #[test]
    fn large_commits_merges_and_boundaries_are_left_out_and_counted() {
        let many: Vec<(String, ChangeKind)> = (0..31).map(|n| (format!("f{n}.ts"), M)).collect();
        let mut big = commit("big", &["c1"], &[]);
        big.changes = many
            .iter()
            .map(|(p, k)| Change {
                path: p.clone(),
                kind: k.clone(),
            })
            .chain([Change {
                path: "price.ts".into(),
                kind: M,
            }])
            .collect();
        let mut boundary = commit("c0", &[], &[]);
        boundary.boundary = true;
        let h = history(
            vec![
                commit("m", &["big", "side"], &[]),
                big,
                commit("side", &["c1"], &[("price.ts", M), ("rates.yaml", M)]),
                commit("c1", &["c0"], &[("other.ts", M)]),
                boundary,
            ],
            &["price.ts", "rates.yaml", "other.ts", "f0.ts"],
        );
        let view = co_change(&h, &targets(&["price.ts"]), &Settings::default());
        assert_eq!(
            view.counts,
            Counts {
                read: 5,
                counted: 2,
                large: 1,
                merges: 1,
                boundaries: 1
            }
        );
        assert_eq!(shown(&view), [("rates.yaml".into(), 1, 1)]);
        // only in a left-out commit: said so
        let only = co_change(&h, &targets(&["f0.ts"]), &Settings::default());
        assert_eq!(only.none, Some(NoCommit::OnlyLeftOut { large: 1 }));
    }

    #[test]
    fn renames_carry_a_file_back_and_a_reused_path_is_another_file() {
        let renamed = |from: &str, similarity| ChangeKind::Renamed {
            from: from.into(),
            similarity,
        };
        let h = history(
            vec![
                // a new file at the old path: another file
                commit("c5", &["c4"], &[("old.ts", A), ("price.ts", M)]),
                commit(
                    "c4",
                    &["c3"],
                    &[("new.ts", renamed("old.ts", 80)), ("price.ts", M)],
                ),
                // a move without edits is no change, but carries identity
                commit(
                    "c3",
                    &["c2"],
                    &[("old.ts", renamed("older.ts", 100)), ("price.ts", M)],
                ),
                commit("c2", &["c1"], &[("older.ts", M), ("price.ts", M)]),
                commit("c1", &[], &[("older.ts", A), ("price.ts", A)]),
            ],
            &["new.ts", "old.ts", "price.ts"],
        );
        let view = co_change(&h, &targets(&["price.ts"]), &Settings::default());
        // new.ts in c4, and as older.ts in c2 and c1; old.ts only in c5
        assert_eq!(
            shown(&view),
            [("new.ts".into(), 3, 3), ("old.ts".into(), 1, 1)]
        );
        let new = &view.files[0];
        assert_eq!(new.earlier.get("c2").map(String::as_str), Some("older.ts"));
        // not followed: each path on its own
        let flat = co_change(
            &h,
            &targets(&["price.ts"]),
            &Settings {
                follow_renames: false,
                ..Settings::default()
            },
        );
        assert_eq!(
            shown(&flat),
            [
                ("older.ts".into(), 2, 2),
                ("new.ts".into(), 1, 1),
                ("old.ts".into(), 1, 1)
            ]
        );
    }

    #[test]
    fn a_commits_files_are_named_as_head_holds_them_now() {
        let renamed = |from: &str| ChangeKind::Renamed {
            from: from.into(),
            similarity: 80,
        };
        let h = history(
            vec![
                commit("c4", &["c3"], &[("old.ts", A), ("gone.ts", D)]),
                commit("c3", &["c2"], &[("new.ts", renamed("old.ts"))]),
                commit("c2", &["c1"], &[("old.ts", M), ("gone.ts", M)]),
                commit("c1", &[], &[("old.ts", A), ("gone.ts", A)]),
            ],
            &["new.ts", "old.ts"],
        );
        let files = commit_files(&h, &["c4", "c2", "missing"].into_iter().collect());
        let file = |path: &str, now: Option<&str>, in_head| CommitFile {
            path: path.into(),
            now: now.map(Into::into),
            in_head,
        };
        assert_eq!(
            files.get("c2").unwrap(),
            &[
                file("old.ts", Some("new.ts"), true),
                file("gone.ts", None, false)
            ]
        );
        // the path an add reuses is another file
        assert_eq!(
            files.get("c4").unwrap(),
            &[file("old.ts", None, true), file("gone.ts", None, false)]
        );
        assert!(!files.contains_key("missing"));
    }

    #[test]
    fn a_target_without_counted_commits_says_why() {
        let h = history(vec![commit("c1", &[], &[("a.ts", A)])], &["a.ts"]);
        let new = co_change(&h, &targets(&["new.ts"]), &Settings::default());
        assert_eq!(new.none, Some(NoCommit::NotInHead));
        let mut bounded = h.clone();
        bounded.head_files.insert("b.ts".into(), HeadEntry::File);
        bounded.reached_bound = true;
        let old = co_change(&bounded, &targets(&["b.ts"]), &Settings::default());
        assert_eq!(old.none, Some(NoCommit::NoneRead { older: true }));
        // a shallow clone's boundary shows its whole tree as added: what
        // changed there is not known
        let mut shallow = history(
            vec![
                commit("c2", &["c1"], &[]),
                commit("c1", &[], &[("b.ts", A)]),
            ],
            &["b.ts"],
        );
        shallow.commits[1].boundary = true;
        let cut = co_change(&shallow, &targets(&["b.ts"]), &Settings::default());
        assert_eq!(cut.none, Some(NoCommit::NoneRead { older: true }));
    }

    #[test]
    fn children_come_before_parents_whatever_the_print_order() {
        // printed parent first, as skewed clocks can make git print it
        let commits = vec![
            commit("p", &[], &[]),
            commit("c", &["p"], &[]),
            commit("x", &[], &[]),
        ];
        let order: Vec<&str> = topological(&commits)
            .into_iter()
            .map(|i| commits[i].id.as_str())
            .collect();
        assert_eq!(order, ["c", "p", "x"]);
    }

    #[test]
    fn deleted_files_stay_out_of_head() {
        let h = history(
            vec![
                commit("c2", &["c1"], &[("gone.ts", D), ("price.ts", M)]),
                commit("c1", &[], &[("gone.ts", A), ("price.ts", A)]),
            ],
            &["price.ts"],
        );
        let view = co_change(&h, &targets(&["price.ts"]), &Settings::default());
        assert_eq!(view.files.len(), 1);
        assert!(!view.files[0].in_head);
    }
}
