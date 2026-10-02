//! What the committed git history says, as observed facts: commits, their
//! parents and committer times, and the files each changed under the root.
//! Co-change and the other views are computed from these facts when a
//! command asks; none of them is stored here.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// The history of a root, as far as it was read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct History {
    pub state: HistoryState,
    /// The root's path from the repository's top, with `/` separators:
    /// empty at the top.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub prefix: String,
    /// `git --version`, since rename detection can differ between versions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_version: Option<String>,
    /// The commits asked for: the first ones git's default walk lists from
    /// HEAD that touch the root.
    pub bound: usize,
    /// As many commits were read as asked for: older ones may exist.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub reached_bound: bool,
    pub renames: Renames,
    /// The commits read, in git's default order (newest committer date
    /// first, which skewed clocks can break).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub commits: Vec<Commit>,
    /// HEAD's files under the root, relative to it.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub head_files: BTreeMap<String, HeadEntry>,
    /// Paths left out because they are not UTF-8.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub skipped_paths: usize,
}

fn is_zero(n: &usize) -> bool {
    *n == 0
}

impl History {
    /// A history that could not be read, with why.
    pub fn unread(state: HistoryState, bound: usize) -> Self {
        History {
            state,
            prefix: String::new(),
            git_version: None,
            bound,
            reached_bound: false,
            renames: Renames::NotDetected,
            commits: Vec::new(),
            head_files: BTreeMap::new(),
            skipped_paths: 0,
        }
    }
}

/// Whether and how the history was read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "state")]
pub enum HistoryState {
    /// Read from HEAD.
    Read {
        /// HEAD's full SHA.
        head: String,
        /// A shallow clone: history ends at its depth.
        shallow: bool,
        /// A partial clone: blobs (and so inexact renames) may be missing.
        partial: bool,
    },
    /// The root is in no git repository.
    NotGit,
    /// No `git` command was found.
    GitMissing,
    /// git refuses a repository that another user owns, until
    /// `safe.directory` names it.
    DubiousOwnership,
    /// The repository has no commit yet.
    NoCommits,
    /// git failed otherwise; the first line it wrote.
    Unreadable { reason: String },
}

/// How renames were detected, as git was asked to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "renames")]
pub enum Renames {
    /// `-M<similarity>%` with `-l<limit>`.
    Detected {
        similarity: u8,
        limit: u32,
        /// git skipped inexact detection in some commit (more pairs than
        /// `limit`); exact renames are found regardless.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        inexact_skipped: bool,
    },
    /// Not detected: a rename is a delete and an add.
    NotDetected,
}

/// A commit, by its own SHA, parents and committer time; no author, email
/// or message is read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Commit {
    pub id: String,
    /// Its own parents, never rewritten: with a root below the repository's
    /// top, many of them touch nothing under it and are not read.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub parents: Vec<String>,
    /// Committer time, in seconds since the Unix epoch.
    pub time: i64,
    /// A shallow clone's boundary: git shows its whole tree as added, so
    /// its changes are not read.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub boundary: bool,
    /// The files it changed under the root, sorted by path; none for a
    /// merge, which is not diffed. A commit that changed more keeps the
    /// first ones and counts the rest in `omitted`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub changes: Vec<Change>,
    /// Changes past the ones kept.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub omitted: usize,
}

impl Commit {
    pub fn is_merge(&self) -> bool {
        self.parents.len() > 1
    }
}

/// A file a commit changed, relative to the root.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Change {
    pub path: String,
    #[serde(flatten)]
    pub kind: ChangeKind,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum ChangeKind {
    Added,
    Modified,
    Deleted,
    /// Its type changed: a file, a symlink or a submodule.
    TypeChanged,
    /// Moved from `from`, with git's similarity score (100: no edit).
    Renamed {
        from: String,
        similarity: u8,
    },
}

/// What HEAD holds at a path under the root.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HeadEntry {
    File,
    /// A submodule: a commit of another repository, whose files are not
    /// read.
    Submodule,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_keeps_facts_only_and_leaves_out_what_is_empty() {
        let commit = Commit {
            id: "a".repeat(40),
            parents: vec!["b".repeat(40)],
            time: 1_700_000_000,
            boundary: false,
            omitted: 0,
            changes: vec![
                Change {
                    path: "src/new.ts".into(),
                    kind: ChangeKind::Renamed {
                        from: "src/old.ts".into(),
                        similarity: 87,
                    },
                },
                Change {
                    path: "src/x.ts".into(),
                    kind: ChangeKind::Modified,
                },
            ],
        };
        assert_eq!(
            serde_json::to_value(&commit).unwrap(),
            serde_json::json!({
                "id": "a".repeat(40),
                "parents": ["b".repeat(40)],
                "time": 1_700_000_000,
                "changes": [
                    {"path": "src/new.ts", "kind": "renamed", "from": "src/old.ts", "similarity": 87},
                    {"path": "src/x.ts", "kind": "modified"}
                ]
            })
        );
        let unread = History::unread(HistoryState::NotGit, 10);
        assert_eq!(
            serde_json::to_value(&unread).unwrap(),
            serde_json::json!({
                "state": {"state": "not_git"},
                "bound": 10,
                "renames": {"renames": "not_detected"}
            })
        );
    }
}
