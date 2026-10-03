//! Smoke tests for the `archmap` binary.

use std::path::{Path, PathBuf};
use std::process::Command;

fn fixture_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/simple-rust-workspace")
}

/// The ids of impact's `direct` or `transitive`, in order.
fn ids(dependents: &serde_json::Value) -> serde_json::Value {
    dependents
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d["id"].clone())
        .collect()
}

/// Where impact's statements are, and the component each is in.
/// The files of impact's tests to run again, without how each reaches.
fn test_files(tests: &serde_json::Value) -> serde_json::Value {
    tests["shown"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["file"].clone())
        .collect()
}

fn sites(statements: &serde_json::Value) -> serde_json::Value {
    statements
        .as_array()
        .unwrap()
        .iter()
        .map(|s| serde_json::json!({"file": s["file"], "line": s["line"], "component": s["component"]}))
        .collect()
}

fn archmap() -> Command {
    keep_own_history_out();
    Command::new(env!("CARGO_BIN_EXE_archmap"))
}

/// Fixtures sit inside archmap's own repository: its history stays out of
/// their answers, in the binary and in process, which read them as roots
/// without one.
fn keep_own_history_out() {
    static CEILING: std::sync::Once = std::sync::Once::new();
    CEILING.call_once(|| {
        let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures");
        std::env::set_var("GIT_CEILING_DIRECTORIES", fixtures);
    });
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
    assert_eq!(graph["schema_version"], 4);
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
    assert_eq!(graph["schema_version"], 4);
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
    // app's import and dependency, and imports from three modules
    assert_eq!(view["incoming"].as_array().unwrap().len(), 5);
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
    let expected = [
        (
            "lib_core",
            "lib_core",
            serde_json::json!(["app", "app::config"]),
        ),
        (
            "crates/lib_core/src/billing.rs",
            "lib_core::billing",
            // nearest first
            serde_json::json!([
                "app",
                "lib_core::api::v1",
                "lib_core::billing::invoice",
                "lib_core::store",
                "app::config"
            ]),
        ),
    ];
    for (target, component, transitive) in expected {
        let out = archmap()
            .args(["impact", target, "--format", "json", "--path"])
            .arg(fixture_root())
            .output()
            .unwrap();
        assert!(out.status.success(), "target {target}");
        let result: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
        assert_eq!(result["target"], component);
        assert_eq!(ids(&result["transitive"]), transitive);
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
        "\nshop.billing -> shop::shop  imports: 2\n",
        // test code is counted apart
        "\ntests -> shop::shop  tests: 1\n",
        // `src/shop/__init__.py` imports its own subpackage
        "\nnot listed: 1 statement of entry files into their own component's submodules\n",
        "## External dependencies\n",
        "\nrequests  declared: pyproject.toml, requirements.txt  importers: 1  top: shop.billing 1\n",
        "\npyyaml  declared: requirements.txt  importers: 1  top: shop.billing 1\n",
        "## Most depended on\n",
        // tests make no dependents
        "\nshop::shop  dependents: 2  dependencies: 0  rank: 1/7\n",
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
    // the project and its package are both named `shop`: lists show the id
    assert!(
        shallow.contains("\ntests -> shop::shop  tests: 3\n"),
        "{shallow}"
    );

    let packages_only = summary_stdout(&["--depth", "0"]);
    assert!(
        packages_only
            .contains("\nshop  package  language: python  path: .  symbols: 11  folded: 7\n"),
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

#[test]
fn summary_caps_long_lists_unless_verbose() {
    let repo = temp_repo("capped");
    for i in 0..35 {
        let dir = repo.join(format!("pkg/sub{i:02}"));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("__init__.py"), "def run():\n    pass\n").unwrap();
    }
    let summary = |args: &[&str]| {
        let out = archmap()
            .arg("summary")
            .arg(&repo)
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap()
    };

    let capped = summary(&[]);
    for expected in [
        "\ncomponents: 30 shown of 37 at depth, 37 in the full graph\n",
        "\nomitted: 7 modules  in: pkg 7  next: query <component>\n",
    ] {
        assert!(
            capped.contains(expected),
            "missing `{expected}` in:\n{capped}"
        );
    }

    let all = summary(&["--verbose"]);
    assert!(all.contains("\ncomponents: 37 shown, 37 in the full graph\n"));
    assert!(!all.contains("omitted: "), "{all}");
    assert_eq!(all.matches("\n    pkg.sub").count(), 35, "{all}");
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
    let result = fixture_json(&[
        "impact",
        "src/shop/integrations/slack/__init__.py",
        "--format",
        "json",
    ]);
    assert_eq!(result["target"], "shop::shop.integrations");
    assert_eq!(result["folded_from"], "shop::shop.integrations.slack");
    assert_eq!(ids(&result["direct"]), serde_json::json!(["shop::shop"]));
    // followed file by file: src/shop/__init__.py imports slack, and runs
    // before any module below it, so tests.unit, which imports only
    // shop/users.py, is reached through it
    assert_eq!(
        ids(&result["transitive"]),
        serde_json::json!(["shop::shop", "shop::scripts", "shop::shop.billing"])
    );
    // the tests to run again, each with how it reaches the target
    assert_eq!(result["tests"]["total"], 2);
    assert_eq!(
        test_files(&result["tests"]),
        serde_json::json!(["tests/test_billing.py", "tests/unit/factories.py"])
    );
    assert_eq!(
        result["tests"]["shown"][0]["ways"],
        serde_json::json!([{"kind": "runs_first", "file": "src/shop/__init__.py"}])
    );
}

#[test]
fn impact_of_a_package_entry_file_lists_the_imports_below_it() {
    let out = archmap()
        .args(["impact", "src/shop/__init__.py", "--path"])
        .arg(python_fixture())
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    // statements outside the package that import a module below it, which
    // run src/shop/__init__.py first; those inside it ran it already
    assert!(
        text.contains(
            "\nImports below: 5 (they run it first)\n  scripts/2024-01-migration/fix.py:1  in scripts\n  \
             scripts/backfill.py:1  in scripts\n  tests/test_billing.py:1 (test)  in tests\n"
        ),
        "{text}"
    );
    let json = fixture_json(&["impact", "src/shop/__init__.py", "--format", "json"]);
    assert_eq!(json["imports_below"]["total"], 5);
    // query counts them and names the impact that lists them
    let query = query_text(&python_fixture(), &["src/shop/__init__.py"]);
    assert!(
        query.contains(
            "\nImports below: 5 (they run it first): `impact src/shop/__init__.py` lists them\n"
        ),
        "{query}"
    );
}

#[test]
fn a_package_entry_file_that_runs_first_is_no_file_nothing_imports() {
    let repo = temp_repo("runs-first");
    std::fs::write(repo.join("pkg/__init__.py"), "import logging\n").unwrap();
    std::fs::write(repo.join("pkg/sub.py"), "def x():\n    pass\n").unwrap();
    std::fs::write(repo.join("main.py"), "from pkg.sub import x\n").unwrap();
    let query = query_text(&repo, &["pkg/__init__.py"]);
    let impact = archmap()
        .args(["impact", "pkg/__init__.py", "--path"])
        .arg(&repo)
        .output()
        .unwrap();
    std::fs::remove_dir_all(&repo).unwrap();
    let impact = String::from_utf8_lossy(&impact.stdout);
    for text in [query.as_str(), &impact] {
        assert!(!text.contains("no importers"), "{text}");
    }
    assert!(
        query.contains("\nImports below: 1 (they run it first)"),
        "{query}"
    );
    assert!(
        impact.contains("\nImports below: 1 (they run it first)\n  main.py:1"),
        "{impact}"
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

        let impact = fixture_json(&["impact", name, "--format", "json"]);
        assert!(impact.get("folded_from").is_none());
        for id in ids(&impact["transitive"]).as_array().unwrap() {
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
fn check_finds_no_cycle_that_only_type_checking_closes() {
    let repo = temp_repo("type-checking-cycle");
    std::fs::create_dir_all(repo.join("other")).unwrap();
    std::fs::write(repo.join("pkg/__init__.py"), "from other import thing\n").unwrap();
    std::fs::write(
        repo.join("other/__init__.py"),
        "from typing import TYPE_CHECKING\n\nif TYPE_CHECKING:\n    import pkg\n",
    )
    .unwrap();

    let rules = "[cycles]\nforbid = true\n";
    let out = check_with("type-checking-cycle", &repo, rules, &[]);
    let text = String::from_utf8_lossy(&out.stdout);
    assert_eq!(out.status.code(), Some(0), "{text}");
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

/// The stdout of a command that answers with candidates: exit code 1.
fn candidates(args: &[&str], root: &Path) -> String {
    let out = archmap()
        .args(args)
        .arg("--path")
        .arg(root)
        .output()
        .unwrap();
    assert_eq!(
        out.status.code(),
        Some(1),
        "{args:?}: {}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
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
        "Children: 1, folded at this depth: query at depth 3\n  shop.integrations.slack\n",
        "Public symbols: 2\n",
        "  def notify(message: str) -> None  src/shop/integrations/slack/__init__.py:1\n",
        "Depends on: none\n",
        // the package's `__init__.py` holds its subpackage rather than
        // depends on it, as summary counts it
        "Used by: 1\n  shop::shop  1 of its entry file: src/shop/__init__.py:3 -> src/shop/integrations/slack/__init__.py\n",
    ] {
        assert!(text.contains(expected), "missing `{expected}` in:\n{text}");
    }
    assert!(!text.contains("Lists are capped"), "nothing was cut");

    // every Rust file of the fixture is read, its integration test included
    let symbols = query_text(&fixture_root(), &["greet"]);
    assert_eq!(
        symbols,
        "Symbols matching `greet`: 1\n  pub fn greet(user: &User) -> String  crates/lib_core/src/lib.rs:28  in lib_core\n\
         \nImported by: 2\n  crates/app/src/config.rs:12 (local)\n  crates/app/src/main.rs:2\n\
         \nUsed at: 2 in 2 files (2 calls)\n  crates/app/src/config.rs:12 (call) as lib_core::greet\n  \
         crates/app/src/main.rs:8 (call)\n"
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
    assert!(capped.contains("Lists are capped; JSON lists every entry with all evidence."));

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
to = []                      # test code breaks no rule

[[allow]]
from = "scripts"
to = []                      # scripts may depend on nothing declared

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
        "unexpected dependency: scripts -> app is not in the allow list: scripts -> shop.billing (import)\n",
        "stale allowance: app -> scripts is allowed but not observed\n",
    ] {
        assert!(text.contains(expected), "missing `{expected}` in:\n{text}");
    }
    assert!(!text.contains("tests -> app"), "{text}");

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
            .args(["impact", target, "--format", "json", "--path"])
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
    assert_eq!(
        ids(&core["transitive"]),
        serde_json::json!(["mixed::app.utils"])
    );
    // the logger really is used by core and models
    let log = impact("app/utils/log.py");
    assert_eq!(
        ids(&log["direct"]),
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
        "python  files: 15  read: 15  imports without an edge: 3 (undeclared 1, extra or dev dependency 1, local name 1 (1 in tests))",
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
        "\nNot mapped: 2",
        "  import_module  dynamic                  1 call: scripts/plugins.py:5 (local)",
        "  pytest         extra or dev dependency  1 import: scripts/report.py:2\n",
    ]
    .join("\n");
    assert!(text.contains(&not_mapped), "{text}");
    let tests = query_text(&python_fixture(), &["tests"]);
    assert!(
        tests.contains(
            "\nNot mapped: 1\n  helpers  local name  1 import: tests/test_billing.py:3 (test)\n"
        ),
        "{tests}"
    );
    let billing = query_text(&python_fixture(), &["shop.billing"]);
    assert!(billing.contains("\nNot mapped: none\n"), "{billing}");

    // every piece of it, with evidence, in JSON
    let view = fixture_json(&["query", "scripts", "--format", "json"]);
    assert_eq!(view["not_mapped"].as_array().unwrap().len(), 1);
    assert_eq!(view["not_mapped"][0]["reason"], "declared_not_required");
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
    // a Rust `use` names the file too, and a manifest names none
    let store = query_text(&fixture_root(), &["lib_core::store"]);
    for expected in [
        "\n  lib_core::billing        1 import: crates/lib_core/src/store/mod.rs:3 -> crates/lib_core/src/billing.rs\n",
        "\n  lib_core::store::memory  1 import: crates/lib_core/src/store/mod.rs:10 -> crates/lib_core/src/store/memory.rs (local)\n",
    ] {
        assert!(store.contains(expected), "missing `{expected}` in:\n{store}");
    }
    let lib_core = query_text(&fixture_root(), &["lib_core"]);
    assert!(
        lib_core.contains("  1 import: crates/app/src/main.rs:2 -> crates/lib_core/src/lib.rs; declared in crates/app/Cargo.toml:7\n"),
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

#[test]
fn query_a_file_shows_its_symbols_imports_and_importers() {
    let text = query_text(&mixed_fixture(), &["app/utils/log.py"]);
    for expected in [
        "app/utils/log.py (file) in app.utils (module, python), depth 2\n",
        "id: mixed::app.utils\n",
        "Public symbols: 1\n  def get_logger()  app/utils/log.py:1\n",
        "Imports: none\n",
        "Imported by: 2\n  app.core    1 import: app/core/__init__.py:1\n  app.models  1 import: app/models/__init__.py:1\n",
        "Not mapped: none\n",
    ] {
        assert!(text.contains(expected), "missing `{expected}` in:\n{text}");
    }
    // a dotted module name reaches the same file
    let dotted = query_text(&mixed_fixture(), &["app.utils.log"]);
    assert!(
        dotted.contains("Imported by: 2\n  app.core    1 import: app/core/__init__.py:1\n"),
        "{dotted}"
    );
    // what a file imports names the loaded file and marks local imports
    let b = query_text(&mixed_fixture(), &["app/b/__init__.py"]);
    assert!(
        b.contains(
            "Imports: 1\n  app.a  1 import: app/b/__init__.py:2 -> app/a/__init__.py (local)\n"
        ),
        "{b}"
    );
    // imports without an edge are listed per file too
    let report = query_text(&python_fixture(), &["scripts/report.py"]);
    assert!(report.contains("\nNot mapped: 1\n"), "{report}");
}

#[test]
fn a_file_imported_by_bare_name_from_its_own_directory_has_that_importer() {
    // scripts/report.py runs as a script and writes `import helpers`
    let text = query_text(&python_fixture(), &["scripts/helpers.py"]);
    assert!(
        text.contains("\nImported by: 1\n  scripts  1 import: scripts/report.py:3\n"),
        "{text}"
    );
    let impact = fixture_json(&["impact", "scripts/helpers.py", "--format", "json"]);
    assert_eq!(
        sites(&impact["importers"]["statements"]),
        serde_json::json!([
            {"file": "scripts/report.py", "line": 3, "component": "shop::scripts"}
        ])
    );
}

#[test]
fn query_a_rust_file_lists_the_statements_that_import_it() {
    let text = query_text(&fixture_root(), &["crates/lib_core/src/lib.rs"]);
    for expected in [
        "crates/lib_core/src/lib.rs (file) in lib_core (package, rust), depth 2\n",
        "\nImports: 1\n  ext:cargo:serde  1 import: crates/lib_core/src/lib.rs:2\n",
        "\nImported by: 4\n",
        // a `use`, a module path in a function body, and a `use` in a test module
        "\n  app::config                 2 imports, 1 in tests: crates/app/src/config.rs:1, crates/app/src/config.rs:12 (local), crates/app/src/config.rs:17 (test)\n",
        "\n  lib_core::billing::invoice  1 import: crates/lib_core/src/billing/invoice.rs:3\n",
    ] {
        assert!(text.contains(expected), "missing `{expected}` in:\n{text}");
    }
    // an import through a re-export counts for the file that defines the item
    let invoice = query_text(&fixture_root(), &["crates/lib_core/src/billing/invoice.rs"]);
    assert!(
        invoice.contains(
            "\nImported by: 1\n  app::config  1 import: crates/app/src/config.rs:1 \
             (via crates/lib_core/src/lib.rs:7)\n"
        ),
        "{invoice}"
    );
}

#[test]
fn impact_of_a_file_lists_the_statements_that_import_it() {
    let out = archmap()
        .args(["impact", "app/utils/log.py", "--format", "json", "--path"])
        .arg(mixed_fixture())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let result: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(result["importers"]["total"], 2);
    assert_eq!(
        sites(&result["importers"]["statements"]),
        serde_json::json!([
            {"file": "app/core/__init__.py", "line": 1, "component": "mixed::app.core"},
            {"file": "app/models/__init__.py", "line": 1, "component": "mixed::app.models"}
        ])
    );
    // a component target has no importer list
    let out = archmap()
        .args(["impact", "app.utils", "--format", "json", "--path"])
        .arg(mixed_fixture())
        .output()
        .unwrap();
    let result: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert!(result.get("importers").is_none(), "{result}");
}

#[test]
fn a_directory_stands_for_the_component_that_owns_it() {
    // query answers for the same component as impact, by name or by path
    let by_name = query_text(&python_fixture(), &["shop.billing"]);
    for dir in ["src/shop/billing", "./src/shop/billing/"] {
        assert_eq!(query_text(&python_fixture(), &[dir]), by_name, "{dir}");
        let impact = fixture_json(&["impact", dir, "--format", "json"]);
        assert_eq!(impact["target"], "shop::shop.billing", "{dir}");
    }
    // a directory inside a folded component answers for that component
    let folded = query_text(&python_fixture(), &["src/shop/integrations/slack"]);
    assert!(
        folded.starts_with(
            "shop.integrations (module, python) at src/shop/integrations, depth 2\n\
             id: shop::shop.integrations\nfolded from: shop.integrations.slack\n"
        ),
        "{folded}"
    );
}

#[test]
fn a_rust_module_directory_stands_for_its_module() {
    // the directory that holds a module's `mod.rs`, or the submodules of a
    // file beside it, answers for that module; `src/` stays the package's,
    // and so do a directory beside `src/main.rs` and one of an inline module
    for (dir, id) in [
        ("crates/lib_core/src/store", "lib_core::store"),
        ("crates/lib_core/src/billing/", "lib_core::billing"),
        ("crates/lib_core/src", "lib_core"),
        ("crates/app/src", "app"),
        ("crates/lib_core/src/api", "lib_core"),
    ] {
        let text = query_text(&fixture_root(), &[dir]);
        assert!(text.contains(&format!("\nid: {id}\n")), "{dir}: {text}");
        let out = archmap()
            .args(["impact", dir, "--format", "json", "--path"])
            .arg(fixture_root())
            .output()
            .unwrap();
        let impact: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
        assert_eq!(impact["target"], id, "{dir}");
    }
    // a test's helper in `tests/common/mod.rs`, with no submodule, is still
    // its directory's module
    let targets = fixture_root().join("../rust-cargo-targets");
    let text = query_text(&targets, &["kiosk/tests/common"]);
    assert!(
        text.contains("\nid: kiosk::tests/common/mod.rs\n"),
        "{text}"
    );
}

#[test]
fn a_directory_without_code_beside_a_file_of_its_name_stays_its_owners() {
    // `src/Button/` holds a stylesheet only, so `Button.tsx` has nothing there
    let repo = temp_repo("dir-beside-data");
    std::fs::write(repo.join("package.json"), "{\"name\": \"web\"}\n").unwrap();
    std::fs::create_dir_all(repo.join("src/Button")).unwrap();
    std::fs::write(repo.join("src/Button.tsx"), "export const Button = 1;\n").unwrap();
    std::fs::write(
        repo.join("src/App.tsx"),
        "import { Button } from './Button';\nexport const App = Button;\n",
    )
    .unwrap();
    std::fs::write(repo.join("src/Button/style.css"), ".b {}\n").unwrap();
    let text = query_text(&repo, &["src/Button"]);
    assert!(text.contains("\nid: web\n"), "{text}");
}

#[test]
fn a_symbol_wins_over_a_directory_of_the_same_name() {
    // `run/` holds no Python code, so only the root component contains it
    let repo = temp_repo("dir-symbol");
    std::fs::create_dir_all(repo.join("run")).unwrap();
    std::fs::write(repo.join("run/settings.yaml"), "a: 1\n").unwrap();
    let text = query_text(&repo, &["run"]);
    std::fs::remove_dir_all(&repo).unwrap();
    assert!(text.starts_with("Symbols matching `run`: 1\n"), "{text}");
}

#[test]
fn an_import_name_without_an_edge_lists_where_it_is_imported() {
    // pytest is declared only as a dev extra
    let pytest = query_text(&python_fixture(), &["pytest"]);
    for expected in [
        "pytest: imports without an edge, depth 2\nin: scripts 1\n",
        "\nNot mapped: 1\n  pytest  extra or dev dependency  1 import: scripts/report.py:2\n",
        "\nNotes: 1\n  import pytest, declared as pytest in pyproject.toml:7 ([project.optional-dependencies] dev)\n",
    ] {
        assert!(pytest.contains(expected), "missing `{expected}` in:\n{pytest}");
    }
    // helpers is reached through sys.path; google covers google.api_core.*
    let helpers = query_text(&python_fixture(), &["helpers"]);
    assert!(
        helpers.contains("\n  helpers  local name  1 import: tests/test_billing.py:3 (test)\n"),
        "{helpers}"
    );
    let google = query_text(&python_fixture(), &["google"]);
    assert!(
        google.contains(
            "\n  google.api_core.exceptions  undeclared  1 import: src/shop/analytics.py:3\n"
        ),
        "{google}"
    );
    // Rust dev-dependencies are imported by crate name, in unit tests and in
    // integration tests alike
    let rust = query_text(&fixture_root(), &["assert_cmd"]);
    assert!(
        rust.contains(
            "\n  assert_cmd  extra or dev dependency  2 imports: crates/app/src/main.rs:14 (test), \
             crates/app/tests/smoke.rs:1 (test)\n"
        ),
        "{rust}"
    );
    // every import with its evidence in JSON
    let view = fixture_json(&["query", "pytest", "--format", "json"]);
    assert_eq!(view["module"], "pytest");
    assert_eq!(
        view["not_mapped"][0]["evidence"]["file"],
        "scripts/report.py"
    );
    // a dotted prefix, not a string prefix
    let out = archmap()
        .args(["query", "google.api", "--path"])
        .arg(python_fixture())
        .output()
        .unwrap();
    assert!(!out.status.success());
}

#[test]
fn imports_without_an_edge_of_one_name_are_capped() {
    let repo = temp_repo("unmapped-caps");
    std::fs::write(
        repo.join("pyproject.toml"),
        "[project]\nname = \"demo\"\n\n[project.optional-dependencies]\nml = [\"torch\"]\n",
    )
    .unwrap();
    for i in 0..5 {
        std::fs::write(repo.join(format!("pkg/m{i}.py")), "import torch\n").unwrap();
    }
    // the one import elsewhere, inside a function
    std::fs::create_dir_all(repo.join("tools")).unwrap();
    std::fs::write(repo.join("tools/run.py"), "def main():\n    import torch\n").unwrap();
    let capped = query_text(&repo, &["torch"]);
    let all = query_text(&repo, &["torch", "--verbose"]);
    std::fs::remove_dir_all(&repo).unwrap();
    assert!(capped.contains("\nin: pkg 5, tools 1\n"), "{capped}");
    // one location per component first, so the cap cannot hide tools
    assert!(
        capped.contains(
            "\n  torch  extra or dev dependency  6 imports: pkg/m0.py:1, tools/run.py:2 (local), pkg/m1.py:1, +3 more\n"
        ),
        "{capped}"
    );
    assert!(
        capped.contains("Lists are capped; JSON lists every entry with all evidence."),
        "{capped}"
    );
    assert!(all.contains("pkg/m4.py:1\n"), "{all}");
}

#[test]
fn a_statement_that_imports_several_modules_counts_once() {
    // `from torch import nn, Tensor` is one statement: two modules
    // without an edge (torch.nn, and torch for Tensor), one import
    let repo = temp_repo("statements");
    std::fs::write(
        repo.join("pyproject.toml"),
        "[project]\nname = \"demo\"\n\n[project.optional-dependencies]\nml = [\"torch\"]\n",
    )
    .unwrap();
    let dist_info = repo.join(".venv/lib/python3.12/site-packages/torch-2.0.dist-info");
    std::fs::create_dir_all(&dist_info).unwrap();
    std::fs::write(
        dist_info.join("RECORD"),
        "torch/__init__.py,,\ntorch/nn/__init__.py,,\n",
    )
    .unwrap();
    std::fs::write(repo.join("pkg/m.py"), "from torch import nn, Tensor\n").unwrap();
    let summary = archmap().arg("summary").arg(&repo).output().unwrap();
    let text = query_text(&repo, &["torch"]);
    std::fs::remove_dir_all(&repo).unwrap();
    let summary = String::from_utf8(summary.stdout).unwrap();
    assert!(
        summary.contains("imports without an edge: 1 (extra or dev dependency 1)\n"),
        "{summary}"
    );
    assert!(text.contains("\nin: pkg 1\n\nNot mapped: 2\n"), "{text}");
}

#[test]
fn query_an_unknown_dotted_module_fails() {
    let out = archmap()
        .args(["query", "app.utils.nope", "--path"])
        .arg(mixed_fixture())
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("app.utils.nope"));
}

/// Python projects that each have a `tests` directory, so each project
/// adds a component named `tests`.
fn projects_with_tests(name: &str, projects: &[&str]) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("archmap-cli-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    for project in projects {
        std::fs::create_dir_all(dir.join(project).join("tests")).unwrap();
        std::fs::write(
            dir.join(project).join("pyproject.toml"),
            format!("[project]\nname = \"{project}\"\nversion = \"0.1.0\"\n"),
        )
        .unwrap();
        std::fs::write(
            dir.join(project).join("tests/test_it.py"),
            "def test_it():\n    pass\n",
        )
        .unwrap();
    }
    dir
}

fn two_projects_with_tests(name: &str) -> PathBuf {
    projects_with_tests(name, &["a", "b"])
}

#[test]
fn query_lists_components_that_share_a_name() {
    let repo = two_projects_with_tests("ambiguous-query");
    let stdout = candidates(&["query", "tests"], &repo);
    std::fs::remove_dir_all(&repo).unwrap();
    assert_eq!(
        stdout,
        "`tests` names 2 components; retry with one of them by id or path:\n  \
         a::tests  a/tests  module\n  b::tests  b/tests  module\n"
    );
}

#[test]
fn impact_lists_components_that_share_a_name() {
    let repo = two_projects_with_tests("ambiguous-impact");
    let text = candidates(&["impact", "tests"], &repo);
    let stdout = candidates(&["impact", "tests", "--format", "json"], &repo);
    std::fs::remove_dir_all(&repo).unwrap();
    assert_eq!(
        text,
        "`tests` names 2 components; retry with one of them by id or path:\n  \
         a::tests  a/tests  module\n  b::tests  b/tests  module\n"
    );
    let json: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(json["requested"], "tests");
    assert_eq!(json["total"], 2);
    assert_eq!(json["candidates"][0]["id"], "a::tests");
    assert_eq!(json["candidates"][0]["path"], "a/tests");
    assert_eq!(json["candidates"][1]["id"], "b::tests");
}

#[test]
fn a_shared_name_is_reported_even_when_roll_up_hides_one_component() {
    // A local package `requests` beside the declared distribution of that
    // name: at depth 0 the local module folds into its project and only the
    // external one stays visible, but the name is still ambiguous.
    let dir = std::env::temp_dir().join(format!(
        "archmap-cli-ambiguous-folded-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("p/requests")).unwrap();
    std::fs::write(
        dir.join("p/pyproject.toml"),
        "[project]\nname = \"p\"\nversion = \"0.1.0\"\ndependencies = [\"requests\"]\n",
    )
    .unwrap();
    std::fs::write(dir.join("p/requests/__init__.py"), "def get():\n    pass\n").unwrap();
    let stdout = candidates(&["query", "requests", "--depth", "0"], &dir);
    std::fs::remove_dir_all(&dir).unwrap();
    assert!(
        stdout
            .starts_with("`requests` names 2 components; retry with one of them by id or path:\n"),
        "{stdout}"
    );
    assert!(
        stdout.contains("\n  ext:pypi:requests  -  external\n"),
        "{stdout}"
    );
    assert!(
        stdout.contains("\n  p::requests  p/requests  module\n"),
        "{stdout}"
    );
}

#[test]
fn the_components_that_share_a_name_are_capped() {
    let projects: Vec<String> = (0..12).map(|i| format!("p{i:02}")).collect();
    let names: Vec<&str> = projects.iter().map(String::as_str).collect();
    let repo = projects_with_tests("ambiguous-capped", &names);
    let stdout = candidates(&["query", "tests"], &repo);
    let json = candidates(&["query", "tests", "--format", "json"], &repo);
    std::fs::remove_dir_all(&repo).unwrap();
    assert!(stdout.contains("`tests` names 12 components"), "{stdout}");
    let listed = stdout.lines().filter(|l| l.contains("::tests  ")).count();
    assert_eq!(listed, 10, "{stdout}");
    assert!(stdout.ends_with("\n  +2 more\n"), "{stdout}");
    // JSON lists every one
    let json: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(json["candidates"].as_array().unwrap().len(), 12);
}

#[test]
fn a_dot_slash_path_reaches_one_of_the_components_that_share_a_name() {
    let repo = two_projects_with_tests("ambiguous-path");
    let out = archmap()
        .args(["query", "./a/tests", "--path"])
        .arg(&repo)
        .output()
        .unwrap();
    std::fs::remove_dir_all(&repo).unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(String::from_utf8_lossy(&out.stdout).contains("id: a::tests"));
}

#[test]
fn check_explains_external_selectors_without_an_ecosystem() {
    let rules = "[[deny]]\nfrom = \"scripts\"\nto = \"ext:requests\"\n";
    let out = check_with("ext-hint", &python_fixture(), rules, &[]);
    assert_eq!(out.status.code(), Some(1));
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.contains(
            "unmatched: deny[0].to `ext:requests` matches no component; \
             external ids name their ecosystem (`ext:cargo:serde`, `ext:pypi:requests`)"
        ),
        "stdout: {text}"
    );
}

fn ts_fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/simple-ts-project")
}

fn ts_stdout(args: &[&str]) -> String {
    let out = archmap()
        .args(args)
        .arg("--path")
        .arg(ts_fixture())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn a_ts_file_query_lists_importers_through_aliases_and_re_exports() {
    let text = ts_stdout(&["query", "src/lib/money.ts"]);
    for expected in [
        "Imported by:",
        "src/app/page.tsx:1",
        "src/index.ts:1",
        "tests/helpers.ts:1",
        "tests/money.test.ts:2",
    ] {
        assert!(text.contains(expected), "missing `{expected}` in:\n{text}");
    }
}

#[test]
fn a_ts_file_component_is_queried_by_its_name_as_its_file() {
    let text = ts_stdout(&["query", "lib/money.ts"]);
    for expected in [
        "src/lib/money.ts (file) in lib/money.ts (module, typescript), depth 2\n",
        "  export function formatPrice(price: Money): string  src/lib/money.ts:8\n",
        "\nImported by:",
    ] {
        assert!(text.contains(expected), "missing `{expected}` in:\n{text}");
    }
}

#[test]
fn a_folded_ts_file_is_queried_as_its_file() {
    // At depth 1 both files fold into `tests`, whose own view would show
    // no importer.
    let text = ts_stdout(&["query", "tests/helpers.ts", "--depth", "1"]);
    for expected in [
        "tests/helpers.ts (file) in tests (module, typescript), depth 1\n",
        "\nImported by: 1\n  tests  1 import in tests: tests/money.test.ts:3 (test)\n",
    ] {
        assert!(text.contains(expected), "missing `{expected}` in:\n{text}");
    }
    let text = ts_stdout(&["query", "lib/money.ts", "--depth", "1"]);
    assert!(
        text.starts_with("src/lib/money.ts (file) in lib (module, typescript), depth 1\n"),
        "{text}"
    );
}

#[test]
fn impact_of_a_ts_file_component_lists_its_importers() {
    let text = ts_stdout(&["impact", "lib/money.ts"]);
    assert!(
        text.contains("\nImported by: 9, showing 5 (2 re-exports)\n"),
        "{text}"
    );
    assert!(text.contains("\n  src/app/page.tsx:1"), "{text}");
}

#[test]
fn a_rust_module_without_submodules_is_queried_as_its_file() {
    let invoice = query_text(&fixture_root(), &["lib_core::billing::invoice"]);
    assert!(
        invoice.starts_with(
            "crates/lib_core/src/billing/invoice.rs (file) in lib_core::billing::invoice \
             (module, rust), depth 2\n"
        ),
        "{invoice}"
    );
    // a module with submodules stays a component
    let billing = query_text(&fixture_root(), &["lib_core::billing"]);
    assert!(
        billing.starts_with("lib_core::billing (module, rust)"),
        "{billing}"
    );
}

#[test]
fn impact_names_the_package_file_that_imports() {
    let text = ts_stdout(&["impact", "src/lib/limits.ts", "--format", "json"]);
    assert!(text.contains("\"file\": \"next.config.ts\""), "{text}");
    assert!(text.contains("\"file\": \"scripts/seed.mjs\""), "{text}");
}

#[test]
fn an_undeclared_npm_import_is_found_by_its_name() {
    let text = ts_stdout(&["query", "left-pad"]);
    assert!(text.contains("src/app/page.tsx:11"), "{text}");
}

#[test]
fn ts_summary_counts_both_languages_and_why_imports_have_no_edge() {
    let out = archmap()
        .arg("summary")
        .arg(ts_fixture())
        .args(["-o", "-"])
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    for expected in [
        // vitest, imported by a test
        "typescript  files: 14  read: 14  imports without an edge: 8 (undeclared 2, extra or dev dependency 2 (1 in tests), local name 1, unresolved 3)",
        "javascript  files: 3  read: 3  imports without an edge: 0",
        // src/global.d.ts
        "scripts: 1 (no import or export: what uses their declarations is not traced)",
        // `require` and `import()` of a computed name
        "dynamic imports: 2  in: scripts/report.cjs 1, app/lazy.tsx 1",
    ] {
        assert!(text.contains(expected), "missing `{expected}` in:\n{text}");
    }
}

#[test]
fn a_python_package_keeps_its_files_when_scripts_sit_below_it() {
    // A TS/JS directory component shares the package's path, and its id
    // sorts after the package's.
    let dir = std::env::temp_dir().join(format!("archmap-cli-django-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    for (file, text) in [
        (
            "pyproject.toml",
            "[project]\nname = \"app\"\ndependencies = []\n",
        ),
        ("myapp/__init__.py", ""),
        (
            "myapp/views.py",
            "import json\n\n\ndef index():\n    return json.dumps({})\n",
        ),
        ("myapp/static/myapp/app.js", "export const ready = true;\n"),
        // folds into the TS/JS `myapp` at depth 1, which must not make it
        // the owner there
        ("myapp/x.js", "export const x = 1;\n"),
    ] {
        let path = dir.join(file);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }
    let file = query_text(&dir, &["myapp/views.py"]);
    let directory = query_text(&dir, &["myapp/"]);
    // both components are named `myapp`: the one that owns the path answers
    let named = query_text(&dir, &["myapp"]);
    let shallow = query_text(&dir, &["myapp", "--depth", "1"]);
    std::fs::remove_dir_all(&dir).unwrap();
    assert!(
        file.starts_with("myapp/views.py (file) in myapp (module, python), depth 2\n"),
        "{file}"
    );
    assert!(
        directory.starts_with("myapp (module, python) at myapp, depth 2\n"),
        "{directory}"
    );
    assert!(
        named.starts_with("myapp (module, python) at myapp, depth 2\n"),
        "{named}"
    );
    assert!(
        shallow.starts_with("myapp (module, python) at myapp, depth 1\n"),
        "{shallow}"
    );
    // the TS/JS directory at the same path is named, so it can be queried
    assert!(
        named.contains("\nalso at this path: ") && named.contains("::myapp\n"),
        "{named}"
    );
}

#[test]
fn query_and_impact_name_other_components_of_the_same_name() {
    let dir = std::env::temp_dir().join(format!("archmap-cli-dup-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    for (file, text) in [
        (
            "rust/Cargo.toml",
            "[package]\nname = \"dup\"\nversion = \"0.1.0\"\n",
        ),
        ("rust/src/lib.rs", "pub fn f() {}\n"),
        ("web/package.json", "{ \"name\": \"dup\" }\n"),
        ("web/index.js", "export const g = 1;\n"),
    ] {
        let path = dir.join(file);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }
    // `dup` is the Rust package's id; the TS/JS one was renamed
    let text = query_text(&dir, &["dup"]);
    let impact = |format: &str| {
        archmap()
            .args(["impact", "dup", "--format", format, "--path"])
            .arg(&dir)
            .output()
            .unwrap()
            .stdout
    };
    let (impact_text, impact_json) = (impact("text"), impact("json"));
    std::fs::remove_dir_all(&dir).unwrap();
    assert!(text.contains("\nalso named: dup+typescript\n"), "{text}");
    let impact_text = String::from_utf8(impact_text).unwrap();
    assert!(
        impact_text.contains("\nalso named: dup+typescript\n"),
        "{impact_text}"
    );
    let impact: serde_json::Value = serde_json::from_slice(&impact_json).unwrap();
    assert_eq!(impact["also_named"], serde_json::json!(["dup+typescript"]));
}

#[test]
fn a_ts_file_query_shows_importers_that_come_through_re_exports() {
    let text = ts_stdout(&["query", "src/components/button.tsx"]);
    for expected in [
        "src/app/page.tsx:6 (via src/components/index.ts:1)",
        "src/app/checkout.ts:1 (via src/index.ts:2)",
    ] {
        assert!(text.contains(expected), "missing `{expected}` in:\n{text}");
    }
    // the statement reaches limits.ts through two re-exports; a file query
    // shows a statement's first evidence in sort order
    let text = ts_stdout(&["query", "src/app/checkout.ts"]);
    assert!(
        text.contains("src/app/checkout.ts:1 -> src/lib/limits.ts (via src/index.ts:3)"),
        "{text}"
    );
}

#[test]
fn a_statement_counts_each_other_file_it_loads_once() {
    // checkout.ts:1 reaches lib/limits.ts and lib/money.ts through two
    // re-exports each: the other files count, not their evidence
    let text = ts_stdout(&["query", "src/app/checkout.ts", "--depth", "0"]);
    assert!(
        text.contains("src/app/checkout.ts:1 -> src/index.ts (+3 files)"),
        "{text}"
    );
    let text = ts_stdout(&["query", "src/app/checkout.ts", "--depth", "1"]);
    assert!(
        text.contains("src/app/checkout.ts:1 -> src/lib/limits.ts (+1 file) (via src/index.ts:3)"),
        "{text}"
    );
}

#[test]
fn a_symbol_query_lists_the_statements_that_import_it() {
    let text = ts_stdout(&["query", "formatPrice"]);
    for expected in [
        "Imported by: 4 (1 re-export)\n",
        "\n  src/app/page.tsx:1\n",
        "\n  src/index.ts:1 (export)\n",
        "\n  src/app/checkout.ts:1 (via src/index.ts:1)\n",
        "\n  tests/money.test.ts:2 (test)\n",
        // a namespace re-export and an `import()` take the file whole, and
        // checkout.ts:2 the barrel that passes the name on; checkout.ts:1,
        // which also does through the re-export, is listed by name already
        "May use: 3 (imports the whole module; 1 re-export)\n  scripts/report.cjs:8 (local)\n  \
         src/app/checkout.ts:2 (whole src/index.ts, which passes it on)\n  src/index.ts:4 (export)\n",
    ] {
        assert!(text.contains(expected), "missing `{expected}` in:\n{text}");
    }
    let json = ts_stdout(&["query", "formatPrice", "--format", "json"]);
    for expected in [
        "\"imported_by\"",
        "\"may_use\"",
        "\"from\"",
        "\"through\": \"src/index.ts\"",
    ] {
        assert!(json.contains(expected), "missing {expected} in:\n{json}");
    }
}

#[test]
fn a_rust_method_is_imported_through_its_type() {
    let total = query_text(&fixture_root(), &["Invoice::total"]);
    assert!(
        total.contains(
            "Imported by: 1\n  crates/app/src/config.rs:1 (via crates/lib_core/src/lib.rs:7)\n"
        ),
        "{total}"
    );
    let greet = query_text(&fixture_root(), &["greet"]);
    for expected in [
        "Imported by: 2\n",
        "\n  crates/app/src/main.rs:2\n",
        "\n  crates/app/src/config.rs:12 (local)\n",
    ] {
        assert!(
            greet.contains(expected),
            "missing `{expected}` in:\n{greet}"
        );
    }
    let receipt = query_text(&fixture_root(), &["receipt"]);
    assert!(
        receipt.contains("Imported by: none resolved\n"),
        "{receipt}"
    );
}

#[test]
fn a_python_symbol_is_imported_from_the_file_that_defines_it() {
    let user = query_text(&python_fixture(), &["User"]);
    assert!(user.contains("Imported by: 5\n"), "{user}");
    // `from shop.billing import pay` goes through the package's
    // `__init__.py`, which binds `pay` from charge.py
    let pay = query_text(&python_fixture(), &["pay"]);
    assert!(
        pay.contains(
            "Imported by: 3\n  scripts/backfill.py:1 (via src/shop/billing/__init__.py:1)\n  \
             src/shop/billing/__init__.py:1\n  \
             tests/test_billing.py:1 (via src/shop/billing/__init__.py:1) (test)\n"
        ),
        "{pay}"
    );
}

/// A TS package with two functions named `helper`, one of them imported.
fn two_helpers(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("archmap-cli-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    for (file, text) in [
        ("package.json", "{\"name\": \"two\"}\n"),
        (
            "src/a.ts",
            "export function helper(): number {\n  return 1;\n}\n",
        ),
        (
            "src/b.ts",
            "export function helper(): number {\n  return 2;\n}\n",
        ),
        (
            "src/c.ts",
            "import { helper } from './a';\nexport const x = helper();\n",
        ),
    ] {
        let path = dir.join(file);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }
    dir
}

#[test]
fn several_symbols_of_one_name_count_their_importers() {
    let dir = two_helpers("helpers");
    let text = candidates(&["query", "helper"], &dir);
    std::fs::remove_dir_all(&dir).unwrap();
    assert_eq!(
        text,
        "`helper` names 2 symbols; retry with one of them by id or path:\n  \
         two::src/a.ts::helper  src/a.ts:1  function  imported by 1, may use 0\n  \
         two::src/b.ts::helper  src/b.ts:1  function  imported by 0, may use 0\n"
    );
}

#[test]
fn impact_of_a_symbol_starts_at_the_statements_that_take_it() {
    let symbol = ts_stdout(&["impact", "formatPrice", "--format", "json"]);
    let file = ts_stdout(&["impact", "src/lib/money.ts", "--format", "json"]);
    // tests/helpers.ts takes another name from money.ts: the file reaches
    // it, the symbol does not
    let tests = |json: &str| -> serde_json::Value {
        test_files(&serde_json::from_str::<serde_json::Value>(json).unwrap()["tests"])
    };
    assert_eq!(
        tests(&file),
        serde_json::json!(["tests/helpers.ts", "tests/money.test.ts"])
    );
    assert_eq!(tests(&symbol), serde_json::json!(["tests/money.test.ts"]));
    for expected in [
        "\"symbol\": \"ts-shop::src/lib/money.ts::formatPrice\"",
        "\"ts-shop::src/app/page.tsx\"",
        "\"file\": \"src/app/page.tsx\"",
        "\"may_use\"",
    ] {
        assert!(
            symbol.contains(expected),
            "missing {expected} in:\n{symbol}"
        );
    }
}

#[test]
fn impact_of_a_name_several_symbols_share_lists_their_ids() {
    let dir = two_helpers("impact-helpers");
    let stdout = candidates(&["impact", "helper", "--format", "json"], &dir);
    std::fs::remove_dir_all(&dir).unwrap();
    let json: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(json["candidates"][0]["kind"], "symbol");
    assert_eq!(json["candidates"][0]["id"], "two::src/a.ts::helper");
    assert_eq!(json["candidates"][0]["imported_by"], 1);
    assert_eq!(json["candidates"][0]["symbol_kind"], "function");
    assert_eq!(json["candidates"][1]["id"], "two::src/b.ts::helper");
}

#[test]
fn a_rust_module_symbol_points_at_its_component() {
    // `pub mod invoice;` is a symbol of billing.rs, but imports name the
    // module's own file: the component answers for it
    let text = query_text(&fixture_root(), &["invoice"]);
    assert!(
        text.contains(
            "`invoice` is a module: `query lib_core::billing::invoice` lists what imports it\n"
        ),
        "{text}"
    );
    assert!(!text.contains("Imported by"), "{text}");
    let out = archmap()
        .args(["impact", "invoice", "--format", "json", "--path"])
        .arg(fixture_root())
        .output()
        .unwrap();
    let json = String::from_utf8_lossy(&out.stdout);
    assert!(
        json.contains("\"target\": \"lib_core::billing::invoice\""),
        "{json}"
    );
    assert!(!json.contains("\"symbol\""), "{json}");
}

#[test]
fn re_export_statements_are_marked_among_importers() {
    // a file query: `export { default as limitOf } from` beside an import
    let file = ts_stdout(&["query", "src/lib/limits.ts"]);
    assert!(file.contains("src/index.ts:3 (export)"), "{file}");
    assert!(!file.contains("src/index.ts:5 (export)"), "{file}");
    // a symbol query counts them apart
    let symbol = ts_stdout(&["query", "limitOf"]);
    for expected in [
        "Imported by: 4 (2 re-exports)\n",
        "\n  src/index.ts:3 (export)\n",
        "\n  src/app/checkout.ts:1 (via src/index.ts:3)\n",
    ] {
        assert!(
            symbol.contains(expected),
            "missing `{expected}` in:\n{symbol}"
        );
    }
}

#[test]
fn nothing_resolved_says_what_the_map_cannot_see() {
    let receipt = query_text(&fixture_root(), &["receipt"]);
    assert!(
        receipt.contains(
            "Imported by: none resolved\n  (only import statements are read; code that a framework or runtime loads by name or path is not seen)\n"
        ),
        "{receipt}"
    );
}

/// A TS package with two exports named `helper`, one under a directory
/// whose name the shell would expand, and a directory named `helper` that
/// holds no code.
fn helpers_beside_a_directory(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("archmap-cli-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    for (file, text) in [
        ("package.json", "{\"name\": \"two\"}\n"),
        ("src/(group)/a.ts", "export const helper = 1;\n"),
        ("src/b.ts", "export const helper = 2;\n"),
        // a directory without code: no component of that name, only a path
        ("helper/README.md", "notes\n"),
    ] {
        let path = dir.join(file);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }
    dir
}

#[test]
fn ids_that_the_shell_would_expand_are_quoted() {
    let dir = helpers_beside_a_directory("quoted");
    let query = candidates(&["query", "helper"], &dir);
    let impact = candidates(&["impact", "helper", "--format", "json"], &dir);
    // the quoted id, pasted back as it is shown, picks that symbol
    let picked = query_text(&dir, &["'two::src/(group)/a.ts::helper'"]);
    std::fs::remove_dir_all(&dir).unwrap();
    assert!(
        query.contains("\n  'two::src/(group)/a.ts::helper'  src/(group)/a.ts:1  constant\n"),
        "{query}"
    );
    // the directory named like the symbols is one more candidate, never the
    // answer for the package that owns it
    assert!(query.contains("\n  ./helper  directory\n"), "{query}");
    let impact: serde_json::Value = serde_json::from_str(&impact).unwrap();
    assert_eq!(impact["candidates"][2]["kind"], "directory");
    assert!(
        picked.starts_with("Symbols matching `two::src/(group)/a.ts::helper`: 1\n"),
        "{picked}"
    );
}

#[test]
fn imports_of_types_only_are_marked_in_query() {
    let text = ts_stdout(&["query", "src/lib/types.ts"]);
    for expected in [
        "src/app/page.tsx:2 (type)",
        // a re-export of a type only
        "src/index.ts:8 (export) (type)",
    ] {
        assert!(text.contains(expected), "missing `{expected}` in:\n{text}");
    }
    // a statement that takes a value from the same file is no type import
    let money = ts_stdout(&["query", "src/lib/money.ts"]);
    assert!(money.contains("src/app/page.tsx:1"), "{money}");
    assert!(!money.contains("src/app/page.tsx:1 (type)"), "{money}");
}

#[test]
fn rule_violations_say_when_they_import_types_only() {
    let rules = r#"
[components]
types = ["src/lib/types.ts"]
money = ["src/lib/money.ts"]
page = ["src/app/page.tsx"]

[[deny]]
from = "types"
to = "money"

[[deny]]
from = "page"
to = "money"
"#;
    let out = check_with("types-only", &ts_fixture(), rules, &[]);
    assert_eq!(out.status.code(), Some(1));
    let text = String::from_utf8_lossy(&out.stdout);
    for expected in [
        "forbidden by deny[0] types -> money: lib/types.ts -> lib/money.ts (import, types only)\n",
        // page.tsx:1 takes a value too
        "forbidden by deny[1] page -> money: app/page.tsx -> lib/money.ts (import)\n",
    ] {
        assert!(text.contains(expected), "missing `{expected}` in:\n{text}");
    }
}

#[test]
fn imports_written_as_calls_show_where_they_run() {
    let text = ts_stdout(&["query", "src/app/lazy.tsx"]);
    for expected in [
        // `lazy(() => import(..))` runs when the component first renders
        "src/app/lazy.tsx:3 -> src/components/button.tsx (local)",
        // `typeof import(..)` never runs
        "src/app/lazy.tsx:4 -> src/lib/limits.ts (type)",
        "import()  dynamic  1 call: src/app/lazy.tsx:7 (local)",
    ] {
        assert!(text.contains(expected), "missing `{expected}` in:\n{text}");
    }
    let text = ts_stdout(&["query", "scripts/report.cjs"]);
    for expected in [
        "scripts/report.cjs:1 -> scripts/format.cjs\n",
        "require  dynamic  1 call: scripts/report.cjs:4 (local)",
    ] {
        assert!(text.contains(expected), "missing `{expected}` in:\n{text}");
    }
}

#[test]
fn a_script_shows_its_kind_and_globals() {
    let text = ts_stdout(&["query", "src/global.d.ts"]);
    for expected in [
        "(script, typescript)",
        "declare const VERSION: string  src/global.d.ts:1",
        "interface Window  src/global.d.ts:3",
        // no import names a global, which reads as unused otherwise
        "Imported by: none (a script: its declarations are global, so what uses them is not traced)",
    ] {
        assert!(text.contains(expected), "missing `{expected}` in:\n{text}");
    }
    // at any depth, where the script folds into its package
    for depth in ["2", "0"] {
        let text = ts_stdout(&["query", "VERSION", "--depth", depth]);
        assert!(
            text.contains(
                "Imported by: none (a script declares it globally: what uses it is not traced)"
            ),
            "{text}"
        );
    }
}

#[test]
fn test_code_is_marked_and_listed_apart() {
    let text = ts_stdout(&["query", "src/lib/money.ts"]);
    for expected in [
        // production importers first; a test file's import is marked
        "tests/money.test.ts  1 import in tests: tests/money.test.ts:2 (test)",
        "tests/helpers.ts     1 import in tests: tests/helpers.ts:1 (test)",
    ] {
        assert!(text.contains(expected), "missing `{expected}` in:\n{text}");
    }
    let json = ts_stdout(&["impact", "src/lib/money.ts", "--format", "json"]);
    let impact: serde_json::Value = serde_json::from_str(&json).unwrap();
    // the tests to run again, apart from the code that depends on the file
    assert_eq!(
        test_files(&impact["tests"]),
        serde_json::json!(["tests/helpers.ts", "tests/money.test.ts"])
    );
    // importers: production code first, test code marked
    let impact = fixture_json(&["impact", "src/shop/users.py", "--format", "json"]);
    let sites: Vec<(String, bool)> = impact["importers"]["statements"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| {
            (
                s["file"].as_str().unwrap().to_owned(),
                s.get("test").is_some(),
            )
        })
        .collect();
    assert_eq!(
        sites,
        [
            ("scripts/2024-01-migration/fix.py".to_owned(), false),
            ("src/shop/__init__.py".to_owned(), false),
            ("src/shop/billing/charge.py".to_owned(), false),
            ("tests/test_billing.py".to_owned(), true),
            ("tests/unit/factories.py".to_owned(), true),
        ]
    );
}

#[test]
fn symbols_are_listed_in_source_order() {
    let text = ts_stdout(&["query", "src/lib/money.ts"]);
    let listed = [
        "Public symbols: 8",
        "  export const CURRENCY: string  src/lib/money.ts:6",
        "  export function formatPrice(price: Money): string  src/lib/money.ts:8",
        "  export class Wallet  src/lib/money.ts:12",
        "  Wallet.pay: pay(amount: number): void  src/lib/money.ts:13",
        "  Wallet.open: static open(): Wallet  src/lib/money.ts:15",
        "  export const schema = z.object(…)  src/lib/money.ts:20",
        "  const rates = {…}  src/lib/money.ts:21",
        "  export const read = () =>  src/lib/money.ts:23",
    ]
    .join("\n");
    assert!(text.contains(&listed), "{text}");
}

#[test]
fn a_relative_import_through_a_package_counts_as_an_absolute_one() {
    let repo = temp_repo("relative-through");
    for (file, text) in [
        ("shop/__init__.py", "from shop.billing.charge import pay\n"),
        ("shop/billing/__init__.py", ""),
        ("shop/billing/charge.py", "def pay():\n    pass\n"),
        ("app/__init__.py", "from shop import pay\n"),
        ("shop/orders/__init__.py", "from .. import pay\n"),
    ] {
        let path = repo.join(file);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }
    let summary = archmap()
        .arg("summary")
        .arg(&repo)
        .args(["-o", "-", "--verbose"])
        .output()
        .unwrap();
    let summary = String::from_utf8_lossy(&summary.stdout);
    // both statements count for the file that defines `pay`, not the package
    for expected in [
        "\napp -> shop.billing  imports: 1\n",
        "\nshop.orders -> shop.billing  imports: 1\n",
    ] {
        assert!(summary.contains(expected), "{summary}");
    }
    assert!(!summary.contains("shop.orders -> shop  "), "{summary}");
    let query = query_text(&repo, &["shop.orders"]);
    std::fs::remove_dir_all(&repo).unwrap();
    assert!(
        query.contains(
            "  shop          1 through re-exports: shop/orders/__init__.py:1 -> shop/__init__.py (through)\n"
        ),
        "{query}"
    );
}

#[test]
fn a_statement_counts_for_each_file_it_loads_apart() {
    let repo = temp_repo("through-apart");
    for (file, text) in [
        ("shop/__init__.py", "from .core.charge import pay\n"),
        ("shop/core/__init__.py", ""),
        ("shop/core/charge.py", "def pay():\n    pass\n"),
        ("shop/sub/__init__.py", ""),
        ("app/__init__.py", "from shop import pay, sub\n"),
    ] {
        let path = repo.join(file);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }
    let summary = archmap()
        .arg("summary")
        .arg(&repo)
        .args(["-o", "-", "--verbose"])
        .output()
        .unwrap();
    std::fs::remove_dir_all(&repo).unwrap();
    let summary = String::from_utf8_lossy(&summary.stdout);
    // `pay` goes through `shop/__init__.py` to shop.core, whatever the
    // statement takes from the other file it loads
    for expected in [
        "\napp -> shop.core  imports: 1\n",
        "\napp -> shop.sub  imports: 1\n",
    ] {
        assert!(summary.contains(expected), "{summary}");
    }
    assert!(!summary.contains("app -> shop  "), "{summary}");
}

#[test]
fn summary_and_query_count_a_pair_alike() {
    // checkout.ts imports from the package's barrel five times; two
    // statements only reach names it re-exports, while line 1 also takes a
    // name found nowhere, which counts for the barrel
    let summary = archmap()
        .arg("summary")
        .arg(ts_fixture())
        .args(["-o", "-", "--verbose"])
        .output()
        .unwrap();
    let summary = String::from_utf8_lossy(&summary.stdout);
    assert!(
        summary.contains("\napp/checkout.ts -> ts-shop  imports: 3\n"),
        "{summary}"
    );
    let query = ts_stdout(&["query", "src/app/checkout.ts"]);
    assert!(
        query.contains("ts-shop                3 imports, 2 through re-exports: "),
        "{query}"
    );
    // a package's entry file into its own submodules: left out of the count,
    // which next.config.ts, another file of the package, is not
    let query = ts_stdout(&["query", "ts-shop", "--depth", "1"]);
    assert!(
        query.contains("1 import, 6 of its entry file: next.config.ts:1 -> src/lib/limits.ts, "),
        "{query}"
    );
}

#[test]
fn summary_and_query_name_a_shared_name_alike() {
    // the project and its package are both named `shop`: both commands
    // list the package by its id, so a name copied from either is one
    // that query takes
    let summary = summary_stdout(&[]);
    assert!(
        summary.contains("\ntests -> shop::shop  tests: 1\n"),
        "{summary}"
    );
    let query = query_text(&python_fixture(), &["tests"]);
    assert!(
        query.contains("\n  shop::shop    1 import in tests: tests/test_billing.py:2 "),
        "{query}"
    );
    let query = query_text(&python_fixture(), &["shop"]);
    assert!(
        query.contains("\n  scripts, shop::shop, tests\n"),
        "{query}"
    );
}

#[test]
fn impact_prints_text_unless_asked_for_json() {
    let text = ts_stdout(&["impact", "src/lib/types.ts"]);
    assert!(
        text.starts_with(
            "src/lib/types.ts (file) in lib/types.ts (module, typescript), depth 2\n\
             id: ts-shop::src/lib/types.ts\n\nDirect dependents: 4\n"
        ),
        "{text}"
    );
    assert_eq!(
        ts_stdout(&["impact", "src/lib/types.ts", "--format", "text"]),
        text
    );
    let json = ts_stdout(&["impact", "src/lib/types.ts", "--format", "json"]);
    let value: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(value["target"], "ts-shop::src/lib/types.ts");
    assert_eq!(value["importers"]["total"], 6);
}

#[test]
fn impact_lists_every_importer_and_test_with_verbose() {
    let capped = ts_stdout(&["impact", "src/lib/money.ts", "--format", "json"]);
    let full = ts_stdout(&[
        "impact",
        "src/lib/money.ts",
        "--format",
        "json",
        "--verbose",
    ]);
    let shown = |json: &str| -> (usize, usize) {
        let value: serde_json::Value = serde_json::from_str(json).unwrap();
        (
            value["importers"]["statements"].as_array().unwrap().len(),
            value["importers"]["total"].as_u64().unwrap() as usize,
        )
    };
    // JSON lists every statement, verbose or not; verbose lifts the text's caps
    let (listed, total) = shown(&capped);
    assert!(total > 5, "{capped}");
    assert_eq!(listed, total);
    assert_eq!(capped, full);
}

#[test]
fn a_path_target_is_read_as_the_root_sees_it() {
    let root = python_fixture().canonicalize().unwrap();
    let absolute = root.join("src/shop/users.py");
    let expected = fixture_json(&["impact", "src/shop/users.py", "--format", "json"]);
    for target in [
        absolute.to_str().unwrap(),
        "src/shop/../shop/./users.py",
        // the name query also takes for the file
        "shop.users",
    ] {
        let found = fixture_json(&["impact", target, "--format", "json"]);
        assert_eq!(found["target"], expected["target"], "{target}");
        assert_eq!(found["importers"], expected["importers"], "{target}");
    }
    let query = fixture_json(&["query", absolute.to_str().unwrap(), "--format", "json"]);
    assert_eq!(query["file"], "src/shop/users.py");
    // a path outside the root names nothing in it
    let outside = root
        .parent()
        .unwrap()
        .join("simple-ts-project/package.json");
    for command in ["query", "impact"] {
        let out = archmap()
            .args([command, outside.to_str().unwrap(), "--path"])
            .arg(&root)
            .output()
            .unwrap();
        assert!(!out.status.success(), "{command}");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(stderr.contains("is outside the scanned root"), "{stderr}");
    }
}

#[test]
fn a_symbol_query_lists_production_importers_first() {
    // in edge order, the test file comes first
    let repo = temp_repo("symbol-order");
    std::fs::create_dir_all(repo.join("web/src/__tests__")).unwrap();
    std::fs::write(repo.join("web/package.json"), "{ \"name\": \"web\" }").unwrap();
    std::fs::write(repo.join("web/src/f.ts"), "export function shout() {}\n").unwrap();
    std::fs::write(
        repo.join("web/src/__tests__/f.test.ts"),
        "import { shout } from '../f';\n",
    )
    .unwrap();
    std::fs::write(repo.join("web/src/b.ts"), "import { shout } from './f';\n").unwrap();
    let text = query_text(&repo, &["shout"]);
    std::fs::remove_dir_all(&repo).unwrap();
    assert!(
        text.contains(
            "\nImported by: 2\n  web/src/b.ts:1\n  web/src/__tests__/f.test.ts:1 (test)\n"
        ),
        "{text}"
    );
}

#[test]
fn a_file_query_names_the_components_that_share_its_name() {
    let repo = temp_repo("file-namesakes");
    for package in ["a", "b"] {
        std::fs::create_dir_all(repo.join(format!("{package}/src"))).unwrap();
        std::fs::write(
            repo.join(format!("{package}/package.json")),
            format!("{{ \"name\": \"{package}\" }}"),
        )
        .unwrap();
        std::fs::write(
            repo.join(format!("{package}/src/types.ts")),
            "export type T = 1;\n",
        )
        .unwrap();
    }
    let text = query_text(&repo, &["a/src/types.ts"]);
    let json = query_text(&repo, &["a::src/types.ts", "--format", "json"]);
    std::fs::remove_dir_all(&repo).unwrap();
    assert!(text.contains("\nalso named: b::src/types.ts\n"), "{text}");
    let json: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(json["also_named"], serde_json::json!(["b::src/types.ts"]));
}

#[test]
fn the_cli_prints_what_the_shared_layer_answers() {
    use archmap_app::{
        load_rules, CheckRequest, Format, ImpactRequest, QueryRequest, ScanMode, Workspace,
        DEFAULT_DEPTH,
    };
    keep_own_history_out();
    let root = python_fixture();
    let ws = Workspace::scan(&root, ScanMode::Full).unwrap();
    let query = |target, format| {
        ws.query(&QueryRequest {
            target,
            depth: DEFAULT_DEPTH,
            format,
            verbose: false,
        })
        .unwrap()
        .output
    };
    let impact = |target, format| {
        ws.impact(&ImpactRequest {
            target,
            depth: DEFAULT_DEPTH,
            format,
            verbose: false,
        })
        .unwrap()
        .output
    };
    let rules = load_rules(&root, None).unwrap();
    let check = ws
        .check(
            &rules,
            &CheckRequest {
                depth: None,
                format: Format::Text,
            },
        )
        .unwrap()
        .output;
    let root_arg = root.to_str().unwrap();
    let cases: Vec<(Vec<&str>, String)> = vec![
        (vec!["summary", root_arg], ws.summary(DEFAULT_DEPTH, false)),
        (
            vec!["query", "shop.users", "--path", root_arg],
            query("shop.users", Format::Text),
        ),
        (
            vec!["query", "shop", "--format", "json", "--path", root_arg],
            query("shop", Format::Json),
        ),
        (
            vec!["impact", "src/shop/users.py", "--path", root_arg],
            impact("src/shop/users.py", Format::Text),
        ),
        (
            vec!["impact", "shop", "--format", "json", "--path", root_arg],
            impact("shop", Format::Json),
        ),
        (vec!["check", "--path", root_arg], check),
    ];
    for (args, expected) in cases {
        let out = archmap().args(&args).output().unwrap();
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            expected,
            "archmap {}",
            args.join(" ")
        );
    }
}

#[test]
fn query_and_impact_exit_0_for_an_answer_1_for_candidates_and_2_for_errors() {
    let repo = two_projects_with_tests("exit-codes");
    let code = |args: &[&str]| {
        archmap()
            .args(args)
            .arg("--path")
            .arg(&repo)
            .output()
            .unwrap()
            .status
            .code()
    };
    for command in ["query", "impact"] {
        assert_eq!(code(&[command, "a::tests"]), Some(0), "{command}");
        assert_eq!(code(&[command, "tests"]), Some(1), "{command}");
        assert_eq!(
            code(&[command, "nothing-is-named-so"]),
            Some(2),
            "{command}"
        );
    }
    std::fs::remove_dir_all(&repo).unwrap();
    let missing = archmap()
        .args(["summary", "no-such-root"])
        .output()
        .unwrap();
    assert_eq!(missing.status.code(), Some(2));
}

/// Speak MCP to `archmap mcp --path <root>` over its stdin and stdout: the
/// responses to `requests` (each with an id), by id, after asserting that
/// every line the server wrote is JSON-RPC.
fn mcp_session(root: &Path, requests: &[serde_json::Value]) -> Vec<serde_json::Value> {
    use std::io::{BufRead, BufReader, Write};
    let mut child = archmap()
        .args(["mcp", "--path"])
        .arg(root)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
    let mut send = |message: serde_json::Value| {
        writeln!(stdin, "{message}").unwrap();
        stdin.flush().unwrap();
    };
    send(serde_json::json!({
        "jsonrpc": "2.0", "id": 0, "method": "initialize",
        "params": {
            "protocolVersion": "2025-06-18",
            "capabilities": {},
            "clientInfo": {"name": "archmap-cli-test", "version": "0"}
        }
    }));
    let mut read = || {
        let line = lines.next().expect("the server closed stdout").unwrap();
        serde_json::from_str::<serde_json::Value>(&line)
            .unwrap_or_else(|e| panic!("not JSON-RPC on stdout: {line:?}: {e}"))
    };
    let initialized = read();
    assert_eq!(initialized["id"], 0, "{initialized}");
    send(serde_json::json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));
    let mut answers = Vec::new();
    for request in requests {
        send(request.clone());
        let answer = read();
        assert_eq!(answer["id"], request["id"], "{answer}");
        answers.push(answer);
    }
    drop(stdin);
    let status = child.wait().unwrap();
    assert!(status.success(), "the server exits when stdin closes");
    answers
}

#[test]
fn archmap_mcp_answers_as_the_cli_does_and_writes_only_json_rpc() {
    let root = python_fixture();
    let call = |id: u32, tool: &str, args: serde_json::Value| {
        serde_json::json!({
            "jsonrpc": "2.0", "id": id, "method": "tools/call",
            "params": {"name": tool, "arguments": args}
        })
    };
    let answers = mcp_session(
        &root,
        &[
            serde_json::json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}),
            call(2, "query", serde_json::json!({"target": "shop.users"})),
            call(
                3,
                "impact",
                serde_json::json!({"target": "src/shop/users.py"}),
            ),
            call(4, "summary", serde_json::json!({})),
        ],
    );
    let tools: Vec<&str> = answers[0]["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert_eq!(tools, ["check", "impact", "query", "summary"]);
    let text = |answer: &serde_json::Value| {
        assert_eq!(answer["result"]["isError"], false, "{answer}");
        answer["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .to_owned()
    };
    let cli = |args: &[&str]| {
        let out = archmap()
            .args(args)
            .arg("--path")
            .arg(&root)
            .output()
            .unwrap();
        String::from_utf8(out.stdout).unwrap()
    };
    assert_eq!(text(&answers[1]), cli(&["query", "shop.users"]));
    assert_eq!(text(&answers[2]), cli(&["impact", "src/shop/users.py"]));
    let summary = archmap().arg("summary").arg(&root).output().unwrap();
    assert_eq!(
        text(&answers[3]),
        String::from_utf8(summary.stdout).unwrap()
    );
}

#[test]
fn archmap_mcp_refuses_a_root_that_is_not_there() {
    let out = archmap()
        .args(["mcp", "--path", "no-such-root"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("no-such-root"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn coverage_counts_an_import_under_the_language_of_its_file() {
    // a JavaScript config file that a TypeScript package owns directly
    let repo = temp_repo("coverage-language");
    std::fs::create_dir_all(repo.join("web/src")).unwrap();
    std::fs::write(repo.join("web/package.json"), "{ \"name\": \"web\" }").unwrap();
    std::fs::write(repo.join("web/src/a.ts"), "export const a = 1;\n").unwrap();
    std::fs::write(
        repo.join("web/eslint.config.js"),
        "import pad from 'left-pad';\nexport default [pad];\n",
    )
    .unwrap();
    let out = archmap()
        .arg("summary")
        .arg(&repo)
        .args(["-o", "-"])
        .output()
        .unwrap();
    std::fs::remove_dir_all(&repo).unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.contains(
            "\njavascript  files: 1  read: 1  imports without an edge: 1 (undeclared 1)\n"
        ),
        "{text}"
    );
    assert!(
        text.contains("\ntypescript  files: 1  read: 1  imports without an edge: 0\n"),
        "{text}"
    );
}

#[test]
fn only_a_walked_note_reads_as_via() {
    // specifiers that contain ` via `, beside one import through a re-export
    let repo = temp_repo("via-notes");
    std::fs::create_dir_all(repo.join("web/src")).unwrap();
    std::fs::write(
        repo.join("web/package.json"),
        "{ \"name\": \"web\", \"dependencies\": { \"pkg\": \"1\" } }",
    )
    .unwrap();
    std::fs::write(
        repo.join("web/src/m.ts"),
        "import x from './a via b';\nimport y from 'pkg/c via d';\nimport { z } from './z';\n\
         export const all = [x, y, z];\n",
    )
    .unwrap();
    std::fs::write(repo.join("web/src/z.ts"), "export { z } from './zz';\n").unwrap();
    std::fs::write(repo.join("web/src/zz.ts"), "export const z = 1;\n").unwrap();
    let text = query_text(&repo, &["web/src/m.ts"]);
    std::fs::remove_dir_all(&repo).unwrap();
    for expected in [
        "1 import: web/src/m.ts:2\n",
        "1 import: web/src/m.ts:3 -> web/src/zz.ts (via web/src/z.ts:1)\n",
        "unresolved  1 import: web/src/m.ts:1\n",
    ] {
        assert!(text.contains(expected), "missing `{expected}` in:\n{text}");
    }
}

#[test]
fn a_statement_counts_for_a_barrel_unless_every_name_passes_through() {
    let repo = temp_repo("barrel-own");
    std::fs::create_dir_all(repo.join("web/src/ui")).unwrap();
    std::fs::write(
        repo.join("web/package.json"),
        "{ \"name\": \"web\", \"dependencies\": { \"@radix-ui/react-dialog\": \"1\" } }",
    )
    .unwrap();
    std::fs::write(
        repo.join("web/src/ui/index.ts"),
        "export { Widget } from './widget';\nexport * from '@radix-ui/react-dialog';\n\
         export default { theme: 'dark' };\n",
    )
    .unwrap();
    std::fs::write(
        repo.join("web/src/ui/widget.ts"),
        "export const Widget = 1;\n",
    )
    .unwrap();
    std::fs::write(
        repo.join("web/src/app.ts"),
        // the barrel's own default; a name from outside the scan; only a
        // re-exported name
        "import cfg, { Widget } from './ui';\nimport { Dialog } from './ui';\n\
         import { Widget as W } from './ui';\nexport const all = [cfg, Widget, Dialog, W];\n",
    )
    .unwrap();
    let out = archmap()
        .arg("summary")
        .arg(&repo)
        .args(["-o", "-"])
        .output()
        .unwrap();
    let query = query_text(&repo, &["web/src/app.ts"]);
    std::fs::remove_dir_all(&repo).unwrap();
    let summary = String::from_utf8_lossy(&out.stdout);
    assert!(
        summary.contains("\napp.ts -> ui  imports: 2\n"),
        "{summary}"
    );
    assert!(
        query.contains("\n  ui            2 imports, 1 through re-exports: "),
        "{query}"
    );
}

#[test]
fn two_checkouts_of_one_commit_give_one_graph() {
    // a worktree takes another directory name; the manifest names the
    // project, as code that no manifest names takes the directory's name
    let graphs: Vec<String> = ["checkout-main", "checkout-feature"]
        .iter()
        .map(|name| {
            let repo = temp_repo(name);
            std::fs::write(repo.join("pyproject.toml"), "[project]\nname = \"shop\"\n").unwrap();
            let out = archmap()
                .arg("scan")
                .arg(&repo)
                .args(["-o", "-"])
                .output()
                .unwrap();
            let summary = archmap()
                .arg("summary")
                .arg(&repo)
                .args(["-o", "-"])
                .output()
                .unwrap();
            std::fs::remove_dir_all(&repo).unwrap();
            // the summary still names the directory it maps
            let summary = String::from_utf8_lossy(&summary.stdout).into_owned();
            let dir = repo.file_name().unwrap().to_string_lossy().into_owned();
            assert!(summary.contains(&format!("\nroot: {dir}\n")), "{summary}");
            String::from_utf8(out.stdout).unwrap()
        })
        .collect();
    assert_eq!(graphs[0], graphs[1]);
}

#[test]
fn impact_reaches_an_importer_through_the_barrel_alone() {
    // checkout.ts imports Button from the package's barrel, never from
    // components/button.tsx itself
    let json = ts_stdout(&["impact", "src/components/button.tsx", "--format", "json"]);
    let value: serde_json::Value = serde_json::from_str(&json).unwrap();
    let direct: Vec<&str> = value["direct"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|d| d["id"].as_str())
        .collect();
    assert!(direct.contains(&"ts-shop::src/app/checkout.ts"), "{json}");
}

#[test]
fn query_on_a_ts_symbol_lists_where_it_is_used() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/ts-uses");
    let text = query_text(&root, &["formatPrice"]);
    // two uses that would read alike say their columns
    let used_at = "\nUsed at: 21 in 14 files, showing 10 (20 calls, 1 type)\n\
         \x20 src/app.ts:11 (call) as fp, src/app.ts:11:20 (call) as m.formatPrice, \
         src/app.ts:11:38 (call) as m.formatPrice, +1 more in this file\n\
         \x20 src/view.tsx:9 (call) as money.formatPrice, src/view.tsx:10 (call), \
         src/view.tsx:11 (call) as all.money.formatPrice\n\
         \x20 scripts/cjs.cjs:4 (call), scripts/cjs.cjs:4 (call) as money.formatPrice\n\
         \x20 src/cast.ts:3 (call) as m.formatPrice, src/cast.ts:4 (call) as m.formatPrice\n\
         \x20 scripts/lazy.mjs:3 (call)\n\
         \x20 src/aliased.ts:3 (call) as all.fp3\n\
         \x20 src/hoisted.ts:1 (call)\n\
         \x20 src/money.ts:10 (call)\n\
         \x20 src/relayed.ts:3 (call) as fmt2\n\
         \x20 src/types.ts:4 (type)\n\
         \x20 never used (1 import): src/unused.ts:1\n\
         \x20 never named (1 import of the whole module): tests/actual.test.ts:2\n\
         \x20 mocked (2 places): tests/mocked.test.ts:4 (test), tests/partial.test.ts:5 (test)\n";
    assert!(text.contains(used_at), "{text}");
    for line in [
        "  whole module: 3 places use the module as a value, which may use this: \
         scripts/lazy.mjs:7, src/app.ts:8, tests/partial.test.ts:4 (test)\n",
        "  uses: not read in 1 place: src/two.ts:1 (ambiguous statement)\n",
    ] {
        assert!(text.contains(line), "{line}\n{text}");
    }
    // a value read, and the imports of the module whole that never name it
    let rates = query_text(&root, &["RATES"]);
    assert!(
        rates.contains(
            "\nUsed at: 1 in 1 file (1 read)\n  src/money.ts:29 (read) as rates\n  \
             never named (6 imports of the whole module): scripts/cjs.cjs:2, src/aliased.ts:1, \
             src/cast.ts:1, +3 more\n"
        ),
        "{rates}"
    );
    // a method that is not static: only what its class and `this` show
    let method = query_text(&root, &["Wallet.pay"]);
    assert!(
        method.contains(
            "\nUsed at: through the class and this only: 2 in 1 file (2 calls)\n\
             \x20 src/money.ts:13 (call) as this.pay, src/money.ts:17 (call) as this.pay\n"
        ),
        "{method}"
    );
    assert!(
        method.contains(
            "  values: calls through a value of the type (x.m()) need its type, which is not \
             read; 9 imports of the type or its module may make them: scripts/cjs.cjs:2, \
             src/aliased.ts:1, src/app.ts:1, +6 more\n"
        ),
        "{method}"
    );
    assert!(!method.contains("never used"), "{method}");
    assert!(
        method.contains("  mocked (1 place): tests/mocked.test.ts:5 (test) as Wallet\n"),
        "{method}"
    );
    assert!(
        method.contains(
            "  subclasses: calls through a subclass (Sub.m(), super.m()) are not read; \
             extended at src/rich.ts:3\n"
        ),
        "{method}"
    );
    // JSON lists every use with its column, role and statement
    let out = archmap()
        .args(["query", "formatPrice", "--format", "json", "--path"])
        .arg(&root)
        .output()
        .unwrap();
    let value: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let uses = value[0]["used_at"]["uses"].as_array().unwrap();
    assert_eq!(uses.len(), 21);
    assert_eq!(
        uses[0],
        serde_json::json!({
            "evidence": {"file": "scripts/cjs.cjs", "line": 4},
            "column": 23,
            "role": "call",
            "statement": {"file": "scripts/cjs.cjs", "line": 1}
        })
    );
    assert_eq!(
        value[0]["used_at"]["mocked"][0],
        serde_json::json!({"file": "tests/mocked.test.ts", "line": 4, "test": true})
    );
    // an import a package's `__init__.py` never uses offers the name to
    // whoever imports the package: no negative fact
    let python = query_text(&python_fixture(), &["notify"]);
    assert!(python.contains("\nUsed at: none found\n"), "{python}");
    assert!(!python.contains("never used"), "{python}");
}

#[test]
fn impact_on_a_symbol_starts_from_every_name_barrels_give_it() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/ts-renames");
    let out = archmap()
        .args(["impact", "formatPrice", "--verbose", "--path"])
        .arg(&root)
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    for line in [
        "  src/alias.ts:1 (via src/money.ts:5)\n",
        "  src/back.ts:1 (export) (from src/chain.ts, which passes it on)\n",
        "  src/chained.ts:1 (via src/chain.ts:1)\n",
        "  src/rename.ts:1 (via src/index.ts:1)\n",
        "  src/namespace.ts:1 (whole src/chain.ts, which passes it on)\n",
        "  src/twonames.ts:1 (from src/both.ts, which passes it on)\n",
    ] {
        assert!(text.contains(line), "{line}\n{text}");
    }
    // no barrel left unfollowed: nothing said of renames
    assert!(!text.contains("renamed"), "{text}");
}
