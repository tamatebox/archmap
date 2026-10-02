//! `query '#N'`: an item of the work snapshot, its links, and its commits
//! against a git history the test builds with fixed identities and dates.

use std::path::{Path, PathBuf};
use std::process::Command;

use archmap_app::{Format, QueryRequest, ScanMode, Workspace, DEFAULT_DEPTH};

/// A repository in a temp directory, removed when the guard drops.
struct Repo {
    dir: PathBuf,
    commits: u32,
}

impl Repo {
    fn new(name: &str) -> Repo {
        let dir =
            std::env::temp_dir().join(format!("archmap-app-work-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let repo = Repo { dir, commits: 0 };
        repo.git(&["init", "-q", "-b", "main"]);
        repo
    }

    fn git(&self, args: &[&str]) -> String {
        let date = format!("2026-01-{:02}T00:00:00Z", self.commits + 1);
        let out = Command::new("git")
            .arg("-C")
            .arg(&self.dir)
            .args(["-c", "commit.gpgsign=false"])
            .args(args)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "A")
            .env("GIT_AUTHOR_EMAIL", "a@example.com")
            .env("GIT_COMMITTER_NAME", "A")
            .env("GIT_COMMITTER_EMAIL", "a@example.com")
            .env("GIT_AUTHOR_DATE", &date)
            .env("GIT_COMMITTER_DATE", &date)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap().trim().to_owned()
    }

    /// Write `file` and commit; the commit's full SHA.
    fn commit(&mut self, file: &str, text: &str) -> String {
        let path = self.dir.join(file);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
        self.git(&["add", "-A"]);
        self.git(&["commit", "-q", "-m", "change"]);
        self.commits += 1;
        self.git(&["rev-parse", "HEAD"])
    }

    fn snapshot(&self, value: &serde_json::Value) {
        std::fs::create_dir_all(self.dir.join(".archmap")).unwrap();
        std::fs::write(
            self.dir.join(".archmap/github.json"),
            serde_json::to_string_pretty(value).unwrap(),
        )
        .unwrap();
    }
}

impl Drop for Repo {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

const MISSING: &str = "1111111111111111111111111111111111111111";

/// A shop with a merged pull request #15 that closes issue #12, and links
/// of every type around them. The pull request lists two commits of HEAD,
/// one of an unmerged branch, and one the repository never had.
fn shop(name: &str) -> (Repo, Vec<String>) {
    let mut repo = Repo::new(name);
    let mut c = Vec::new();
    repo.write_package();
    c.push(repo.commit("src/price.ts", "export const price = 1;\n"));
    c.push(repo.commit("src/price.ts", "export const price = 2;\n"));
    repo.git(&["checkout", "-q", "-b", "side"]);
    c.push(repo.commit("src/side.ts", "export const side = 1;\n"));
    repo.git(&["checkout", "-q", "main"]);
    c.push(repo.commit("src/price.ts", "export const price = 3;\n"));
    let item = |kind: &str, number: u64, state: &str| {
        serde_json::json!({
            "kind": kind, "number": number, "id": format!("X_{number}"),
            "title": format!("Item {number}"), "state": state,
            "created_at": "2026-01-01T00:00:00Z", "updated_at": "2026-01-04T00:00:00Z",
        })
    };
    let mut pr = item("pull_request", 15, "merged");
    pr["merged_at"] = "2026-01-04T00:00:00Z".into();
    pr["closed_at"] = "2026-01-04T00:00:00Z".into();
    pr["merge_commit"] = c[3].clone().into();
    pr["commits"] = serde_json::json!([c[0], c[1], c[2], MISSING]);
    pr["commit_count"] = 4.into();
    let mut issue = item("issue", 12, "open");
    issue["title"] = "Refunds round down".into();
    let link = |kind: &str, from: serde_json::Value, to: serde_json::Value, observed: &str| serde_json::json!({"type": kind, "from": from, "to": to, "observed": [observed]});
    let mut closed = link(
        "closed_by",
        serde_json::json!({"item": 12}),
        serde_json::json!({"item": 15}),
        "ClosedEvent.closer",
    );
    closed["at"] = "2026-01-04T00:00:00Z".into();
    let mut by_commit = link(
        "referenced",
        serde_json::json!({"commit": c[1]}),
        serde_json::json!({"item": 12}),
        "ReferencedEvent",
    );
    by_commit["at"] = "2026-01-02T00:00:00Z".into();
    repo.snapshot(&serde_json::json!({
        "schema": 1, "source": "github", "host": "github.com", "repository": "acme/shop",
        "fetched_at": "2026-10-03T09:00:00Z",
        "range": {"updated_since": "2025-10-03T09:00:00Z", "since_rule": "given",
                  "bound": 5000, "issues": 2, "pull_requests": 1},
        "relation_types": ["sub_issue", "blocked_by", "closes", "linked", "closed_by",
                           "cross_referenced", "referenced", "duplicate_of"],
        "items": [issue, item("issue", 9, "open"), pr],
        "relations": [
            link("closes", serde_json::json!({"item": 15}), serde_json::json!({"item": 12}), "PullRequest.closingIssuesReferences"),
            link("closes", serde_json::json!({"item": 15}), serde_json::json!({"item": 12}), "Issue.closedByPullRequestsReferences"),
            closed,
            by_commit,
            link("blocked_by", serde_json::json!({"item": 12}), serde_json::json!({"item": 9}), "Issue.blockedBy"),
            link("sub_issue", serde_json::json!({"item": 40}), serde_json::json!({"item": 12}), "Issue.parent"),
            link("cross_referenced", serde_json::json!({"repository": "acme/web", "number": 4}), serde_json::json!({"item": 12}), "CrossReferencedEvent"),
        ],
        "not_visible": {"links": {"cross_referenced": 1}, "events": 2},
    }));
    (repo, c)
}

impl Repo {
    fn write_package(&self) {
        std::fs::write(self.dir.join("package.json"), "{ \"name\": \"shop\" }\n").unwrap();
    }
}

fn query(ws: &Workspace, target: &str, format: Format) -> String {
    ws.query(&QueryRequest {
        target,
        depth: DEFAULT_DEPTH,
        format,
        verbose: false,
    })
    .unwrap()
    .output
}

fn scan(root: &Path) -> Workspace {
    Workspace::scan(root, ScanMode::Full).unwrap()
}

fn short(sha: &str) -> &str {
    &sha[..7]
}

#[test]
fn a_pull_request_lists_its_commits_against_the_local_history() {
    let (repo, c) = shop("pull");
    let text = query(&scan(&repo.dir), "#15", Format::Text);
    let expected = format!(
        "#15 pull request: Item 15\n  merged 2026-01-04\n  snapshot: github acme/shop, 2 \
         issues and 1 pull request updated since 2025-10-03, fetched 2026-10-03 09:00 UTC, as visible \
         to the account that fetched\n\
         \nCommits: 4, 2 in the local history\n  \
         {} matched by sha (2026-01-01)\n  \
         {} matched by sha (2026-01-02)\n  \
         {} in the repository, not in HEAD's history\n  \
         1111111 no local commit with the same sha\n\
         Merge commit: {} matched by sha (2026-01-04)\n\
         \nCloses: #12 issue, open\n\
         Closed: #12 issue, open, 2026-01-04\n",
        short(&c[0]),
        short(&c[1]),
        short(&c[2]),
        short(&c[3]),
    );
    assert!(text.starts_with(&expected), "{text}");
    // a pull request closes: the closings of issues outside the range
    assert!(
        text.contains(
            "  closings: seen on the closed issue's timeline, so issues outside the range that \
             #15 closed are not seen\n"
        ),
        "{text}"
    );
}

#[test]
fn an_issue_lists_each_link_type_apart_from_its_end() {
    let (repo, c) = shop("issue");
    let text = query(&scan(&repo.dir), "'#12'", Format::Text);
    for line in [
        "#12 issue: Refunds round down\n  open\n",
        "\nSub-issue of: #40 (outside the fetched range)\n",
        "Blocked by: #9 issue, open\n",
        "Closing references from: #15 pull request, merged\n",
        // a past closing beside the state now
        "Closed by (open now): #15 pull request, merged, 2026-01-04\n",
        "Cross-referenced by: acme/web#4 (another repository)\n",
        "  duplicates: seen on the duplicate, so duplicates of #12 outside the range are not \
         seen\n",
        "  outside the range: 1 of these links name items the snapshot does not hold\n",
        "  not visible: 1 cross_referenced end, 2 timeline events the fetching account could \
         not see\n",
    ] {
        assert!(text.contains(line), "{line}\n{text}");
    }
    assert!(
        text.contains(&format!(
            "Referenced by commits: {} matched by sha (2026-01-02)\n",
            short(&c[1])
        )),
        "{text}"
    );
    // an issue closes nothing: no note on closings
    assert!(!text.contains("closings:"), "{text}");
}

#[test]
fn json_gives_every_link_with_what_observed_it_and_every_match() {
    let (repo, c) = shop("json");
    let json = query(&scan(&repo.dir), "acme/shop#15", Format::Json);
    let value: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(value["item"]["number"], 15);
    let closes = &value["relations"][0];
    assert_eq!(closes["type"], "closes");
    assert_eq!(closes["end"], "from");
    assert_eq!(
        closes["observed"],
        serde_json::json!([
            "Issue.closedByPullRequestsReferences",
            "PullRequest.closingIssuesReferences"
        ])
    );
    let commits = value["commits"].as_array().unwrap();
    assert_eq!(commits[0]["sha"], c[0]);
    assert_eq!(commits[0]["matched_by"], "sha");
    assert_eq!(commits[2]["reason"], "not_in_head");
    assert_eq!(commits[3]["reason"], "no_local_commit_with_same_sha");
    assert_eq!(value["merge_commit"]["matched_by"], "sha");
    assert_eq!(value["coverage"]["range"]["since_rule"], "given");
}

#[test]
fn an_item_outside_the_snapshot_or_its_repository_says_so() {
    let (repo, _) = shop("outside");
    let ws = scan(&repo.dir);
    let text = query(&ws, "#40", Format::Text);
    assert!(
        text.starts_with("#40: outside the fetched range\n"),
        "{text}"
    );
    assert!(text.contains("\nSub-issues: #12 issue, open\n"), "{text}");
    assert_eq!(
        query(&ws, "acme/web#4", Format::Text),
        "acme/web#4: not in this snapshot (it holds acme/shop)\n"
    );
}

#[test]
fn without_a_snapshot_the_answer_says_where_it_looked() {
    let mut repo = Repo::new("none");
    repo.write_package();
    repo.commit("src/a.ts", "export const a = 1;\n");
    let ws = scan(&repo.dir);
    assert_eq!(
        query(&ws, "#3", Format::Text),
        "#3: no work snapshot\n  looked at .archmap/github.json (`archmap fetch github` \
         writes one)\n"
    );
    // nor does summary say anything of work
    assert!(!ws.summary(DEFAULT_DEPTH, false).contains("\nwork:"));
}

#[test]
fn summary_names_the_snapshot_and_another_path_can_be_read() {
    let (repo, _) = shop("summary");
    let summary = scan(&repo.dir).summary(DEFAULT_DEPTH, false);
    assert!(
        summary.contains(
            "\nwork: github acme/shop, 2 issues and 1 pull request updated since 2025-10-03, \
             fetched 2026-10-03 09:00 UTC, as visible to the account that fetched\n"
        ),
        "{summary}"
    );
    // a snapshot kept elsewhere
    let elsewhere = repo.dir.join("elsewhere.json");
    std::fs::rename(repo.dir.join(".archmap/github.json"), &elsewhere).unwrap();
    let ws = scan(&repo.dir).with_snapshot(&elsewhere);
    assert!(query(&ws, "#15", Format::Text).starts_with("#15 pull request: Item 15\n"));
}

#[test]
fn a_fetch_takes_its_host_from_origin_only_on_github_com() {
    let repo = Repo::new("origin");
    repo.git(&[
        "remote",
        "add",
        "origin",
        "https://evil.example/acme/shop.git",
    ]);
    let request = archmap_app::FetchRequest {
        repo: None,
        since: None,
        all: false,
        max_items: 10,
        titles: true,
        output: None,
    };
    // refused before anything runs: no gh, nothing written
    let error = archmap_app::fetch_github(&repo.dir, &request)
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("is not a github.com repository: name it with --repo"),
        "{error}"
    );
    assert!(!repo.dir.join(".archmap").exists());
    let error = archmap_app::fetch_github(
        &repo.dir,
        &archmap_app::FetchRequest {
            repo: Some("acme"),
            ..request
        },
    )
    .unwrap_err()
    .to_string();
    assert!(
        error.contains("--repo takes OWNER/NAME or HOST/OWNER/NAME"),
        "{error}"
    );
    let error = archmap_app::fetch_github(
        &repo.dir,
        &archmap_app::FetchRequest {
            repo: Some("acme/shop"),
            since: Some("last week"),
            ..request
        },
    )
    .unwrap_err()
    .to_string();
    assert!(error.contains("--since takes a date"), "{error}");
}
