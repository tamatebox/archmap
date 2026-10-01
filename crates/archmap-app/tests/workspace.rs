//! The shared layer seen from an interface: scan a repository once, then ask.

use std::path::{Path, PathBuf};

use archmap_app::{
    load_rules, CheckRequest, Format, ImpactRequest, QueryRequest, ScanMode, Workspace,
    DEFAULT_DEPTH,
};

fn python_fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/simple-python-project")
}

#[test]
fn a_workspace_summarizes_the_repository_it_scanned() {
    let ws = Workspace::scan(&python_fixture(), ScanMode::Full).unwrap();
    let summary = ws.summary(DEFAULT_DEPTH, false);
    assert!(
        summary.starts_with("# archmap summary\nroot: simple-python-project\ndepth: 2\n"),
        "{summary}"
    );
    assert_eq!(ws.root(), python_fixture().as_path());
}

#[test]
fn a_manifests_only_scan_reads_no_source() {
    let ws = Workspace::scan(&python_fixture(), ScanMode::ManifestsOnly).unwrap();
    assert!(ws.graph().symbols.is_empty());
    assert!(ws.warnings().is_empty());
}

#[test]
fn a_missing_root_names_it_in_the_error() {
    let missing = python_fixture().join("no-such-dir");
    let err = Workspace::scan(&missing, ScanMode::Full).err().unwrap();
    assert!(format!("{err:#}").starts_with("scanning "), "{err:#}");
}

fn scanned() -> Workspace {
    Workspace::scan(&python_fixture(), ScanMode::Full).unwrap()
}

#[test]
fn query_answers_a_file_by_its_dotted_name() {
    let text = scanned()
        .query(&QueryRequest {
            target: "shop.users",
            depth: DEFAULT_DEPTH,
            format: Format::Text,
            verbose: false,
        })
        .unwrap()
        .output;
    assert!(
        text.starts_with("src/shop/users.py (file) in shop (module, python), depth 2\n"),
        "{text}"
    );
}

#[test]
fn impact_answers_in_json_for_the_component_that_holds_a_file() {
    let json = scanned()
        .impact(&ImpactRequest {
            target: "src/shop/users.py",
            depth: DEFAULT_DEPTH,
            verbose: false,
        })
        .unwrap()
        .output;
    let value: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(value["target"], "shop::shop", "{json}");
    assert!(json.ends_with("}\n"), "{json}");
}

#[test]
fn check_without_a_rules_file_reports_signals_only() {
    let ws = scanned();
    let rules = load_rules(ws.root(), None).unwrap();
    assert_eq!(rules.label(), None);
    let answer = ws
        .check(
            &rules,
            &CheckRequest {
                depth: None,
                format: Format::Text,
            },
        )
        .unwrap();
    assert_eq!(answer.findings, 0);
    assert!(
        answer.output.starts_with("archmap check: no findings"),
        "{}",
        answer.output
    );
}
