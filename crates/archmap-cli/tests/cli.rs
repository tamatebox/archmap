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
    assert_eq!(graph["schema_version"], 1);
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
    assert_eq!(graph["schema_version"], 1);
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
        .args(["query", "lib_core", "--path"])
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
fn check_is_a_stub_with_distinct_exit_code() {
    let out = archmap().arg("check").output().unwrap();
    assert_eq!(out.status.code(), Some(2));
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

    assert!(first.starts_with("# Architecture summary: simple-python-project\n"));
    for expected in [
        "## Components",
        "  - shop: `src/shop`, 3 public symbols",
        "    - shop.billing: `src/shop/billing`, 4 public symbols",
        "## Internal dependencies",
        "- shop.billing -> shop (2)",
        "## External dependencies",
        "- requests: declared in `pyproject.toml`, `requirements.txt`; \
         imported by 1 component: shop.billing (1)",
        "- pyyaml: declared in `requirements.txt`; imported by 1 component: shop.billing (1)",
        "## Most depended-on",
    ] {
        assert!(
            first.contains(expected),
            "missing `{expected}` in:\n{first}"
        );
    }
    // machine-specific paths never leak into the summary
    let root = python_fixture().canonicalize().unwrap();
    assert!(!first.contains(&root.display().to_string()));
}

#[test]
fn summary_depth_controls_the_roll_up() {
    let shallow = summary_stdout(&["--depth", "1"]);
    assert!(
        !shallow.contains("shop.billing:"),
        "billing is folded:\n{shallow}"
    );
    assert!(shallow.contains("  - shop: `src/shop`, 9 public symbols, 3 submodules folded"));
    assert!(shallow.contains("- tests -> shop (3)"));

    let packages_only = summary_stdout(&["--depth", "0"]);
    assert!(packages_only
        .contains("- shop: python package, `.`, 9 public symbols, 7 submodules folded"));
    assert!(packages_only.contains("No internal dependencies."));
}

#[test]
fn summary_writes_under_dot_archmap_by_default() {
    let repo = temp_repo("summary");
    let out = archmap().arg("summary").arg(&repo).output().unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(out.stdout.is_empty());
    let written = std::fs::read_to_string(repo.join(".archmap/summary.md")).unwrap();
    assert!(written.starts_with("# Architecture summary: "));
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
    let view = fixture_json(&["query", "shop.integrations.slack"]);
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

    let deeper = fixture_json(&["query", "shop.integrations.slack", "--depth", "3"]);
    assert_eq!(deeper["component"]["id"], "shop::shop.integrations.slack");
    assert!(deeper.get("folded_from").is_none());
}

#[test]
fn impact_of_a_file_uses_the_summary_depth() {
    let result = fixture_json(&["impact", "src/shop/integrations/slack/__init__.py"]);
    assert_eq!(result["target"], "shop::shop.integrations");
    assert_eq!(result["folded_from"], "shop::shop.integrations.slack");
    assert_eq!(result["direct"], serde_json::json!(["shop::shop"]));
    assert_eq!(
        result["transitive"],
        serde_json::json!([
            "shop::scripts",
            "shop::shop",
            "shop::shop.billing",
            "shop::tests",
            "shop::tests.unit"
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
        .filter_map(|l| l.trim_start().strip_prefix("- "))
        .filter_map(|l| l.split(": ").next())
        .collect();
    assert!(names.contains("shop.billing") && names.contains("tests.unit"));

    for name in names {
        let view = fixture_json(&["query", name]);
        assert!(view.get("folded_from").is_none(), "`{name}` is folded");
        assert_eq!(view["component"]["name"], name);

        let impact = fixture_json(&["impact", name]);
        assert!(impact.get("folded_from").is_none());
        for id in impact["transitive"].as_array().unwrap() {
            let dependent = fixture_json(&["query", id.as_str().unwrap()]);
            assert!(
                dependent.get("folded_from").is_none(),
                "impact of `{name}` names `{id}`, which the summary does not show"
            );
        }
    }
}
