//! The committed git history of a root, read with the git CLI: commits,
//! parents, committer times and the files each changed under the root.
//!
//! Every call pins what configuration could change: it reads no
//! uncommitted change, fetches nothing (a partial clone's missing blobs stay
//! missing), runs no program the repository configures (`core.fsmonitor`,
//! a pager, a credential prompt), follows no replace refs, and asks for
//! renames one way. `--name-status` runs no textconv and no external diff;
//! the flags say so to a later reader. The environment's `GIT_DIR` and its
//! kin are dropped, so a caller inside a git hook cannot point archmap at
//! another repository; `GIT_CEILING_DIRECTORIES` is the user's and stays.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use archmap_core::history::{
    Change, ChangeKind, Commit, HeadEntry, History, HistoryState, Renames,
};

/// Commits read unless told otherwise: the first ones git's default walk
/// lists from HEAD that touch the root.
pub const DEFAULT_BOUND: usize = 10_000;

/// `-M50%`, git's own default similarity.
const SIMILARITY: u8 = 50;
/// `-l1000`: past this many candidate pairs in a commit, git looks for exact
/// renames only.
const RENAME_LIMIT: u32 = 1000;

/// Changes kept per commit; past it, a commit keeps the first ones by path
/// and counts the rest, so one vendoring or codemod commit cannot hold
/// hundreds of thousands of paths.
pub const MAX_CHANGES: usize = 1_000;

/// Variables that would point git at another repository, carry a caller's
/// configuration, or graft another history on (repository-local grafts
/// stay: they are the repository's own state). `LANGUAGE` would translate
/// git's messages, which the reader matches in English.
const DROPPED: [&str; 11] = [
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_INDEX_FILE",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_COMMON_DIR",
    "GIT_NAMESPACE",
    "GIT_CONFIG_PARAMETERS",
    "GIT_CONFIG_COUNT",
    "GIT_GRAFT_FILE",
    "LANGUAGE",
];

/// Read the history of `root`, at most `bound` commits. Never fails: a
/// history that cannot be read says why in its state.
pub fn read(root: &Path, bound: usize) -> History {
    let located = match locate(root) {
        Ok(located) => located,
        Err(state) => return History::unread(state, bound),
    };
    let git_version = run(root, &["--version"])
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned());
    let partial = partial_clone(root);
    if partial == Partial::WithoutTrees {
        let mut history = History::unread(
            HistoryState::Unreadable {
                reason: "a partial clone without trees".to_owned(),
            },
            bound,
        );
        history.git_version = git_version;
        return history;
    }
    let renames = match partial {
        Partial::No => Renames::Detected {
            similarity: SIMILARITY,
            limit: RENAME_LIMIT,
            inexact_skipped: false,
        },
        _ => Renames::NotDetected,
    };
    let mut history = History {
        state: HistoryState::Read {
            head: located.head,
            shallow: located.shallow.is_some(),
            partial: partial != Partial::No,
        },
        prefix: located.prefix.trim_end_matches('/').to_owned(),
        git_version,
        bound,
        reached_bound: false,
        renames,
        commits: Vec::new(),
        head_files: BTreeMap::new(),
        skipped_paths: 0,
    };
    let log = match run(root, &log_args(bound, renames)) {
        Ok(out) if out.status.success() => out,
        Ok(out) => return failed(history, &out),
        Err(error) => return failed_to_start(history, &error),
    };
    if String::from_utf8_lossy(&log.stderr).contains("rename detection was skipped") {
        if let Renames::Detected {
            inexact_skipped, ..
        } = &mut history.renames
        {
            *inexact_skipped = true;
        }
    }
    let (commits, skipped) = parse_log(&log.stdout);
    history.reached_bound = commits.len() >= bound;
    history.commits = commits;
    history.skipped_paths = skipped;
    if let Some(shallow) = &located.shallow {
        mark_boundaries(&mut history.commits, shallow);
    }
    match run(root, &["ls-tree", "-r", "-z", "HEAD"]) {
        Ok(out) if out.status.success() => {
            let (files, skipped) = parse_tree(&out.stdout);
            history.head_files = files;
            history.skipped_paths += skipped;
        }
        Ok(out) => return failed(history, &out),
        Err(error) => return failed_to_start(history, &error),
    }
    history
}

/// What a history read at `root` would start from: HEAD and whether the
/// clone is shallow, as git prints them, or `None` where git cannot say (no
/// repository, no commit, no git). A long-running caller compares it to
/// know when to read the history again: a commit or a checkout changes no
/// file a scan reads, and a `fetch --unshallow` changes no HEAD.
pub(crate) fn head_stamp(root: &Path) -> Option<String> {
    let out = run(root, &["rev-parse", "--is-shallow-repository", "HEAD"]).ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_owned())
}

/// The URL of the root's `origin` remote, as git configures it.
pub fn origin_url(root: &Path) -> Option<String> {
    let out = run(root, &["remote", "get-url", "origin"]).ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_owned())
}

/// Where a commit that a history read does not hold stands in the local
/// repository.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocalCommit {
    /// The repository has no such object.
    Missing,
    /// An ancestor of HEAD that the bounded read did not reach.
    BeyondRead,
    /// Present, but HEAD does not contain it (another branch).
    NotInHead,
}

/// Look `shas` up in the repository at `root`: one `cat-file
/// --batch-check` for them all, then `merge-base --is-ancestor` for each
/// present one, in the same hardened environment as the history read (a
/// partial clone fetches nothing). `None` when git cannot answer.
pub fn local_commits(root: &Path, shas: &[String]) -> Option<BTreeMap<String, LocalCommit>> {
    use std::io::Write;
    let mut child = git(root)
        .args(["cat-file", "--batch-check=%(objectname) %(objecttype)"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    {
        let mut stdin = child.stdin.take()?;
        for sha in shas {
            // an object name only: anything else could name a revision
            if !sha.bytes().all(|b| b.is_ascii_hexdigit()) {
                continue;
            }
            writeln!(stdin, "{sha}").ok()?;
        }
    }
    let out = child.wait_with_output().ok()?;
    if !out.status.success() {
        return None;
    }
    let mut found = BTreeMap::new();
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        let mut fields = line.split_whitespace();
        let (Some(sha), kind) = (fields.next(), fields.next()) else {
            continue;
        };
        let state = match kind {
            Some("commit") => {
                let ancestor = run(root, &["merge-base", "--is-ancestor", sha, "HEAD"]).ok()?;
                match ancestor.status.code() {
                    Some(0) => LocalCommit::BeyondRead,
                    Some(1) => LocalCommit::NotInHead,
                    _ => return None,
                }
            }
            _ => LocalCommit::Missing,
        };
        found.insert(sha.to_owned(), state);
    }
    Some(found)
}

/// The arguments of the log that lists the commits and their changes.
fn log_args(bound: usize, renames: Renames) -> Vec<String> {
    let mut args: Vec<String> = [
        "-c",
        "diff.renames=false",
        "-c",
        "log.follow=false",
        "-c",
        "log.showSignature=false",
        "-c",
        "log.showRoot=true",
        "-c",
        "diff.relative=false",
        "log",
        "-z",
        "--no-color",
        "--no-ext-diff",
        "--no-textconv",
        "--no-show-signature",
        "--diff-merges=off",
        "--full-history",
        "--relative",
        "--format=%x1e%H %P%x1f%ct",
        "--name-status",
    ]
    .map(str::to_owned)
    .into();
    match renames {
        Renames::Detected {
            similarity, limit, ..
        } => {
            args.push(format!("-M{similarity}%"));
            args.push(format!("-l{limit}"));
        }
        Renames::NotDetected => args.push("--no-renames".to_owned()),
    }
    args.extend([
        format!("-n{bound}"),
        "HEAD".to_owned(),
        "--".to_owned(),
        ".".to_owned(),
    ]);
    args
}

struct Located {
    head: String,
    prefix: String,
    /// The shallow file, when the clone is shallow.
    shallow: Option<PathBuf>,
}

/// Where the root sits in its repository, and HEAD.
fn locate(root: &Path) -> Result<Located, HistoryState> {
    let out = run(
        root,
        &[
            "rev-parse",
            "--is-inside-work-tree",
            "--show-prefix",
            "--is-shallow-repository",
            "--git-path",
            "shallow",
        ],
    )
    .map_err(|e| start_state(&e))?;
    if !out.status.success() {
        return Err(state_of(&out));
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let lines: Vec<&str> = text.lines().collect();
    let [inside, prefix, shallow, shallow_file] = lines.as_slice() else {
        return Err(HistoryState::Unreadable {
            reason: format!("unexpected rev-parse output: {}", text.trim()),
        });
    };
    if *inside != "true" {
        return Err(HistoryState::NotGit);
    }
    let head = run(root, &["rev-parse", "--verify", "--quiet", "HEAD^{commit}"])
        .map_err(|e| start_state(&e))?;
    if !head.status.success() {
        return Err(HistoryState::NoCommits);
    }
    let shallow = (*shallow == "true").then(|| root.join(shallow_file));
    Ok(Located {
        head: String::from_utf8_lossy(&head.stdout).trim().to_owned(),
        prefix: (*prefix).to_owned(),
        shallow,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Partial {
    No,
    /// Blobs may be missing: inexact renames need them.
    WithBlobs,
    /// Trees may be missing: even names cannot be read without fetching.
    WithoutTrees,
}

/// Whether the clone is partial, from its configuration: a promisor remote
/// and the filter it was cloned with.
fn partial_clone(root: &Path) -> Partial {
    let Ok(out) = run(
        root,
        &[
            "config",
            "--get-regexp",
            r"^(extensions\.partialclone|remote\..*\.(promisor|partialclonefilter))$",
        ],
    ) else {
        return Partial::No;
    };
    let text = String::from_utf8_lossy(&out.stdout).to_lowercase();
    let promisor = text.lines().any(|line| {
        line.starts_with("extensions.partialclone ")
            || (line.contains(".promisor ") && line.ends_with(" true"))
    });
    if !promisor {
        return Partial::No;
    }
    let trees = text
        .lines()
        .any(|line| line.contains(".partialclonefilter ") && line.contains("tree:"));
    match trees {
        true => Partial::WithoutTrees,
        false => Partial::WithBlobs,
    }
}

/// git at `root`, with a caller's repository variables dropped, nothing
/// fetched, and no program the repository configures.
fn git(root: &Path) -> Command {
    let mut command = Command::new("git");
    for (key, _) in std::env::vars_os() {
        let key = key.to_string_lossy();
        if key.starts_with("GIT_CONFIG_KEY_") || key.starts_with("GIT_CONFIG_VALUE_") {
            command.env_remove(key.as_ref());
        }
    }
    for variable in DROPPED {
        command.env_remove(variable);
    }
    command
        // untranslated messages, which `state_of` reads
        .env("LC_ALL", "C")
        .env("GIT_NO_LAZY_FETCH", "1")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_PAGER", "cat")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_NO_REPLACE_OBJECTS", "1")
        .arg("-C")
        .arg(root)
        .args([
            "-c",
            "core.fsmonitor=false",
            "-c",
            "protocol.allow=never",
            "-c",
            "core.quotepath=off",
            "--no-pager",
        ])
        .stdin(Stdio::null());
    command
}

fn run<S: AsRef<OsStr>>(root: &Path, args: &[S]) -> std::io::Result<Output> {
    git(root).args(args).output()
}

fn start_state(error: &std::io::Error) -> HistoryState {
    match error.kind() {
        std::io::ErrorKind::NotFound => HistoryState::GitMissing,
        _ => HistoryState::Unreadable {
            reason: error.to_string(),
        },
    }
}

/// What a failed git call says about the repository.
fn state_of(out: &Output) -> HistoryState {
    let stderr = String::from_utf8_lossy(&out.stderr);
    if stderr.contains("dubious ownership") {
        HistoryState::DubiousOwnership
    } else if stderr.contains("not a git repository") {
        HistoryState::NotGit
    } else {
        HistoryState::Unreadable {
            reason: stderr
                .lines()
                .find(|l| !l.trim().is_empty())
                .unwrap_or("git failed")
                .trim()
                .to_owned(),
        }
    }
}

fn failed(mut history: History, out: &Output) -> History {
    history.state = state_of(out);
    history.commits.clear();
    history
}

fn failed_to_start(mut history: History, error: &std::io::Error) -> History {
    history.state = start_state(error);
    history.commits.clear();
    history
}

/// The commits of a `-z` log with `%x1e%H %P%x1f%ct` and `--name-status`,
/// and how many paths were left out for not being UTF-8.
fn parse_log(bytes: &[u8]) -> (Vec<Commit>, usize) {
    let mut commits = Vec::new();
    let mut skipped = 0;
    for record in bytes.split(|&b| b == 0x1e).filter(|r| !r.is_empty()) {
        let Some(split) = record.iter().position(|&b| b == 0x1f) else {
            continue;
        };
        let header = String::from_utf8_lossy(&record[..split]);
        let mut ids = header.split_whitespace();
        let Some(id) = ids.next() else {
            continue;
        };
        let parents: Vec<String> = ids.map(str::to_owned).collect();
        let mut fields = record[split + 1..].split(|&b| b == 0);
        let time = fields
            .next()
            .and_then(|t| std::str::from_utf8(t).ok())
            .and_then(|t| t.trim().parse().ok())
            .unwrap_or(0);
        let mut changes = Vec::new();
        let mut fields = fields.map(|f| f.strip_prefix(b"\n").unwrap_or(f));
        while let Some(status) = fields.next() {
            let Some(&code) = status.first() else {
                continue;
            };
            let path = |field: Option<&[u8]>, skipped: &mut usize| match field {
                Some(f) => match std::str::from_utf8(f) {
                    Ok(path) => Some(path.to_owned()),
                    Err(_) => {
                        *skipped += 1;
                        None
                    }
                },
                None => None,
            };
            let kind = match code {
                b'R' | b'C' => {
                    let from = path(fields.next(), &mut skipped);
                    let to = path(fields.next(), &mut skipped);
                    let (Some(from), Some(to)) = (from, to) else {
                        continue;
                    };
                    let similarity = std::str::from_utf8(&status[1..])
                        .ok()
                        .and_then(|s| s.parse().ok())
                        .unwrap_or(0);
                    if code == b'C' {
                        // copies were not asked for; read one as an add
                        changes.push(Change {
                            path: to,
                            kind: ChangeKind::Added,
                        });
                    } else {
                        changes.push(Change {
                            path: to,
                            kind: ChangeKind::Renamed { from, similarity },
                        });
                    }
                    continue;
                }
                b'A' => ChangeKind::Added,
                b'M' => ChangeKind::Modified,
                b'D' => ChangeKind::Deleted,
                b'T' => ChangeKind::TypeChanged,
                _ => {
                    fields.next();
                    continue;
                }
            };
            if let Some(path) = path(fields.next(), &mut skipped) {
                changes.push(Change { path, kind });
            }
        }
        changes.sort();
        let omitted = changes.len().saturating_sub(MAX_CHANGES);
        changes.truncate(MAX_CHANGES);
        commits.push(Commit {
            id: id.to_owned(),
            parents,
            time,
            boundary: false,
            changes,
            omitted,
        });
    }
    (commits, skipped)
}

/// A shallow clone's boundary commits: git shows their whole tree as added,
/// so their changes are no facts of theirs.
fn mark_boundaries(commits: &mut [Commit], shallow: &Path) {
    let Ok(text) = std::fs::read_to_string(shallow) else {
        return;
    };
    let boundaries: BTreeSet<&str> = text.lines().map(str::trim).collect();
    for commit in commits {
        if boundaries.contains(commit.id.as_str()) {
            commit.boundary = true;
            commit.changes.clear();
            commit.omitted = 0;
        }
    }
}

/// HEAD's entries from `ls-tree -r -z`: files and submodules, by path, and
/// how many paths were left out for not being UTF-8.
fn parse_tree(bytes: &[u8]) -> (BTreeMap<String, HeadEntry>, usize) {
    let mut files = BTreeMap::new();
    let mut skipped = 0;
    for entry in bytes.split(|&b| b == 0).filter(|e| !e.is_empty()) {
        let Some(tab) = entry.iter().position(|&b| b == b'\t') else {
            continue;
        };
        let meta = String::from_utf8_lossy(&entry[..tab]);
        let Ok(path) = std::str::from_utf8(&entry[tab + 1..]) else {
            skipped += 1;
            continue;
        };
        let kind = match meta.split_whitespace().nth(1) {
            Some("commit") => HeadEntry::Submodule,
            _ => HeadEntry::File,
        };
        files.insert(path.to_owned(), kind);
    }
    (files, skipped)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_log_gives_commits_parents_times_and_changes() {
        let log = b"\x1eaaa bbb\x1f1700000100\0\nM\0src/a.ts\0R087\0old.ts\0new.ts\0A\0b\xffc\0\x1ebbb ccc ddd\x1f1700000050\0\x1eccc\x1f1700000000\0\nA\0src/a.ts\0";
        let (commits, skipped) = parse_log(log);
        assert_eq!(skipped, 1);
        assert_eq!(commits.len(), 3);
        assert_eq!(commits[0].id, "aaa");
        assert_eq!(commits[0].parents, ["bbb"]);
        assert_eq!(commits[0].time, 1_700_000_100);
        assert_eq!(
            commits[0].changes,
            [
                Change {
                    path: "new.ts".into(),
                    kind: ChangeKind::Renamed {
                        from: "old.ts".into(),
                        similarity: 87
                    }
                },
                Change {
                    path: "src/a.ts".into(),
                    kind: ChangeKind::Modified
                },
            ]
        );
        // a merge is read for its parents only
        assert!(commits[1].is_merge() && commits[1].changes.is_empty());
        assert!(commits[2].parents.is_empty());
    }

    #[test]
    fn a_tree_listing_tells_files_from_submodules() {
        let listing = b"100644 blob 1111\tsrc/a.ts\x00160000 commit 2222\tvendor/lib\x00100644 blob 3333\tb\xff\0";
        let (files, skipped) = parse_tree(listing);
        assert_eq!(skipped, 1);
        assert_eq!(files.get("src/a.ts"), Some(&HeadEntry::File));
        assert_eq!(files.get("vendor/lib"), Some(&HeadEntry::Submodule));
    }

    #[test]
    fn git_speaks_english_and_reads_no_caller_repository() {
        let command = git(Path::new("."));
        let envs: BTreeMap<String, Option<String>> = command
            .get_envs()
            .map(|(k, v)| {
                (
                    k.to_string_lossy().into_owned(),
                    v.map(|v| v.to_string_lossy().into_owned()),
                )
            })
            .collect();
        assert_eq!(envs.get("LC_ALL"), Some(&Some("C".to_owned())));
        for dropped in ["LANGUAGE", "GIT_DIR", "GIT_GRAFT_FILE"] {
            assert_eq!(envs.get(dropped), Some(&None), "{dropped}");
        }
    }

    #[test]
    fn a_huge_commit_keeps_the_first_changes_and_counts_the_rest() {
        let mut log = b"\x1eaaa\x1f1\0\n".to_vec();
        for n in 0..MAX_CHANGES + 5 {
            log.extend(format!("A\0f{n:05}.ts\0").as_bytes());
        }
        let (commits, _) = parse_log(&log);
        assert_eq!(commits[0].changes.len(), MAX_CHANGES);
        assert_eq!(commits[0].omitted, 5);
        assert_eq!(commits[0].changes[0].path, "f00000.ts");
    }

    #[test]
    fn git_failures_say_what_they_mean() {
        let out = |stderr: &str| Output {
            status: std::process::ExitStatus::default(),
            stdout: Vec::new(),
            stderr: stderr.as_bytes().to_vec(),
        };
        assert_eq!(
            state_of(&out(
                "fatal: not a git repository (or any of the parent directories): .git\n"
            )),
            HistoryState::NotGit
        );
        assert_eq!(
            state_of(&out(
                "fatal: detected dubious ownership in repository at '/r'\n"
            )),
            HistoryState::DubiousOwnership
        );
        assert_eq!(
            state_of(&out("\nfatal: bad object HEAD\n")),
            HistoryState::Unreadable {
                reason: "fatal: bad object HEAD".into()
            }
        );
    }
}
