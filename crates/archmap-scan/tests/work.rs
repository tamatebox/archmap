//! A work snapshot read as written, and commits looked up in a repository
//! the test builds.

use std::path::{Path, PathBuf};
use std::process::Command;

use archmap_core::work::RelationType;
use archmap_scan::history::{local_commits, LocalCommit};
use archmap_scan::work::read;

fn temp(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("archmap-work-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

const SNAPSHOT: &str = r#"{
  "schema": 1, "source": "github", "host": "github.com", "repository": "acme/shop",
  "fetched_at": "2026-10-03T09:00:00Z",
  "range": {"updated_since": "2025-10-03T09:00:00Z", "since_rule": "given", "bound": 5000,
            "issues": 1, "pull_requests": 1},
  "relation_types": ["linked", "closes"],
  "items": [
    {"kind": "pull_request", "number": 15, "id": "PR_15", "state": "merged",
     "created_at": "2026-01-02T00:00:00Z", "updated_at": "2026-01-04T00:00:00Z"},
    {"kind": "issue", "number": 12, "id": "I_12", "state": "open",
     "created_at": "2026-01-01T00:00:00Z", "updated_at": "2026-01-04T00:00:00Z"}
  ],
  "relations": [
    {"type": "closes", "from": {"item": 15}, "to": {"item": 12},
     "observed": ["PullRequest.closingIssuesReferences"]},
    {"type": "closes", "from": {"item": 15}, "to": {"item": 12},
     "observed": ["Issue.closedByPullRequestsReferences"]}
  ]
}"#;

#[test]
fn a_snapshot_is_read_sorted_with_a_link_seen_from_both_ends_as_one() {
    let dir = temp("read");
    let path = dir.join("github.json");
    assert_eq!(read(&path), Ok(None));
    std::fs::write(&path, SNAPSHOT).unwrap();
    let snapshot = read(&path).unwrap().unwrap();
    assert_eq!(
        snapshot.items.iter().map(|i| i.number).collect::<Vec<_>>(),
        [12, 15]
    );
    assert_eq!(
        snapshot.relation_types,
        [RelationType::Closes, RelationType::Linked]
    );
    assert_eq!(snapshot.relations.len(), 1);
    assert_eq!(snapshot.relations[0].observed.len(), 2);
    // another schema is refused with what to do
    std::fs::write(&path, SNAPSHOT.replace("\"schema\": 1", "\"schema\": 2")).unwrap();
    let error = read(&path).unwrap_err();
    assert!(
        error.contains("schema 2") && error.contains("fetch it again"),
        "{error}"
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "commit.gpgsign=false"])
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "A")
        .env("GIT_AUTHOR_EMAIL", "a@example.com")
        .env("GIT_COMMITTER_NAME", "A")
        .env("GIT_COMMITTER_EMAIL", "a@example.com")
        .env("GIT_AUTHOR_DATE", "2026-01-01T00:00:00Z")
        .env("GIT_COMMITTER_DATE", "2026-01-01T00:00:00Z")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap().trim().to_owned()
}

#[test]
fn commits_are_looked_up_as_missing_in_head_or_on_another_branch() {
    let dir = temp("lookup");
    git(&dir, &["init", "-q", "-b", "main"]);
    std::fs::write(dir.join("a.txt"), "1\n").unwrap();
    git(&dir, &["add", "-A"]);
    git(&dir, &["commit", "-q", "-m", "one"]);
    let first = git(&dir, &["rev-parse", "HEAD"]);
    git(&dir, &["checkout", "-q", "-b", "side"]);
    std::fs::write(dir.join("a.txt"), "2\n").unwrap();
    git(&dir, &["commit", "-q", "-am", "side"]);
    let side = git(&dir, &["rev-parse", "HEAD"]);
    git(&dir, &["checkout", "-q", "main"]);
    let missing = "1111111111111111111111111111111111111111".to_owned();
    let found = local_commits(&dir, &[first.clone(), side.clone(), missing.clone()]).unwrap();
    assert_eq!(found[&first], LocalCommit::BeyondRead);
    assert_eq!(found[&side], LocalCommit::NotInHead);
    assert_eq!(found[&missing], LocalCommit::Missing);
    // anything but an object name is never asked
    let odd = local_commits(&dir, &["HEAD".to_owned()]).unwrap();
    assert!(odd.is_empty());
    std::fs::remove_dir_all(&dir).unwrap();
}
