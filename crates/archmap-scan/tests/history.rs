//! The committed git history of a root, read from repositories the tests
//! build with fixed identities and dates, so their SHAs are the same on
//! every machine.

use std::path::{Path, PathBuf};
use std::process::Command;

use archmap_core::history::{Change, ChangeKind, HeadEntry, History, HistoryState, Renames};
use archmap_scan::history::{read, DEFAULT_BOUND};

/// A repository in a temp directory, its git config kept apart from the
/// user's (this isolation is for tests only: the product keeps it).
struct Repo {
    base: PathBuf,
    dir: PathBuf,
    commits: u32,
}

impl Repo {
    fn new(name: &str) -> Repo {
        let base =
            std::env::temp_dir().join(format!("archmap-history-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let dir = base.join("repo");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::create_dir_all(base.join("home")).unwrap();
        let repo = Repo {
            base,
            dir,
            commits: 0,
        };
        repo.git(&["init", "-q", "--object-format=sha1", "-b", "main"]);
        repo
    }

    fn command(&self, dir: &Path) -> Command {
        let date = format!("2026-01-{:02}T00:00:00Z", self.commits + 1);
        let mut command = Command::new("git");
        command
            .arg("-C")
            .arg(dir)
            .args([
                "-c",
                "core.autocrlf=false",
                "-c",
                "core.filemode=true",
                "-c",
                "commit.gpgsign=false",
                "-c",
                "init.defaultBranch=main",
                "-c",
                "uploadpack.allowFilter=true",
                "-c",
                "protocol.file.allow=always",
            ])
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("HOME", self.base.join("home"))
            .env("GIT_AUTHOR_NAME", "A")
            .env("GIT_AUTHOR_EMAIL", "a@example.com")
            .env("GIT_COMMITTER_NAME", "A")
            .env("GIT_COMMITTER_EMAIL", "a@example.com")
            .env("GIT_AUTHOR_DATE", &date)
            .env("GIT_COMMITTER_DATE", &date);
        command
    }

    fn git_in(&self, dir: &Path, args: &[&str]) -> String {
        let out = self.command(dir).args(args).output().unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap().trim().to_owned()
    }

    fn git(&self, args: &[&str]) -> String {
        self.git_in(&self.dir, args)
    }

    fn write(&self, path: &str, text: &str) {
        let path = self.dir.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    /// Commit everything, one day after the last commit; the new HEAD.
    fn commit(&mut self, message: &str) -> String {
        self.git(&["add", "-A"]);
        self.git(&["commit", "-q", "--allow-empty", "-m", message]);
        self.commits += 1;
        self.git(&["rev-parse", "HEAD"])
    }
}

impl Drop for Repo {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.base);
    }
}

fn changes(history: &History, id: &str) -> Vec<Change> {
    history
        .commits
        .iter()
        .find(|c| c.id == id)
        .unwrap_or_else(|| panic!("no commit {id}"))
        .changes
        .clone()
}

fn change(path: &str, kind: ChangeKind) -> Change {
    Change {
        path: path.into(),
        kind,
    }
}

/// Lines long enough that an edit keeps a rename similar.
fn body(tag: &str) -> String {
    (0..20)
        .map(|i| format!("line {i} of the file\n"))
        .collect::<String>()
        + tag
}

#[test]
fn commits_give_their_parents_times_and_the_files_they_changed() {
    let mut repo = Repo::new("basic");
    repo.write("src/a.ts", &body("a"));
    repo.write("src/b.ts", &body("b"));
    let first = repo.commit("first");
    repo.write("src/a.ts", &body("a2"));
    repo.write("config/rates.yaml", "rate: 1\n");
    let second = repo.commit("second");
    // moved with an edit, and deleted
    std::fs::remove_file(repo.dir.join("src/b.ts")).unwrap();
    repo.write("lib/b.ts", &body("b2"));
    std::fs::remove_file(repo.dir.join("config/rates.yaml")).unwrap();
    let third = repo.commit("third");

    let history = read(&repo.dir, DEFAULT_BOUND);
    assert_eq!(
        history.state,
        HistoryState::Read {
            head: third.clone(),
            shallow: false,
            partial: false
        }
    );
    assert_eq!(history.prefix, "");
    assert!(history
        .git_version
        .as_deref()
        .is_some_and(|v| v.starts_with("git version")));
    assert_eq!(
        history.renames,
        Renames::Detected {
            similarity: 50,
            limit: 1000,
            inexact_skipped: false
        }
    );
    let ids: Vec<&str> = history.commits.iter().map(|c| c.id.as_str()).collect();
    assert_eq!(ids, [third.as_str(), second.as_str(), first.as_str()]);
    assert_eq!(history.commits[0].parents, std::slice::from_ref(&second));
    assert!(history.commits[2].parents.is_empty());
    // committer dates, one day apart
    assert_eq!(history.commits[1].time - history.commits[2].time, 86_400);
    assert_eq!(
        changes(&history, &first),
        [
            change("src/a.ts", ChangeKind::Added),
            change("src/b.ts", ChangeKind::Added)
        ]
    );
    assert_eq!(
        changes(&history, &second),
        [
            change("config/rates.yaml", ChangeKind::Added),
            change("src/a.ts", ChangeKind::Modified)
        ]
    );
    let third_changes = changes(&history, &third);
    assert_eq!(
        third_changes[0],
        change("config/rates.yaml", ChangeKind::Deleted)
    );
    match &third_changes[1].kind {
        ChangeKind::Renamed { from, similarity } => {
            assert_eq!(third_changes[1].path, "lib/b.ts");
            assert_eq!(from, "src/b.ts");
            assert!((50..100).contains(similarity), "{similarity}");
        }
        other => panic!("not a rename: {other:?}"),
    }
    let files: Vec<&str> = history.head_files.keys().map(String::as_str).collect();
    assert_eq!(files, ["lib/b.ts", "src/a.ts"]);
    assert!(!history.reached_bound);
    // the same HEAD gives the same history
    assert_eq!(read(&repo.dir, DEFAULT_BOUND), history);
}

#[test]
fn a_merge_is_read_for_its_parents_and_its_branch_commits_for_their_changes() {
    let mut repo = Repo::new("merge");
    repo.write("a.ts", "a\n");
    repo.write("b.ts", "b\n");
    repo.commit("base");
    repo.git(&["checkout", "-q", "-b", "feature"]);
    repo.write("a.ts", "a2\n");
    let feature = repo.commit("feature");
    repo.git(&["checkout", "-q", "main"]);
    repo.write("b.ts", "b2\n");
    let main = repo.commit("main");
    repo.git(&["merge", "-q", "--no-ff", "-m", "merge", "feature"]);
    repo.commits += 1;
    let merge = repo.git(&["rev-parse", "HEAD"]);

    let history = read(&repo.dir, DEFAULT_BOUND);
    let merged = history.commits.iter().find(|c| c.id == merge).unwrap();
    assert!(merged.is_merge());
    assert_eq!(merged.parents, [main.clone(), feature.clone()]);
    assert!(merged.changes.is_empty());
    assert_eq!(
        changes(&history, &feature),
        [change("a.ts", ChangeKind::Modified)]
    );
    assert_eq!(
        changes(&history, &main),
        [change("b.ts", ChangeKind::Modified)]
    );
}

#[test]
fn a_move_without_edits_is_a_rename_of_similarity_100() {
    let mut repo = Repo::new("move");
    repo.write("src/a.ts", &body("a"));
    repo.commit("first");
    std::fs::create_dir_all(repo.dir.join("lib")).unwrap();
    repo.git(&["mv", "src/a.ts", "lib/a.ts"]);
    let moved = repo.commit("move");
    let history = read(&repo.dir, DEFAULT_BOUND);
    assert_eq!(
        changes(&history, &moved),
        [change(
            "lib/a.ts",
            ChangeKind::Renamed {
                from: "src/a.ts".into(),
                similarity: 100
            }
        )]
    );
}

#[test]
fn a_root_below_the_top_reads_the_commits_under_it_with_its_own_paths() {
    let mut repo = Repo::new("subdir");
    repo.write("app/main.ts", &body("main"));
    repo.write("lib/shared.ts", &body("shared"));
    let first = repo.commit("first");
    repo.write("lib/shared.ts", &body("shared2"));
    repo.commit("outside the root");
    // moved into the root: an add for the root
    std::fs::remove_file(repo.dir.join("lib/shared.ts")).unwrap();
    repo.write("app/shared.ts", &body("shared2"));
    let moved = repo.commit("move in");

    let root = repo.dir.join("app");
    let history = read(&root, DEFAULT_BOUND);
    assert_eq!(history.prefix, "app");
    let ids: Vec<&str> = history.commits.iter().map(|c| c.id.as_str()).collect();
    assert_eq!(ids, [moved.as_str(), first.as_str()]);
    assert_eq!(
        changes(&history, &first),
        [change("main.ts", ChangeKind::Added)]
    );
    assert_eq!(
        changes(&history, &moved),
        [change("shared.ts", ChangeKind::Added)]
    );
    let files: Vec<&str> = history.head_files.keys().map(String::as_str).collect();
    assert_eq!(files, ["main.ts", "shared.ts"]);
}

#[test]
fn uncommitted_changes_are_not_read() {
    let mut repo = Repo::new("uncommitted");
    repo.write("a.ts", "a\n");
    repo.commit("first");
    let before = read(&repo.dir, DEFAULT_BOUND);
    repo.write("a.ts", "edited, not committed\n");
    repo.write("new.ts", "untracked\n");
    repo.git(&["add", "new.ts"]);
    assert_eq!(read(&repo.dir, DEFAULT_BOUND), before);
    assert!(!before.head_files.contains_key("new.ts"));
}

#[test]
fn the_bound_says_when_it_was_reached() {
    let mut repo = Repo::new("bound");
    for n in 0..3 {
        repo.write("a.ts", &format!("{n}\n"));
        repo.commit("change");
    }
    let history = read(&repo.dir, 2);
    assert_eq!(history.commits.len(), 2);
    assert!(history.reached_bound);
    assert_eq!(history.bound, 2);
}

#[test]
fn a_shallow_clone_marks_its_boundary_and_reads_none_of_its_changes() {
    let mut repo = Repo::new("shallow");
    for n in 0..3 {
        repo.write("a.ts", &format!("{n}\n"));
        repo.write(&format!("f{n}.ts"), "x\n");
        repo.commit("change");
    }
    let clone = repo.base.join("shallow");
    let url = format!("file://{}", repo.dir.display());
    repo.git_in(
        &repo.base,
        &["clone", "-q", "--depth", "2", &url, clone.to_str().unwrap()],
    );
    let history = read(&clone, DEFAULT_BOUND);
    assert!(matches!(
        history.state,
        HistoryState::Read { shallow: true, .. }
    ));
    assert_eq!(history.commits.len(), 2);
    let boundary = &history.commits[1];
    assert!(boundary.boundary && boundary.changes.is_empty());
    assert!(!history.commits[0].boundary);
}

#[test]
fn a_clone_without_blobs_is_read_without_renames_and_fetches_nothing() {
    let mut repo = Repo::new("partial");
    repo.write("src/a.ts", &body("a"));
    repo.commit("first");
    std::fs::create_dir_all(repo.dir.join("lib")).unwrap();
    repo.git(&["mv", "src/a.ts", "lib/a.ts"]);
    repo.commit("move");
    let clone = repo.base.join("partial");
    let url = format!("file://{}", repo.dir.display());
    repo.git_in(
        &repo.base,
        &[
            "clone",
            "-q",
            "--filter=blob:none",
            "--no-checkout",
            &url,
            clone.to_str().unwrap(),
        ],
    );
    let history = read(&clone, DEFAULT_BOUND);
    assert!(matches!(
        history.state,
        HistoryState::Read { partial: true, .. }
    ));
    assert_eq!(history.renames, Renames::NotDetected);
    // a move is a delete and an add
    let kinds: Vec<&ChangeKind> = history.commits[0].changes.iter().map(|c| &c.kind).collect();
    assert_eq!(kinds, [&ChangeKind::Added, &ChangeKind::Deleted]);
}

#[test]
fn a_root_outside_any_repository_or_before_any_commit_has_no_history() {
    let dir = std::env::temp_dir().join(format!("archmap-history-none-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    assert_eq!(read(&dir, DEFAULT_BOUND).state, HistoryState::NotGit);
    std::fs::remove_dir_all(&dir).unwrap();

    let repo = Repo::new("empty");
    assert_eq!(
        read(&repo.dir, DEFAULT_BOUND).state,
        HistoryState::NoCommits
    );
}

#[test]
fn a_submodule_is_an_entry_of_its_own() {
    let mut repo = Repo::new("submodule");
    repo.write("a.ts", "a\n");
    let first = repo.commit("first");
    let gitlink = format!("160000,{first},vendor/lib");
    repo.git(&["update-index", "--add", "--cacheinfo", &gitlink]);
    repo.git(&["commit", "-q", "-m", "submodule"]);
    let history = read(&repo.dir, DEFAULT_BOUND);
    assert_eq!(
        history.head_files.get("vendor/lib"),
        Some(&HeadEntry::Submodule)
    );
    assert_eq!(history.head_files.get("a.ts"), Some(&HeadEntry::File));
}
