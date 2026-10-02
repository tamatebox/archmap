//! A stamp tells a long-running caller whether a scan would read anything
//! different, without scanning again.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use archmap_scan::stamp;

/// A throwaway repository, removed when the guard drops.
struct Repo(PathBuf);

impl Repo {
    fn new(name: &str) -> Repo {
        let dir = std::env::temp_dir().join(format!("archmap-stamp-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let repo = Repo(dir);
        repo.write(".gitignore", "ignored.py\n");
        repo.write("pkg/__init__.py", "def run():\n    pass\n");
        repo.write("pkg/core.py", "VALUE = 1\n");
        repo
    }

    fn write(&self, file: &str, text: &str) {
        let path = self.0.join(file);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    /// Set a file's modification time back an hour, so a later write is
    /// sure to differ even on a coarse clock.
    fn age(&self, file: &str) {
        let past = SystemTime::now() - Duration::from_secs(3600);
        std::fs::File::options()
            .write(true)
            .open(self.0.join(file))
            .unwrap()
            .set_modified(past)
            .unwrap();
    }
}

impl Drop for Repo {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn changes(repo: &Repo, change: impl FnOnce(&Path)) -> bool {
    let before = stamp(&repo.0).unwrap();
    change(&repo.0);
    stamp(&repo.0).unwrap() != before
}

#[test]
fn an_unchanged_tree_keeps_its_stamp() {
    let repo = Repo::new("same");
    assert_eq!(stamp(&repo.0).unwrap(), stamp(&repo.0).unwrap());
}

#[test]
fn editing_adding_deleting_or_renaming_a_file_changes_the_stamp() {
    let repo = Repo::new("edits");
    repo.age("pkg/core.py");
    // the same length, other bytes: only the modification time tells
    assert!(changes(&repo, |r| std::fs::write(
        r.join("pkg/core.py"),
        "VALUE = 2\n"
    )
    .unwrap()));
    assert!(changes(&repo, |r| std::fs::write(
        r.join("pkg/new.py"),
        "X = 1\n"
    )
    .unwrap()));
    assert!(changes(&repo, |r| std::fs::remove_file(
        r.join("pkg/new.py")
    )
    .unwrap()));
    assert!(changes(&repo, |r| {
        std::fs::rename(r.join("pkg/core.py"), r.join("pkg/main.py")).unwrap()
    }));
}

#[test]
fn files_the_scan_skips_leave_the_stamp_alone() {
    let repo = Repo::new("skipped");
    repo.write("ignored.py", "x = 1\n");
    // `target/` beside a `Cargo.toml` is build output
    repo.write("Cargo.toml", "[package]\nname = \"x\"\n");
    repo.write("target/debug/out.txt", "x\n");
    repo.write(".git/HEAD", "ref: refs/heads/main\n");
    assert!(!changes(&repo, |r| {
        std::fs::write(r.join("ignored.py"), "x = 22\n").unwrap();
        std::fs::write(r.join("target/debug/out.txt"), "yy\n").unwrap();
        std::fs::write(r.join(".git/HEAD"), "ref: refs/heads/other\n").unwrap();
    }));
}

#[test]
fn installing_into_the_virtualenv_changes_the_stamp() {
    // the Python analyzer reads `.venv`, which the walk skips as hidden
    let repo = Repo::new("venv");
    repo.write(
        "pyproject.toml",
        "[project]\nname = \"pkg\"\nversion = \"0.1.0\"\n",
    );
    let site = ".venv/lib/python3.12/site-packages";
    repo.write(
        &format!("{site}/requests-2.0.dist-info/RECORD"),
        "requests/__init__.py,,\n",
    );
    assert!(changes(&repo, |r| {
        std::fs::create_dir_all(r.join(site).join("pyyaml-6.0.dist-info")).unwrap()
    }));
}

#[cfg(unix)]
#[test]
fn a_symlinked_file_is_stamped_through_the_link() {
    let repo = Repo::new("link");
    let outside = Repo::new("link-target");
    outside.write("shared.py", "A = 1\n");
    outside.age("shared.py");
    std::os::unix::fs::symlink(outside.0.join("shared.py"), repo.0.join("pkg/shared.py")).unwrap();
    assert!(changes(&repo, |_| std::fs::write(
        outside.0.join("shared.py"),
        "A = 2\n"
    )
    .unwrap()));
}

#[test]
fn a_missing_root_is_an_error() {
    assert!(stamp(Path::new("/no/such/archmap/root")).is_err());
}

#[test]
fn a_commit_changes_the_stamp_though_no_file_does() {
    let repo = Repo::new("commit");
    let git = |args: &[&str]| {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(&repo.0)
            .args(["-c", "commit.gpgsign=false"])
            .args(args)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "A")
            .env("GIT_AUTHOR_EMAIL", "a@example.com")
            .env("GIT_COMMITTER_NAME", "A")
            .env("GIT_COMMITTER_EMAIL", "a@example.com")
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    };
    git(&["init", "-q", "-b", "main"]);
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "first"]);
    assert!(changes(&repo, |_| git(&[
        "commit",
        "-q",
        "--allow-empty",
        "-m",
        "second"
    ])));
}
