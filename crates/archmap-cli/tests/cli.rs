//! Smoke tests for the `archmap` binary.

use std::path::{Path, PathBuf};
use std::process::Command;

fn fixture_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/simple-rust-workspace")
}

fn archmap() -> Command {
    Command::new(env!("CARGO_BIN_EXE_archmap"))
}

/// A throwaway directory containing one tiny Python package.
fn temp_repo(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("archmap-cli-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("pkg")).unwrap();
    std::fs::write(dir.join("pkg/__init__.py"), "def run():\n    pass\n").unwrap();
    dir
}

#[test]
fn scan_writes_graph_under_dot_archmap_by_default() {
    let repo = temp_repo("default");
    let out = archmap().arg("scan").arg(&repo).output().unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(out.stdout.is_empty(), "graph goes to the file, not stdout");
    assert!(String::from_utf8_lossy(&out.stderr).contains("wrote"));

    let graph: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(repo.join(".archmap/graph.json")).unwrap())
            .unwrap();
    assert_eq!(graph["schema_version"], 2);
    // only the graph is written; whether to ignore it is the repository's call
    let written: Vec<String> = std::fs::read_dir(repo.join(".archmap"))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(written, vec!["graph.json".to_owned()]);

    // a second scan does not pick up its own output
    let again = archmap().arg("scan").arg(&repo).output().unwrap();
    assert!(again.status.success());
    let graph2: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(repo.join(".archmap/graph.json")).unwrap())
            .unwrap();
    assert_eq!(graph, graph2);

    std::fs::remove_dir_all(&repo).unwrap();
}

#[test]
fn scan_writes_to_explicit_output_file() {
    let repo = temp_repo("explicit");
    let target = repo.join("out/nested/graph.json");
    let out = archmap()
        .arg("scan")
        .arg(&repo)
        .arg("-o")
        .arg(&target)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(target.is_file());
    assert!(!repo.join(".archmap").exists());
    std::fs::remove_dir_all(&repo).unwrap();
}

#[test]
fn scan_emits_json_graph_to_stdout_with_dash() {
    let out = archmap()
        .arg("scan")
        .arg(fixture_root())
        .args(["--format", "json", "--output", "-"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let graph: serde_json::Value = serde_json::from_slice(&out.stdout).expect("valid json");
    assert_eq!(graph["schema_version"], 2);
    assert_eq!(graph["components"]["app"]["kind"], "package");
    assert!(graph["symbols"]["lib_core::greet"].is_object());
    assert!(graph["edges"]
        .as_array()
        .unwrap()
        .iter()
        .any(|e| { e["from"] == "app" && e["to"] == "lib_core" && e["kind"] == "import" }));
}

#[test]
fn query_component_lists_symbols_and_edges() {
    let out = archmap()
        .args(["query", "lib_core", "--format", "json", "--path"])
        .arg(fixture_root())
        .output()
        .unwrap();
    assert!(out.status.success());
    let view: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(view["component"]["id"], "lib_core");
    assert!(!view["symbols"].as_array().unwrap().is_empty());
    assert_eq!(view["incoming"].as_array().unwrap().len(), 2); // import + dependency from app
}

#[test]
fn query_unknown_target_fails() {
    let out = archmap()
        .args(["query", "does-not-exist", "--path"])
        .arg(fixture_root())
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("does-not-exist"));
}

#[test]
fn impact_accepts_component_or_file() {
    for target in ["lib_core", "crates/lib_core/src/billing.rs"] {
        let out = archmap()
            .args(["impact", target, "--path"])
            .arg(fixture_root())
            .output()
            .unwrap();
        assert!(out.status.success(), "target {target}");
        let result: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
        assert_eq!(result["target"], "lib_core");
        assert_eq!(result["transitive"], serde_json::json!(["app"]));
    }
}

#[test]
fn check_without_a_rules_file_reports_signals_only() {
    let repo = temp_repo("no-rules");
    let out = archmap()
        .args(["check", "--path"])
        .arg(&repo)
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0));
    assert!(String::from_utf8_lossy(&out.stdout).contains("(rules: none, signals only,"));

    // a rules file that was asked for but is missing is an error
    let missing = archmap()
        .args(["check", "--config", "missing.toml", "--path"])
        .arg(&repo)
        .output()
        .unwrap();
    assert_eq!(missing.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&missing.stderr).contains("missing.toml"));
    std::fs::remove_dir_all(&repo).unwrap();
}

fn python_fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/simple-python-project")
}

fn summary_stdout(args: &[&str]) -> String {
    let out = archmap()
        .arg("summary")
        .arg(python_fixture())
        .args(args)
        .args(["-o", "-"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}

#[test]
fn summary_is_deterministic_markdown() {
    let first = summary_stdout(&[]);
    assert_eq!(first, summary_stdout(&[]), "same scan, same summary");

    assert!(first.starts_with("# archmap summary\nroot: simple-python-project\ndepth: 2\n"));
    for expected in [
        "components: 7 shown, 8 in the full graph\n",
        "## Components\n",
        "\n  shop  path: src/shop  symbols: 3\n",
        "\n    shop.billing  path: src/shop/billing  symbols: 4\n",
        "## Internal dependencies\n",
        "\nshop.billing -> shop  imports: 2\n",
        "## External dependencies\n",
        "\nrequests  declared: pyproject.toml, requirements.txt  importers: 1  top: shop.billing 1\n",
        "\npyyaml  declared: requirements.txt  importers: 1  top: shop.billing 1\n",
        "## Most depended on\n",
        "\nshop  dependents: 4  dependencies: 1  rank: 1/7\n",
    ] {
        assert!(first.contains(expected), "missing `{expected}` in:\n{first}");
    }
    // machine-specific paths never leak into the summary
    let root = python_fixture().canonicalize().unwrap();
    assert!(!first.contains(&root.display().to_string()));
}

#[test]
fn summary_depth_controls_the_roll_up() {
    let shallow = summary_stdout(&["--depth", "1"]);
    assert!(
        !shallow.contains("shop.billing  "),
        "billing is folded:\n{shallow}"
    );
    assert!(
        shallow.contains("\n  shop  path: src/shop  symbols: 9  folded: 3\n"),
        "{shallow}"
    );
    assert!(
        shallow.contains("\ntests -> shop  imports: 3\n"),
        "{shallow}"
    );

    let packages_only = summary_stdout(&["--depth", "0"]);
    assert!(
        packages_only
            .contains("\nshop  package  language: python  path: .  symbols: 9  folded: 7\n"),
        "{packages_only}"
    );
    assert!(packages_only.contains("## Internal dependencies\nnone\n"));
}

#[test]
fn summary_prints_by_default_and_saves_only_when_asked() {
    let repo = temp_repo("summary");
    let out = archmap().arg("summary").arg(&repo).output().unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(String::from_utf8_lossy(&out.stdout).starts_with("# archmap summary\n"));
    assert!(
        !repo.join(".archmap").exists(),
        "a summary is a view, not a file"
    );

    let file = repo.join("out/summary.md");
    let saved = archmap()
        .arg("summary")
        .arg(&repo)
        .arg("-o")
        .arg(&file)
        .output()
        .unwrap();
    assert!(saved.status.success() && saved.stdout.is_empty());
    assert!(std::fs::read_to_string(&file)
        .unwrap()
        .starts_with("# archmap summary\n"));
    std::fs::remove_dir_all(&repo).unwrap();
}

/// Run an archmap subcommand against the Python fixture and parse its JSON.
fn fixture_json(args: &[&str]) -> serde_json::Value {
    let out = archmap()
        .args(args)
        .arg("--path")
        .arg(python_fixture())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).unwrap()
}

#[test]
fn query_folds_deep_components_like_the_summary() {
    let view = fixture_json(&["query", "shop.integrations.slack", "--format", "json"]);
    assert_eq!(view["depth"], 2);
    assert_eq!(view["component"]["id"], "shop::shop.integrations");
    assert_eq!(view["folded_from"], "shop::shop.integrations.slack");
    assert_eq!(
        view["children"],
        serde_json::json!(["shop::shop.integrations.slack"])
    );
    let symbols: Vec<&str> = view["symbols"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["name"].as_str().unwrap())
        .collect();
    assert_eq!(symbols, vec!["notify", "send_webhook"]);

    let deeper = fixture_json(&[
        "query",
        "shop.integrations.slack",
        "--depth",
        "3",
        "--format",
        "json",
    ]);
    assert_eq!(deeper["component"]["id"], "shop::shop.integrations.slack");
    assert!(deeper.get("folded_from").is_none());
}

#[test]
fn impact_of_a_file_uses_the_summary_depth() {
    let result = fixture_json(&["impact", "src/shop/integrations/slack/__init__.py"]);
    assert_eq!(result["target"], "shop::shop.integrations");
    assert_eq!(result["folded_from"], "shop::shop.integrations.slack");
    assert_eq!(result["direct"], serde_json::json!(["shop::shop"]));
    // followed file by file: tests.unit imports only shop/users.py, which
    // does not use slack, so it is not reached
    assert_eq!(
        result["transitive"],
        serde_json::json!([
            "shop::scripts",
            "shop::shop",
            "shop::shop.billing",
            "shop::tests"
        ])
    );
}

#[test]
fn every_summary_component_is_visible_to_query_and_impact() {
    let summary = summary_stdout(&[]);
    let components = summary
        .split("## Components")
        .nth(1)
        .and_then(|rest| rest.split("\n## ").next())
        .unwrap();
    let names: std::collections::BTreeSet<&str> = components
        .lines()
        .filter_map(|l| l.trim_start().split("  ").next())
        .filter(|name| !name.is_empty() && *name != "none")
        .collect();
    assert!(names.contains("shop.billing") && names.contains("tests.unit"));

    for name in names {
        let view = fixture_json(&["query", name, "--format", "json"]);
        assert!(view.get("folded_from").is_none(), "`{name}` is folded");
        assert_eq!(view["component"]["name"], name);

        let impact = fixture_json(&["impact", name]);
        assert!(impact.get("folded_from").is_none());
        for id in impact["transitive"].as_array().unwrap() {
            let dependent = fixture_json(&["query", id.as_str().unwrap(), "--format", "json"]);
            assert!(
                dependent.get("folded_from").is_none(),
                "impact of `{name}` names `{id}`, which the summary does not show"
            );
        }
    }
}

/// Write `rules` to a temporary file and run `archmap check` on `root` with it.
fn check_with(name: &str, root: &Path, rules: &str, extra: &[&str]) -> std::process::Output {
    let dir = std::env::temp_dir().join(format!("archmap-rules-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("archmap.toml");
    std::fs::write(&file, rules).unwrap();
    let out = archmap()
        .args(["check", "--path"])
        .arg(root)
        .arg("--config")
        .arg(&file)
        .args(extra)
        .output()
        .unwrap();
    std::fs::remove_dir_all(&dir).unwrap();
    out
}

const SHOP_RULES: &str = r#"
[components]
scripts = ["scripts"]
billing = ["src/shop/billing"]
legacy = ["src/shop/legacy"]

[[deny]]
from = "scripts"
to = "billing"
reason = "scripts go through the public shop API"
"#;

#[test]
fn check_reports_forbidden_dependencies_and_stale_declarations() {
    let out = check_with("shop", &python_fixture(), SHOP_RULES, &[]);
    assert_eq!(out.status.code(), Some(1));
    let text = String::from_utf8_lossy(&out.stdout);
    for expected in [
        "archmap check: 2 findings",
        "forbidden by deny[0] scripts -> billing: scripts -> shop.billing (import)",
        "  reason: scripts go through the public shop API",
        "  scripts/backfill.py:1 -> src/shop/billing/__init__.py  import",
        "unmatched: components.legacy `src/shop/legacy` matches no component",
    ] {
        assert!(text.contains(expected), "missing `{expected}` in:\n{text}");
    }
}

#[test]
fn check_json_lists_the_same_findings() {
    let out = check_with(
        "shop-json",
        &python_fixture(),
        SHOP_RULES,
        &["--format", "json"],
    );
    assert_eq!(out.status.code(), Some(1));
    let report: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(report["depth"], 2);
    let kinds: Vec<&str> = report["findings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["kind"].as_str().unwrap())
        .collect();
    assert_eq!(kinds, vec!["unmatched", "forbidden"]);
    assert_eq!(
        report["findings"][1]["evidence"][0]["file"],
        "scripts/backfill.py"
    );
}

#[test]
fn check_finds_cycles_at_the_chosen_depth() {
    let repo = temp_repo("cycle");
    std::fs::create_dir_all(repo.join("other")).unwrap();
    std::fs::write(repo.join("pkg/__init__.py"), "from other import thing\n").unwrap();
    std::fs::write(repo.join("other/__init__.py"), "import pkg\n").unwrap();
    let rules = "[cycles]\nforbid = true\n";

    let out = check_with("cycle", &repo, rules, &[]);
    assert_eq!(out.status.code(), Some(1));
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("cycle: other, pkg"), "{text}");
    assert!(
        text.contains(
            "  file level: cycle through other/__init__.py, pkg/__init__.py; closes at module scope"
        ),
        "{text}"
    );
    assert!(
        text.contains("  other -> pkg  other/__init__.py:1 -> pkg/__init__.py  import"),
        "{text}"
    );

    // at depth 0 both fold into the project, so there is nothing to report
    let flat = check_with("cycle-flat", &repo, rules, &["--depth", "0"]);
    assert_eq!(flat.status.code(), Some(0));
    std::fs::remove_dir_all(&repo).unwrap();
}

#[test]
fn archmap_passes_its_own_rules() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let out = archmap()
        .args(["check", "--path"])
        .arg(&root)
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    assert_eq!(out.status.code(), Some(0), "{text}");
    assert!(text.starts_with("archmap check: no findings"));
}

#[test]
fn check_reports_undeclared_imports_unless_ignored() {
    let rules = "[undeclared_imports]\nforbid = true\n";
    let out = check_with("undeclared", &python_fixture(), rules, &[]);
    assert_eq!(out.status.code(), Some(1));
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.contains(
            "undeclared import: google.api_core.exceptions in shop\n  src/shop/analytics.py:3  import"
        ),
        "{text}"
    );

    let ignoring = "[undeclared_imports]\nforbid = true\nignore = [\"google.api_core\"]\n";
    let out = check_with("undeclared-ignored", &python_fixture(), ignoring, &[]);
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
}

fn query_text(root: &Path, args: &[&str]) -> String {
    let out = archmap()
        .arg("query")
        .args(args)
        .arg("--path")
        .arg(root)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}

#[test]
fn query_text_is_a_compact_drill_down() {
    let text = query_text(&python_fixture(), &["shop.integrations.slack"]);
    for expected in [
        "shop.integrations (module, python) at src/shop/integrations, depth 2\n",
        "id: shop::shop.integrations\n",
        "folded from: shop.integrations.slack\n",
        "Children: 1, folded at this depth: use --depth 3\n  shop.integrations.slack\n",
        "Public symbols: 2\n",
        "  def notify(message: str) -> None  src/shop/integrations/slack/__init__.py:1\n",
        "Depends on: none\n",
        "Used by: 1\n  shop  1 import: src/shop/__init__.py:3 -> src/shop/integrations/slack/__init__.py\n",
    ] {
        assert!(text.contains(expected), "missing `{expected}` in:\n{text}");
    }
    assert!(!text.contains("Lists are capped"), "nothing was cut");

    let symbols = query_text(&fixture_root(), &["greet"]);
    assert_eq!(
        symbols,
        "Symbols matching `greet`: 1\n  pub fn greet(user: &User) -> String  crates/lib_core/src/lib.rs:21  in lib_core\n"
    );
}

#[test]
fn query_text_caps_long_lists_and_verbose_lifts_the_caps() {
    let repo = temp_repo("caps");
    let functions: String = (0..40)
        .map(|i| format!("def f{i}():\n    pass\n"))
        .collect();
    std::fs::write(repo.join("pkg/__init__.py"), functions).unwrap();
    std::fs::create_dir_all(repo.join("app")).unwrap();
    let imports: String = (0..5).map(|i| format!("from pkg import f{i}\n")).collect();
    std::fs::write(repo.join("app/__init__.py"), imports).unwrap();

    let capped = query_text(&repo, &["pkg"]);
    assert!(
        capped.contains("Public symbols: 40, showing 30\n"),
        "{capped}"
    );
    assert!(
        capped.contains(
            "app  5 imports: app/__init__.py:1 -> pkg/__init__.py, app/__init__.py:2 -> pkg/__init__.py, app/__init__.py:3 -> pkg/__init__.py, +2 more\n"
        ),
        "{capped}"
    );
    assert!(capped.contains("Lists are capped."));

    let full = query_text(&repo, &["pkg", "--verbose"]);
    assert!(full.contains("Public symbols: 40\n"));
    assert!(full.contains("app/__init__.py:5 -> pkg/__init__.py\n"));
    assert!(!full.contains("more") && !full.contains("Lists are capped"));
    std::fs::remove_dir_all(&repo).unwrap();
}

const LAYERED_SHOP: &str = r#"
[components]
app = ["src/shop"]
scripts = ["scripts"]
tests = ["tests"]

[layers]
order = ["app", "scripts"]   # scripts sits below app, so scripts -> app points up

[[allow]]
from = "tests"
to = []                      # tests may depend on nothing declared

[[allow]]
from = "app"
to = ["scripts"]             # never observed
"#;

#[test]
fn check_enforces_layers_and_allow_lists() {
    let out = check_with("layers", &python_fixture(), LAYERED_SHOP, &[]);
    assert_eq!(out.status.code(), Some(1));
    let text = String::from_utf8_lossy(&out.stdout);
    for expected in [
        "layer violation: scripts must not depend on the higher layer app: scripts -> shop.billing (import)\n  scripts/backfill.py:1 -> src/shop/billing/__init__.py  import\n",
        "unexpected dependency: tests -> app is not in the allow list: tests.unit -> shop (import)\n",
        "stale allowance: app -> scripts is allowed but not observed\n",
    ] {
        assert!(text.contains(expected), "missing `{expected}` in:\n{text}");
    }

    let json = check_with(
        "layers-json",
        &python_fixture(),
        LAYERED_SHOP,
        &["--format", "json"],
    );
    let report: serde_json::Value = serde_json::from_slice(&json.stdout).unwrap();
    let kinds: std::collections::BTreeSet<&str> = report["findings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["kind"].as_str().unwrap())
        .collect();
    assert_eq!(
        kinds,
        [
            "layer_violation",
            "stale_allowance",
            "unexpected_dependency"
        ]
        .into_iter()
        .collect()
    );
}

#[test]
fn check_reports_components_missing_from_the_declarations() {
    let rules = r#"
[components]
app = ["src/shop"]

[coverage]
require = ["src", "tests"]
"#;
    let out = check_with("coverage", &python_fixture(), rules, &[]);
    assert_eq!(out.status.code(), Some(1));
    let text = String::from_utf8_lossy(&out.stdout);
    // tests.unit is a leaf at depth 2 and declared nowhere; `tests` itself is
    // a container, and everything under src/shop is declared as `app`
    assert!(
        text.contains("uncovered: tests.unit at tests/unit belongs to no declared component\n"),
        "{text}"
    );
    assert_eq!(text.matches("uncovered:").count(), 1, "{text}");
}

fn mixed_fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/mixed-utils-project")
}

#[test]
fn check_separates_component_cycles_from_file_cycles_and_reports_signals() {
    let out = check_with("mixed", &mixed_fixture(), "[cycles]\nforbid = true\n", &[]);
    assert_eq!(out.status.code(), Some(1), "cycles are findings");
    let text = String::from_utf8_lossy(&out.stdout);
    for expected in [
        "archmap check: 2 findings, 1 signal",
        // utils <-> core and utils <-> models, but through different files
        "cycle: app.core, app.models, app.utils\n  file level: no cycle; different files form each direction\n",
        // a -> b at module scope, b -> a only inside a function
        "cycle: app.a, app.b\n  file level: cycle through app/a/__init__.py, app/b/__init__.py; closes only through local-scope imports\n",
        "signal: app.utils mixes dependency directions with app.core, app.models\n",
        "  used by them: app/utils/log.py (2)\n",
        "  using them: app/utils/registry.py -> app.models; app/utils/store.py -> app.core\n",
        "Signals are observations; they never change the exit code.",
    ] {
        assert!(text.contains(expected), "missing `{expected}` in:\n{text}");
    }

    // without rules the same signal is reported, and nothing fails
    let signals_only = archmap()
        .args(["check", "--path"])
        .arg(mixed_fixture())
        .output()
        .unwrap();
    assert_eq!(signals_only.status.code(), Some(0));
    assert!(String::from_utf8_lossy(&signals_only.stdout).contains("signal: app.utils mixes"));
}

#[test]
fn impact_does_not_travel_through_a_shared_component() {
    let impact = |target: &str| {
        let out = archmap()
            .args(["impact", target, "--path"])
            .arg(mixed_fixture())
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        serde_json::from_slice::<serde_json::Value>(&out.stdout).unwrap()
    };
    // core is used only by utils/store.py, which nothing else imports: the
    // component graph would claim models too, through utils
    let core = impact("app/core/__init__.py");
    assert_eq!(core["transitive"], serde_json::json!(["mixed::app.utils"]));
    // the logger really is used by core and models
    let log = impact("app/utils/log.py");
    assert_eq!(
        log["direct"],
        serde_json::json!(["mixed::app.core", "mixed::app.models"])
    );
    // a path that exists nowhere is an error, not an empty answer
    let out = archmap()
        .args(["impact", "app/nowhere.py", "--path"])
        .arg(mixed_fixture())
        .output()
        .unwrap();
    assert!(!out.status.success());
}

#[test]
fn summary_says_what_the_map_does_not_cover_before_the_map() {
    let summary = summary_stdout(&[]);
    let coverage = [
        "\n## Coverage",
        "python  files: 15  read: 15  imports without an edge: 4",
        "not analyzed  shell: 1",
        "dynamic imports: 1  in: scripts 1",
        "runtime coupling: not analyzed (HTTP, databases, queues, subprocesses, configuration-driven loading)",
        "",
        "## Components\n",
    ]
    .join("\n");
    assert!(summary.contains(&coverage), "{summary}");
}

#[test]
fn query_lists_imports_the_graph_does_not_map() {
    let text = query_text(&python_fixture(), &["scripts"]);
    let not_mapped = [
        "\nNot mapped: 4",
        "  backfill       local name               1 import: scripts/report.py:4",
        "  helpers        local name               1 import: scripts/report.py:3",
        "  import_module  dynamic                  1 call: scripts/plugins.py:5",
        "  pytest         extra or dev dependency  1 import: scripts/report.py:2\n",
    ]
    .join("\n");
    assert!(text.contains(&not_mapped), "{text}");
    let billing = query_text(&python_fixture(), &["shop.billing"]);
    assert!(billing.contains("\nNot mapped: none\n"), "{billing}");

    // every piece of it, with evidence, in JSON
    let view = fixture_json(&["query", "scripts", "--format", "json"]);
    assert_eq!(view["not_mapped"].as_array().unwrap().len(), 3);
    assert_eq!(view["not_mapped"][2]["reason"], "declared_not_required");
    assert_eq!(view["dynamic_imports"][0]["call"], "import_module");
    assert_eq!(view["dynamic_imports"][0]["evidence"]["scope"], "local");
}

#[test]
fn query_locations_name_the_imported_file_and_mark_local_imports() {
    let utils = query_text(&mixed_fixture(), &["app.utils"]);
    for expected in [
        "Depends on: 2\n  app.core    1 import: app/utils/store.py:1 -> app/core/__init__.py\n  app.models  1 import: app/utils/registry.py:1 -> app/models/__init__.py\n",
        "Used by: 2\n  app.core    1 import: app/core/__init__.py:1 -> app/utils/log.py\n  app.models  1 import: app/models/__init__.py:1 -> app/utils/log.py\n",
    ] {
        assert!(utils.contains(expected), "missing `{expected}` in:\n{utils}");
    }
    // an import inside a function body runs only when the function is called
    let a = query_text(&mixed_fixture(), &["app.a"]);
    assert!(
        a.contains("\n  app.b  1 import: app/b/__init__.py:2 -> app/a/__init__.py (local)\n"),
        "{a}"
    );
    // manifests and Rust name no file: locations stay as they were
    let lib_core = query_text(&fixture_root(), &["lib_core"]);
    assert!(
        lib_core.contains("\n  app  3 imports: crates/app/src/config.rs:1, crates/app/src/main.rs:1, crates/app/src/main.rs:2; declared in crates/app/Cargo.toml\n"),
        "{lib_core}"
    );
}

#[test]
fn a_statement_that_loads_several_files_names_the_first_and_counts_the_rest() {
    let repo = temp_repo("multi-target");
    std::fs::create_dir_all(repo.join("pkg/lib")).unwrap();
    std::fs::create_dir_all(repo.join("pkg/app")).unwrap();
    std::fs::write(repo.join("pkg/lib/__init__.py"), "").unwrap();
    std::fs::write(repo.join("pkg/lib/a.py"), "def f():\n    pass\n").unwrap();
    std::fs::write(repo.join("pkg/lib/b.py"), "def g():\n    pass\n").unwrap();
    std::fs::write(
        repo.join("pkg/app/__init__.py"),
        "from pkg.lib import a, b\n",
    )
    .unwrap();
    let text = query_text(&repo, &["pkg.app"]);
    std::fs::remove_dir_all(&repo).unwrap();
    assert!(
        text.contains("\n  pkg.lib  1 import: pkg/app/__init__.py:1 -> pkg/lib/a.py (+1 file)\n"),
        "{text}"
    );
}
