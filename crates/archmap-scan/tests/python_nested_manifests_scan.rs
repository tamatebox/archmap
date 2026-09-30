//! End-to-end scan of `fixtures/nested-manifests-project`: which files each
//! manifest declares dependencies for.

use std::path::Path;

use archmap_core::{ArchitectureGraph, EdgeKind, UnmappedReason};
use archmap_scan::{scan, ScanOptions};

fn scan_fixture() -> ArchitectureGraph {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/nested-manifests-project")
        .canonicalize()
        .expect("fixture exists");
    let report = scan(&root, &ScanOptions::default()).expect("scan succeeds");
    assert!(
        report.warnings.is_empty(),
        "unexpected warnings: {:?}",
        report.warnings
    );
    report.graph
}

/// Components with a `kind` edge to `to`, each with its evidence files.
fn sources<'a>(
    graph: &'a ArchitectureGraph,
    kind: EdgeKind,
    to: &str,
) -> Vec<(&'a str, Vec<&'a str>)> {
    graph
        .edges
        .iter()
        .filter(|e| e.kind == kind && e.to.as_str() == to)
        .map(|e| {
            let files = e.evidence.iter().map(|ev| ev.file.as_str()).collect();
            (e.from.as_str(), files)
        })
        .collect()
}

#[test]
fn a_nested_requirements_file_declares_for_its_own_directory() {
    let graph = scan_fixture();
    // the declaration belongs to the directory it sits in, and code there
    // resolves against it
    assert_eq!(
        sources(&graph, EdgeKind::Dependency, "ext:slack-sdk"),
        vec![(
            "jobs::functions.notify",
            vec!["functions/notify/requirements.txt"]
        )]
    );
    assert_eq!(
        sources(&graph, EdgeKind::Import, "ext:slack-sdk"),
        vec![("jobs::functions.notify", vec!["functions/notify/main.py"])]
    );
    // code outside that directory does not: its import is undeclared, and
    // the note says what matched and where the declaration applies
    let unmapped: Vec<(&str, &str, UnmappedReason, Option<&str>)> = graph
        .unmapped_imports
        .iter()
        .map(|u| {
            (
                u.from.as_str(),
                u.module.as_str(),
                u.reason,
                u.evidence.note.as_deref(),
            )
        })
        .collect();
    assert_eq!(
        unmapped,
        vec![
            (
                "jobs::jobs",
                "dateutil",
                UnmappedReason::Undeclared,
                Some(
                    "import dateutil, declared as python-dateutil \
                     (matched by known import name) only for functions/notify/ \
                     in functions/notify/requirements.txt:3"
                )
            ),
            (
                "jobs::jobs",
                "slack_sdk",
                UnmappedReason::Undeclared,
                Some(
                    "import slack_sdk, declared as slack-sdk only for functions/notify/ \
                     in functions/notify/requirements.txt:2"
                )
            ),
        ]
    );
}

#[test]
fn requirements_without_python_code_nearby_declare_for_the_project() {
    let graph = scan_fixture();
    // requirements/prod.txt, docker/requirements.txt and libs/requirements.txt
    // sit where none of the project's own Python code lives (libs/ holds
    // only the `tool` project), so they declare for the whole project like
    // the root file
    for (external, file) in [
        ("ext:pandas", "requirements.txt"),
        ("ext:rich", "requirements/prod.txt"),
        ("ext:pyyaml", "docker/requirements.txt"),
        ("ext:click", "libs/requirements.txt"),
    ] {
        assert_eq!(
            sources(&graph, EdgeKind::Dependency, external),
            vec![("jobs", vec![file])],
            "{external}"
        );
    }
    assert_eq!(
        sources(&graph, EdgeKind::Import, "ext:rich"),
        vec![("jobs::jobs", vec!["jobs/report.py"])]
    );
    assert_eq!(
        sources(&graph, EdgeKind::Import, "ext:pyyaml"),
        vec![("jobs::jobs", vec!["jobs/report.py"])]
    );
    assert_eq!(
        sources(&graph, EdgeKind::Import, "ext:click"),
        vec![("jobs::jobs", vec!["jobs/report.py"])]
    );
    // a nested directory still sees what its ancestors declare
    for external in ["ext:pandas", "ext:requests"] {
        assert_eq!(
            sources(&graph, EdgeKind::Import, external),
            vec![
                ("jobs::functions.notify", vec!["functions/notify/main.py"]),
                ("jobs::jobs", vec!["jobs/report.py"]),
            ],
            "{external}"
        );
    }
}
