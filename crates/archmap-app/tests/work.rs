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

fn impact(ws: &Workspace, target: &str, format: Format) -> String {
    ws.impact(&archmap_app::ImpactRequest {
        target,
        depth: DEFAULT_DEPTH,
        format,
        verbose: false,
    })
    .unwrap()
    .output
}

#[test]
fn a_file_lists_the_pull_requests_and_issues_linked_to_its_commits() {
    let (repo, c) = shop("work-file");
    let ws = scan(&repo.dir);
    let expected = format!(
        "\nWork: 3 of the 3 commits that changed src/price.ts are linked to 1 pull request and \
         1 issue\n  \
         pull requests: 1\n  \
         #15 pull request, merged 2026-01-04: Item 15\n    \
         by commit list: {} 2026-01-02, {} 2026-01-01; by merge commit: {} 2026-01-04\n    \
         closes, closed: #12 issue, open: Refunds round down\n  \
         linked from commits: 1\n  \
         #12 issue, open: Refunds round down\n    \
         referenced in commit {} 2026-01-02\n  \
         work: github acme/shop, 2 issues and 1 pull request updated since 2025-10-03, fetched \
         2026-10-03 09:00 UTC, as visible to the account that fetched; states as of the fetch\n",
        short(&c[1]),
        short(&c[0]),
        short(&c[3]),
        short(&c[1]),
    );
    // in query on the file, and in impact after the files changed with it
    let text = query(&ws, "src/price.ts", Format::Text);
    assert!(text.contains(&expected), "{text}");
    let text = impact(&ws, "src/price.ts", Format::Text);
    assert!(text.contains(&expected), "{text}");
    let co_change = text.find("\nChanged in the same commits").unwrap();
    assert!(co_change < text.find("\nWork:").unwrap(), "{text}");
}

#[test]
fn a_title_that_holds_a_marks_words_adds_no_mark() {
    let (repo, _) = shop("work-title");
    let path = repo.dir.join(".archmap/github.json");
    let snapshot = std::fs::read_to_string(&path)
        .unwrap()
        .replace("Item 15", "Fix flaky checkout (test) on CI")
        .replace("Refunds round down", "Retry (local) cache");
    std::fs::write(&path, snapshot).unwrap();
    let ws = scan(&repo.dir);
    for text in [
        query(&ws, "src/price.ts", Format::Text),
        impact(&ws, "src/price.ts", Format::Text),
    ] {
        assert!(
            text.contains(": Fix flaky checkout (test) on CI\n"),
            "{text}"
        );
        assert!(text.contains(": Retry (local) cache\n"), "{text}");
        assert!(!text.contains("(test) in test code"), "{text}");
        assert!(!text.contains("(local) inside"), "{text}");
    }
}

#[test]
fn json_gives_each_step_of_the_work_with_its_link() {
    let (repo, c) = shop("work-json");
    let json = query(&scan(&repo.dir), "src/price.ts", Format::Json);
    let value: serde_json::Value = serde_json::from_str(&json).unwrap();
    let work = &value["work"];
    assert_eq!(work["state"], "read");
    assert_eq!(work["snapshot"], ".archmap/github.json");
    assert_eq!(
        (work["commits"].clone(), work["linked"].clone()),
        (3.into(), 3.into())
    );
    let pull = &work["pull_requests"][0];
    assert_eq!(pull["number"], 15);
    let ids = |list: &serde_json::Value| -> Vec<String> {
        list.as_array()
            .unwrap()
            .iter()
            .map(|c| c["id"].as_str().unwrap().to_owned())
            .collect()
    };
    assert_eq!(ids(&pull["by_commit_list"]), [c[1].clone(), c[0].clone()]);
    assert_eq!(ids(&pull["by_merge_commit"]), [c[3].clone()]);
    assert!(pull.get("merged_as").is_none(), "{pull}");
    // the issue once, with both links
    assert_eq!(pull["items"].as_array().unwrap().len(), 1);
    assert_eq!(
        pull["items"][0]["links"],
        serde_json::json!([
            {"type": "closes", "end": "from"},
            {"type": "closed_by", "end": "to"}
        ])
    );
    assert_eq!(pull["items"][0]["other"], serde_json::json!({"item": 12}));
    assert_eq!(pull["items"][0]["state"], "open");
    let from = &work["from_commits"][0];
    assert_eq!(from["other"], serde_json::json!({"item": 12}));
    assert_eq!(from["by"][0]["type"], "referenced");
    assert_eq!(from["by"][0]["commit"]["id"], c[1].as_str());
}

#[test]
fn a_true_merge_is_the_pull_requests_own_fact_apart_from_the_commits_counted() {
    let mut repo = Repo::new("work-merge");
    repo.write_package();
    let base = repo.commit("src/price.ts", "export const price = 1;\n");
    repo.git(&["checkout", "-q", "-b", "feature"]);
    let change = repo.commit("src/price.ts", "export const price = 2;\n");
    repo.git(&["checkout", "-q", "main"]);
    repo.git(&["merge", "-q", "--no-ff", "-m", "merge", "feature"]);
    repo.commits += 1;
    let merge = repo.git(&["rev-parse", "HEAD"]);
    let alone = repo.commit("src/price.ts", "export const price = 3;\n");
    repo.snapshot(&serde_json::json!({
        "schema": 1, "source": "github", "host": "github.com", "repository": "acme/shop",
        "fetched_at": "2026-10-03T09:00:00Z",
        // the range starts after the first commit
        "range": {"updated_since": "2026-01-01T12:00:00Z", "since_rule": "given",
                  "bound": 5000, "issues": 0, "pull_requests": 1},
        "relation_types": ["closes", "linked", "closed_by", "cross_referenced", "referenced"],
        "items": [{
            "kind": "pull_request", "number": 7, "id": "P7", "title": "Raise price",
            "state": "merged", "created_at": "2026-01-02T00:00:00Z",
            "updated_at": "2026-01-03T00:00:00Z", "merged_at": "2026-01-03T00:00:00Z",
            "closed_at": "2026-01-03T00:00:00Z", "merge_commit": merge, "commits": [change],
        }, {
            "kind": "issue", "number": 30, "id": "I30", "title": "Prices", "state": "open",
            "created_at": "2026-01-02T00:00:00Z", "updated_at": "2026-01-03T00:00:00Z",
        }, {
            "kind": "issue", "number": 31, "id": "I31", "title": "Rates", "state": "closed",
            "created_at": "2026-01-02T00:00:00Z", "updated_at": "2026-01-03T00:00:00Z",
        }],
        // from the item that references to the one it references: #7
        // references #30, and #31 references #7
        "relations": [
            {"type": "cross_referenced", "from": {"item": 7}, "to": {"item": 30},
             "observed": ["CrossReferencedEvent"]},
            {"type": "cross_referenced", "from": {"item": 31}, "to": {"item": 7},
             "observed": ["CrossReferencedEvent"]},
        ],
    }));
    let ws = scan(&repo.dir);
    let text = query(&ws, "src/price.ts", Format::Text);
    for line in [
        "\nWork: 1 of the 3 commits that changed src/price.ts is linked to 1 pull request and \
         2 issues\n"
            .to_owned(),
        // a closed item without a date in the snapshot says no date
        "    cross-references #30 issue, open: Prices; cross-referenced by #31 issue, closed: \
         Rates\n"
            .to_owned(),
        format!(
            "    by commit list: {} 2026-01-02; merged as {}, a merge commit, not among the \
             commits counted\n",
            short(&change),
            short(&merge)
        ),
        // newest first; the first commit is older than the range
        format!(
            "  2 commits are linked to no pull request or item in the snapshot by SHA (after a \
             squash or rebase merge, a pull request's own commits have other SHAs): {} \
             2026-01-04, {} 2026-01-01; 1 is older than the snapshot's range, whose pull \
             requests it may not hold\n",
            short(&alone),
            short(&base)
        ),
    ] {
        assert!(text.contains(&line), "{line}\n{text}");
    }
    // the pull request's code: the merge changes nothing of its own
    let text = query(&ws, "#7", Format::Text);
    let expected = format!(
        "\nCode: 1 file in 1 component\n  \
         by commit list: src/price.ts\n  \
         merged as {}, a merge commit, whose own changes are not read\n  \
         components: price.ts 1 file\n",
        short(&merge)
    );
    assert!(text.contains(&expected), "{text}");
    let json = query(&ws, "src/price.ts", Format::Json);
    let value: serde_json::Value = serde_json::from_str(&json).unwrap();
    let items = &value["work"]["pull_requests"][0]["items"];
    assert_eq!(
        items,
        &serde_json::json!([
            {"other": {"item": 30}, "kind": "issue", "state": "open", "title": "Prices",
             "links": [{"type": "cross_referenced", "end": "from"}]},
            {"other": {"item": 31}, "kind": "issue", "state": "closed", "title": "Rates",
             "links": [{"type": "cross_referenced", "end": "to"}]},
        ])
    );
}

#[test]
fn without_a_snapshot_a_file_says_where_the_work_would_come_from() {
    let mut repo = Repo::new("work-none");
    repo.write_package();
    repo.commit("src/price.ts", "export const price = 1;\n");
    let text = query(&scan(&repo.dir), "src/price.ts", Format::Text);
    assert!(
        text.contains("\nWork: none (no snapshot at .archmap/github.json)\n"),
        "{text}"
    );
}

#[test]
fn a_pull_request_lists_the_files_of_its_commits_by_kind_apart() {
    let (repo, _) = shop("code-pull");
    let text = query(&scan(&repo.dir), "#15", Format::Text);
    // the commit on a side branch and the one never fetched give no file
    assert!(
        text.contains(
            "\nCode: 2 files in 2 components\n  \
             by commit list: package.json, src/price.ts\n  \
             by merge commit: src/price.ts\n  \
             components: price.ts 1 file, shop 1 file\n\n"
        ),
        "{text}"
    );
}

#[test]
fn an_issue_lists_the_files_through_each_pull_request_and_commit_it_links() {
    let (repo, c) = shop("code-issue");
    let ws = scan(&repo.dir);
    let text = query(&ws, "#12", Format::Text);
    let expected = format!(
        "\nCode: 2 files in 2 components, through 1 pull request and 1 commit\n  \
         through #15 pull request (closes, closed_by), 2 of its 4 commits in the local \
         history, and its merge commit:\n    \
         package.json, src/price.ts\n  \
         through commit {} 2026-01-02 (referenced):\n    \
         src/price.ts\n  \
         components: price.ts 1 file, shop 1 file\n\n",
        short(&c[1])
    );
    assert!(text.contains(&expected), "{text}");
    // an issue no work links to code says by which links it looked
    let text = query(&ws, "#9", Format::Text);
    assert!(
        text.contains(
            "\nCode: no file in the local history\n  (no pull request or commit is linked to \
             it by closes, linked, closed_by, cross_referenced or referenced)\n"
        ),
        "{text}"
    );

    let json = query(&ws, "#12", Format::Json);
    let value: serde_json::Value = serde_json::from_str(&json).unwrap();
    let code = &value["code"];
    assert_eq!(code["files"], 2);
    assert_eq!(
        code["components"],
        serde_json::json!([
            {"id": "shop::src/price.ts", "name": "price.ts", "files": 1},
            {"id": "shop", "name": "shop", "files": 1},
        ])
    );
    let pull = &code["through"][0];
    assert_eq!(
        (
            pull["kind"].clone(),
            pull["number"].clone(),
            pull["links"].clone()
        ),
        (
            "pull_request".into(),
            15.into(),
            serde_json::json!(["closes", "closed_by"])
        )
    );
    assert_eq!(pull["by_commit_list"][1]["id"], c[0].as_str());
    assert_eq!(
        pull["by_commit_list"][1]["files"],
        serde_json::json!([
            {"path": "package.json", "in_head": true},
            {"path": "src/price.ts", "in_head": true},
        ])
    );
    assert_eq!(pull["by_merge_commit"]["id"], c[3].as_str());
    let commit = &code["through"][1];
    assert_eq!(
        (
            commit["kind"].clone(),
            commit["id"].clone(),
            commit["links"].clone()
        ),
        (
            "commit".into(),
            c[1].as_str().into(),
            serde_json::json!(["referenced"])
        )
    );
    assert_eq!(
        value["unmatched"],
        serde_json::json!({"merged": 1, "unmatched": 0})
    );
}

impl Repo {
    /// Run `args` (a `git mv` or `git rm`) and commit; the commit's SHA.
    fn commit_git(&mut self, args: &[&str]) -> String {
        self.git(args);
        self.git(&["commit", "-q", "-m", "change"]);
        self.commits += 1;
        self.git(&["rev-parse", "HEAD"])
    }
}

#[test]
fn files_are_named_as_head_holds_them_and_unmatched_work_is_counted() {
    let mut repo = Repo::new("code-moves");
    repo.write_package();
    let squashed = repo.commit("src/old.ts", "export const price = 1;\n");
    let removed = repo.commit("src/gone.ts", "export const gone = 1;\n");
    repo.commit_git(&["mv", "src/old.ts", "src/new.ts"]);
    repo.commit_git(&["rm", "-q", "src/gone.ts"]);
    let item = |kind: &str, number: u64, state: &str| {
        serde_json::json!({
            "kind": kind, "number": number, "id": format!("X_{number}"),
            "title": format!("Item {number}"), "state": state,
            "created_at": "2026-01-01T00:00:00Z", "updated_at": "2026-01-04T00:00:00Z",
        })
    };
    // #20 was squashed into a commit of HEAD, #22 into one the clone lacks
    let mut squash = item("pull_request", 20, "merged");
    squash["merge_commit"] = squashed.clone().into();
    squash["commits"] = serde_json::json!([MISSING]);
    let mut lost = item("pull_request", 22, "merged");
    lost["merge_commit"] = "2222222222222222222222222222222222222222".into();
    lost["commits"] = serde_json::json!([MISSING]);
    repo.snapshot(&serde_json::json!({
        "schema": 1, "source": "github", "host": "github.com", "repository": "acme/shop",
        "fetched_at": "2026-10-03T09:00:00Z",
        "range": {"since_rule": "all", "bound": 5000, "issues": 1, "pull_requests": 2},
        "relation_types": ["closes", "linked", "closed_by", "cross_referenced", "referenced"],
        "items": [squash, item("issue", 21, "closed"), lost],
        "relations": [
            {"type": "closed_by", "from": {"item": 21}, "to": {"commit": removed},
             "observed": ["ClosedEvent.closer"]},
            {"type": "referenced", "from": {"commit": MISSING}, "to": {"item": 21},
             "observed": ["ReferencedEvent"]},
            // #20 only mentions #21
            {"type": "cross_referenced", "from": {"item": 20}, "to": {"item": 21},
             "observed": ["CrossReferencedEvent"]},
        ],
    }));
    let ws = scan(&repo.dir);
    let text = query(&ws, "#20", Format::Text);
    assert!(
        text.contains(
            "\nCode: 2 files in 2 components\n  \
             by merge commit: package.json, src/old.ts now src/new.ts\n  \
             components: new.ts 1 file, shop 1 file\n"
        ),
        "{text}"
    );
    let unmatched = "\n  unmatched: 1 of the snapshot's 2 merged pull requests matches no \
                     commit of the history read, so their code is not shown\n";
    assert!(text.contains(unmatched), "{text}");
    let text = query(&ws, "#21", Format::Text);
    // the closing commit first; the files a mention alone reaches counted
    let expected = format!(
        "\nCode: 3 files in 2 components, through 1 pull request and 2 commits; 2 of them \
         only through cross_referenced or referenced links\n  \
         through commit {} 2026-01-02 (closed_by):\n    \
         src/gone.ts not in HEAD\n  \
         through #20 pull request (cross_referenced), 0 of its 1 commit in the local \
         history, and its merge commit:\n    \
         package.json, src/old.ts now src/new.ts\n  \
         through commit {} (referenced), not in the local history\n  \
         components: shop 2 files, new.ts 1 file\n",
        short(&removed),
        short(MISSING)
    );
    assert!(text.contains(&expected), "{text}");
    let text = query(&ws, "#22", Format::Text);
    assert!(
        text.contains(
            "\nCode: no file in the local history\n  no commit of #22 is in the local \
             history\n"
        ),
        "{text}"
    );
    let summary = ws.summary(DEFAULT_DEPTH, false);
    assert!(
        summary
            .contains("; 1 of its 2 merged pull requests matches no commit of the history read\n"),
        "{summary}"
    );
}
