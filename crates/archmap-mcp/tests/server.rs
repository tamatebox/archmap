//! The MCP server seen from a client: the four tools, their answers from the
//! shared layer, roots, and when the graph it keeps is scanned again.

use std::path::{Path, PathBuf};

use archmap_app::{Format, ImpactRequest, QueryRequest, ScanMode, Workspace, DEFAULT_DEPTH};
use archmap_mcp::Server;
use rmcp::model::{CallToolRequestParams, CallToolResult};
use rmcp::service::RunningService;
use rmcp::{RoleClient, ServiceExt};

fn fixture(name: &str) -> PathBuf {
    std::fs::canonicalize(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures")
            .join(name),
    )
    .unwrap()
}

/// A throwaway repository, removed when the guard drops.
struct Repo(PathBuf);

impl Repo {
    fn new(name: &str) -> Repo {
        let dir = std::env::temp_dir().join(format!("archmap-mcp-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Repo(std::fs::canonicalize(dir).unwrap())
    }

    fn write(&self, file: &str, text: &str) -> &Repo {
        let path = self.0.join(file);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
        self
    }

    /// A Python package `pkg` with one public function.
    fn python(name: &str) -> Repo {
        let repo = Repo::new(name);
        repo.write("pkg/__init__.py", "def run():\n    pass\n");
        repo
    }
}

impl Drop for Repo {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

type Client = RunningService<RoleClient, ()>;

/// A client connected in process to a server for `root`.
async fn connect(server: Server) -> Client {
    let (server_io, client_io) = tokio::io::duplex(1 << 20);
    tokio::spawn(async move {
        if let Ok(running) = server.serve(server_io).await {
            let _ = running.waiting().await;
        }
    });
    ().serve(client_io).await.unwrap()
}

async fn call(client: &Client, tool: &'static str, args: serde_json::Value) -> CallToolResult {
    client
        .call_tool(
            CallToolRequestParams::new(tool).with_arguments(args.as_object().unwrap().clone()),
        )
        .await
        .unwrap()
}

/// The answer's first text block.
fn text(result: &CallToolResult) -> String {
    result
        .content
        .first()
        .and_then(|c| c.as_text())
        .map(|t| t.text.clone())
        .unwrap_or_default()
}

fn ok(result: &CallToolResult) -> String {
    assert_eq!(result.is_error, Some(false), "{}", text(result));
    text(result)
}

fn failed(result: &CallToolResult) -> String {
    assert_eq!(result.is_error, Some(true), "{}", text(result));
    text(result)
}

#[tokio::test]
async fn the_server_offers_four_read_only_tools_and_says_what_it_is_for() {
    let client = connect(Server::new(fixture("simple-python-project"))).await;
    let info = client.peer_info().unwrap();
    assert_eq!(info.server_info.as_ref().unwrap().name, "archmap");
    let instructions = info.instructions.clone().unwrap();
    assert!(
        instructions.starts_with("archmap maps a repository"),
        "{instructions}"
    );
    assert!(instructions.contains("runtime coupling"), "{instructions}");

    let tools = client.list_all_tools().await.unwrap();
    let names: Vec<&str> = tools.iter().map(|t| t.name.as_ref()).collect();
    assert_eq!(names, ["check", "impact", "query", "summary"]);
    for tool in &tools {
        let annotations = tool.annotations.as_ref().unwrap();
        assert_eq!(annotations.read_only_hint, Some(true), "{}", tool.name);
        let required = tool.input_schema.get("required").cloned();
        let wants_target = matches!(tool.name.as_ref(), "query" | "impact");
        assert_eq!(
            required == Some(serde_json::json!(["target"])),
            wants_target,
            "{}: {required:?}",
            tool.name
        );
        let description = tool.description.as_deref().unwrap();
        assert!(!description.contains("callers"), "{}", tool.name);
    }
    // what a client loads in every session, before any answer
    let size = instructions.len() + serde_json::to_string(&tools).unwrap().len();
    assert!(
        size <= MAX_SESSION_TEXT,
        "{size} bytes over {MAX_SESSION_TEXT}: every session loads these texts; say details in \
         the answers (Marks line, headings, Not traced) or docs/reference/, not here"
    );
    client.cancel().await.unwrap();
}

/// The bytes of instructions and tool listings a client receives (6,815 when
/// set). Raising it is the user's decision, as CLAUDE.md says.
const MAX_SESSION_TEXT: usize = 7_500;

#[tokio::test]
async fn query_answers_with_the_shared_layers_text() {
    let root = fixture("simple-python-project");
    let client = connect(Server::new(root.clone())).await;
    let answer = ok(&call(
        &client,
        "query",
        serde_json::json!({"target": "shop.users"}),
    )
    .await);
    let ws = Workspace::scan(&root, ScanMode::Full).unwrap();
    let expected = ws
        .query(&QueryRequest {
            target: "shop.users",
            depth: DEFAULT_DEPTH,
            format: Format::Text,
            verbose: false,
        })
        .unwrap()
        .output;
    assert_eq!(answer, expected);
    let json = ok(&call(
        &client,
        "query",
        serde_json::json!({"target": "shop", "format": "json", "depth": 1}),
    )
    .await);
    let value: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(value["depth"], 1);
    client.cancel().await.unwrap();
}

#[tokio::test]
async fn query_answers_where_a_symbol_is_used_as_the_app_does() {
    let root = fixture("ts-uses");
    let client = connect(Server::new(root.clone())).await;
    let ws = Workspace::scan(&root, ScanMode::Full).unwrap();
    for (format, name) in [(Format::Text, "text"), (Format::Json, "json")] {
        let expected = ws
            .query(&QueryRequest {
                target: "formatPrice",
                depth: DEFAULT_DEPTH,
                format,
                verbose: false,
            })
            .unwrap()
            .output;
        let args = serde_json::json!({"target": "formatPrice", "format": name});
        let answer = ok(&call(&client, "query", args).await);
        assert_eq!(answer, expected, "{name}");
    }
    let text = ok(&call(
        &client,
        "query",
        serde_json::json!({"target": "formatPrice"}),
    )
    .await);
    assert!(
        text.contains("\nUsed at: 24 in 16 files, 5 in tests, showing 10 (20 calls, 4 types)\n"),
        "{text}"
    );
    client.cancel().await.unwrap();
}

#[tokio::test]
async fn impact_answers_with_the_shared_layers_text_or_json() {
    let root = fixture("simple-ts-project");
    let client = connect(Server::new(root.clone())).await;
    let ws = Workspace::scan(&root, ScanMode::Full).unwrap();
    let expected = |format, verbose| {
        ws.impact(&ImpactRequest {
            target: "src/lib/money.ts",
            depth: DEFAULT_DEPTH,
            format,
            verbose,
        })
        .unwrap()
        .output
    };
    for (args, format, verbose) in [
        (serde_json::json!({}), Format::Text, false),
        (serde_json::json!({"verbose": true}), Format::Text, true),
        (serde_json::json!({"format": "json"}), Format::Json, false),
        (
            serde_json::json!({"format": "json", "verbose": true}),
            Format::Json,
            true,
        ),
    ] {
        let mut args = args;
        args["target"] = "src/lib/money.ts".into();
        let answer = ok(&call(&client, "impact", args.clone()).await);
        assert_eq!(answer, expected(format, verbose), "{args}");
    }
    client.cancel().await.unwrap();
}

/// Commit everything in `dir`, with fixed identities and dates and the
/// user's git config kept apart.
fn commit(dir: &Path, day: u32) {
    let date = format!("2026-01-{day:02}T00:00:00Z");
    let git = |args: &[&str]| {
        let out = std::process::Command::new("git")
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
            .env("GIT_AUTHOR_DATE", &date)
            .env("GIT_COMMITTER_DATE", &date)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    };
    if !dir.join(".git").exists() {
        git(&["init", "-q", "-b", "main"]);
    }
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "change"]);
}

#[tokio::test]
async fn impact_lists_the_files_changed_in_the_same_commits_as_the_app_does() {
    let repo = Repo::python("co-change");
    repo.write("pkg/rates.toml", "rate = 1\n");
    commit(&repo.0, 1);
    repo.write("pkg/__init__.py", "def run():\n    return 1\n")
        .write("pkg/rates.toml", "rate = 2\n");
    commit(&repo.0, 2);
    let client = connect(Server::new(repo.0.clone())).await;
    let ws = Workspace::scan(&repo.0, ScanMode::Full).unwrap();
    for format in [Format::Text, Format::Json] {
        let expected = ws
            .impact(&ImpactRequest {
                target: "pkg/__init__.py",
                depth: DEFAULT_DEPTH,
                format,
                verbose: false,
            })
            .unwrap()
            .output;
        let mut args = serde_json::json!({"target": "pkg/__init__.py"});
        if format == Format::Json {
            args["format"] = "json".into();
        }
        assert_eq!(ok(&call(&client, "impact", args).await), expected);
    }
    let text = ok(&call(
        &client,
        "impact",
        serde_json::json!({"target": "pkg/__init__.py"}),
    )
    .await);
    assert!(
        text.contains("\nChanged in the same commits (history, not imports): 1 file, in the 2 commits that changed pkg/__init__.py; per file: commits shared, of the target's and of its own\n  pkg/rates.toml  2 of the target's 2, 2 of its own 2: "),
        "{text}"
    );
    client.cancel().await.unwrap();
}

#[tokio::test]
async fn query_answers_an_item_of_the_work_snapshot_as_the_app_does() {
    let repo = Repo::python("work");
    commit(&repo.0, 1);
    repo.write(
        ".archmap/github.json",
        r#"{"schema": 1, "source": "github", "host": "github.com", "repository": "acme/shop",
            "fetched_at": "2026-10-03T09:00:00Z",
            "range": {"since_rule": "all", "bound": 5000, "issues": 1, "pull_requests": 0},
            "relation_types": ["closed_by"],
            "items": [{"kind": "issue", "number": 7, "id": "I_7", "title": "Run fails",
                       "state": "open", "created_at": "2026-01-01T00:00:00Z",
                       "updated_at": "2026-01-01T00:00:00Z"}],
            "relations": []}"#,
    );
    let client = connect(Server::new(repo.0.clone())).await;
    let ws = Workspace::scan(&repo.0, ScanMode::Full).unwrap();
    for format in [Format::Text, Format::Json] {
        let expected = ws
            .query(&QueryRequest {
                target: "#7",
                depth: DEFAULT_DEPTH,
                format,
                verbose: false,
            })
            .unwrap()
            .output;
        let mut args = serde_json::json!({"target": "#7"});
        if format == Format::Json {
            args["format"] = "json".into();
        }
        assert_eq!(ok(&call(&client, "query", args).await), expected);
    }
    let text = ok(&call(&client, "query", serde_json::json!({"target": "#7"})).await);
    assert!(text.starts_with("#7 issue: Run fails\n  open\n"), "{text}");
    client.cancel().await.unwrap();
}

#[tokio::test]
async fn every_tool_answers_on_a_fixture() {
    let client = connect(Server::new(fixture("simple-python-project"))).await;
    let summary = ok(&call(&client, "summary", serde_json::json!({})).await);
    assert!(summary.starts_with("# archmap summary\nroot: simple-python-project\n"));
    let impact = ok(&call(
        &client,
        "impact",
        serde_json::json!({"target": "src/shop/users.py"}),
    )
    .await);
    assert!(
        impact.starts_with("src/shop/users.py (file) in shop (module, python), depth 2\n"),
        "{impact}"
    );
    let check = ok(&call(&client, "check", serde_json::json!({})).await);
    assert!(check.starts_with("archmap check: no findings"), "{check}");
    client.cancel().await.unwrap();
}

#[tokio::test]
async fn a_target_that_names_several_things_gives_candidates_not_an_error() {
    let repo = Repo::new("candidates");
    for project in ["a", "b"] {
        repo.write(
            &format!("{project}/pyproject.toml"),
            &format!("[project]\nname = \"{project}\"\nversion = \"0.1.0\"\n"),
        );
        repo.write(
            &format!("{project}/tests/test_it.py"),
            "def test_it():\n    pass\n",
        );
    }
    let client = connect(Server::new(repo.0.clone())).await;
    let answer = ok(&call(&client, "query", serde_json::json!({"target": "tests"})).await);
    assert!(
        answer.starts_with("`tests` names 2 components;"),
        "{answer}"
    );
    let missing = failed(
        &call(
            &client,
            "query",
            serde_json::json!({"target": "nothing-here"}),
        )
        .await,
    );
    assert!(
        missing.starts_with(
            "no component, file, symbol or import named `nothing-here`, nor a name that contains it"
        ),
        "{missing}"
    );
    client.cancel().await.unwrap();
}

#[tokio::test]
async fn path_picks_another_root_relative_to_the_default_or_absolute() {
    let fixtures = fixture("simple-python-project")
        .parent()
        .unwrap()
        .to_path_buf();
    let client = connect(Server::new(fixtures.clone())).await;
    let relative = ok(&call(
        &client,
        "summary",
        serde_json::json!({"path": "simple-ts-project"}),
    )
    .await);
    assert!(
        relative.contains("\nroot: simple-ts-project\n"),
        "{relative}"
    );
    let absolute = ok(&call(
        &client,
        "summary",
        serde_json::json!({"path": fixtures.join("simple-ts-project")}),
    )
    .await);
    assert_eq!(absolute, relative);
    let missing = failed(
        &call(
            &client,
            "summary",
            serde_json::json!({"path": "no-such-dir"}),
        )
        .await,
    );
    assert!(
        missing.starts_with("`no-such-dir` is no directory"),
        "{missing}"
    );
    client.cancel().await.unwrap();
}

#[tokio::test]
async fn check_reads_a_rules_file_inside_the_root_only() {
    let repo = Repo::python("rules");
    repo.write(
        "archmap.toml",
        "[components]\npkg = [\"pkg\"]\n\n[[allow]]\nfrom = \"pkg\"\nto = []\n",
    );
    let outside = Repo::new("rules-outside");
    outside.write("rules.toml", "[components]\n");
    let client = connect(Server::new(repo.0.clone())).await;
    let default = ok(&call(&client, "check", serde_json::json!({})).await);
    assert!(
        default.contains("(rules: archmap.toml, roll-up depth 2)"),
        "{default}"
    );
    let named = ok(&call(
        &client,
        "check",
        serde_json::json!({"config": "archmap.toml"}),
    )
    .await);
    assert_eq!(named, default);
    let escaped = failed(
        &call(
            &client,
            "check",
            serde_json::json!({"config": outside.0.join("rules.toml")}),
        )
        .await,
    );
    assert!(escaped.contains("is outside the root"), "{escaped}");
    let missing = failed(&call(&client, "check", serde_json::json!({"config": "nope.toml"})).await);
    assert!(missing.contains("nope.toml"), "{missing}");
    client.cancel().await.unwrap();
}

#[tokio::test]
async fn the_graph_is_scanned_again_only_when_the_files_change() {
    let repo = Repo::python("rescan");
    let server = Server::new(repo.0.clone());
    let client = connect(server.clone()).await;
    let query = || call(&client, "query", serde_json::json!({"target": "pkg"}));
    assert!(ok(&query().await).contains("def run()"));
    assert_eq!(server.scans(), 1);
    ok(&query().await);
    assert_eq!(server.scans(), 1, "nothing changed");

    repo.write("pkg/more.py", "def more():\n    pass\n");
    assert!(ok(&query().await).contains("def more()"));
    assert_eq!(server.scans(), 2, "a file was added");

    std::fs::remove_file(repo.0.join("pkg/more.py")).unwrap();
    assert!(!ok(&query().await).contains("def more()"));
    assert_eq!(server.scans(), 3, "a file was deleted");
    client.cancel().await.unwrap();
}

#[tokio::test]
async fn a_failed_scan_is_tried_again_on_the_next_call() {
    let repo = Repo::python("retry");
    let server = Server::new(repo.0.clone());
    let client = connect(server.clone()).await;
    let summary = || call(&client, "summary", serde_json::json!({}));
    ok(&summary().await);
    std::fs::remove_dir_all(&repo.0).unwrap();
    failed(&summary().await);
    std::fs::create_dir_all(repo.0.join("pkg")).unwrap();
    // other bytes than before, so a coarse clock cannot hide the change
    repo.write("pkg/__init__.py", "def run():\n    return 1\n");
    ok(&summary().await);
    assert_eq!(server.scans(), 2);
    client.cancel().await.unwrap();
}

#[tokio::test]
async fn at_most_eight_roots_are_kept() {
    let repos: Vec<Repo> = (0..9)
        .map(|i| Repo::python(&format!("roots-{i}")))
        .collect();
    let server = Server::new(repos[0].0.clone());
    let client = connect(server.clone()).await;
    for repo in &repos {
        ok(&call(&client, "summary", serde_json::json!({"path": repo.0})).await);
    }
    assert_eq!(server.scans(), 9);
    // the last eight stay; the first, least recently used, went
    ok(&call(&client, "summary", serde_json::json!({"path": repos[8].0})).await);
    assert_eq!(server.scans(), 9);
    ok(&call(&client, "summary", serde_json::json!({"path": repos[0].0})).await);
    assert_eq!(server.scans(), 10);
    client.cancel().await.unwrap();
}

#[tokio::test]
async fn two_calls_at_once_on_one_root_scan_it_once() {
    let repo = Repo::python("concurrent");
    let server = Server::new(repo.0.clone());
    let client = connect(server.clone()).await;
    let (a, b) = tokio::join!(
        call(&client, "summary", serde_json::json!({})),
        call(&client, "query", serde_json::json!({"target": "pkg"}))
    );
    ok(&a);
    ok(&b);
    assert_eq!(server.scans(), 1);
    client.cancel().await.unwrap();
}

#[tokio::test]
async fn a_target_outside_the_root_fails_before_any_scan() {
    let root = fixture("simple-python-project");
    let outside = root.parent().unwrap().to_path_buf();
    let server = Server::new(root);
    let client = connect(server.clone()).await;
    for tool in ["query", "impact"] {
        let message = failed(&call(&client, tool, serde_json::json!({"target": outside})).await);
        assert!(
            message.contains("is outside the scanned root"),
            "{tool}: {message}"
        );
    }
    assert_eq!(server.scans(), 0);
    client.cancel().await.unwrap();
}

#[tokio::test]
async fn an_edited_file_is_read_again() {
    let repo = Repo::python("edited");
    let server = Server::new(repo.0.clone());
    let client = connect(server.clone()).await;
    let query = || call(&client, "query", serde_json::json!({"target": "pkg"}));
    assert!(ok(&query().await).contains("def run()"));
    repo.write("pkg/__init__.py", "def walk():\n    pass\n");
    let after = ok(&query().await);
    assert!(
        after.contains("def walk()") && !after.contains("def run()"),
        "{after}"
    );
    assert_eq!(server.scans(), 2);
    client.cancel().await.unwrap();
}
